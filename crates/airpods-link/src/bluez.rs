//! BlueZ transport (Linux only): classic L2CAP SEQPACKET sockets via the
//! `bluer` crate, ported from `linux-rust/src/bluetooth/{aacp,att,discovery}.rs`
//! on the upstream `linux/rust` branch.

use std::sync::Arc;
use std::time::Duration;

use bluer::l2cap::{Socket, SocketAddr};
use bluer::{Address, AddressType};
use uuid::Uuid;
use log::{debug, error, info, warn};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Instant};

use crate::transport::{looks_like_airpods, BoxFuture, DeviceEntry, HostStatus, Link, LinkError, Transport};

/// Apple's Bluetooth SIG company ID, as set by `DeviceID = bluetooth:004C:...`.
const APPLE_VENDOR_ID: u32 = 0x004C;
/// Pairing waits on the user (case button) and on the buds.
const PAIR_TIMEOUT: Duration = Duration::from_secs(90);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const RECV_BUF: usize = 2048;
/// ENOTCONN can be returned by `send` right after connect while BlueZ is
/// still finishing the channel configuration (upstream fix a78a8bb).
const SEND_RETRIES: usize = 10;
const ENOTCONN: i32 = 107;

/// Turn the raw errno from an L2CAP `connect()` into something a user can
/// act on. The kernel maps HCI status codes onto errnos (`bt_to_errno`), so
/// a few of them mean very specific Bluetooth things.
fn explain_connect_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        // HCI 0x06 "PIN or Key Missing": the buds no longer accept our link key.
        Some(52) => "the AirPods rejected this host's pairing key (HCI: PIN or Key Missing). \
Remove them in Bluetooth settings (bluetoothctl remove <MAC>) and pair again"
            .into(),
        // HCI 0x05 "Authentication Failure".
        Some(13) => "authentication failed; remove the AirPods in Bluetooth settings and pair again".into(),
        // HCI 0x04 / 0x3c page timeout: nobody answered.
        Some(112) => "the AirPods did not answer (in the case, out of range, or asleep)".into(),
        Some(110) => "timed out waiting for the AirPods".into(),
        // PSM 31 without the Apple vendor ID, or a second AACP client.
        Some(111) => "connection refused by the AirPods (Hearing Health channel closed, or another app is using it)".into(),
        _ => e.to_string(),
    }
}

fn adapter_err(e: bluer::Error) -> LinkError {
    LinkError::Adapter(e.to_string())
}

async fn default_adapter() -> Result<bluer::Adapter, LinkError> {
    let session = bluer::Session::new().await.map_err(adapter_err)?;
    session.default_adapter().await.map_err(adapter_err)
}

fn parse_addr(mac: &str) -> Result<Address, LinkError> {
    mac.parse::<Address>().map_err(|e| LinkError::Other(format!("bad address {mac}: {e}")))
}

/// Every device BlueZ knows about, paired or only seen in a scan.
async fn known_devices(adapter: &bluer::Adapter) -> Result<Vec<DeviceEntry>, LinkError> {
    let target = Uuid::parse_str(airpods_proto::aacp::SERVICE_UUID).map_err(|e| LinkError::Other(e.to_string()))?;
    let addrs = adapter.device_addresses().await.map_err(adapter_err)?;
    let mut out = Vec::new();
    for addr in addrs {
        let Ok(device) = adapter.device(addr) else { continue };
        let name = device.alias().await.unwrap_or_else(|_| addr.to_string());
        let connected = device.is_connected().await.unwrap_or(false);
        let paired = device.is_paired().await.unwrap_or(false);
        let is_airpods = matches!(device.uuids().await, Ok(Some(uuids)) if uuids.contains(&target));
        out.push(DeviceEntry { mac: addr.to_string(), name, connected, is_airpods, paired });
    }
    Ok(out)
}

#[derive(Default)]
pub struct BluezTransport;

impl BluezTransport {
    pub fn new() -> Self {
        Self
    }
}

impl Transport for BluezTransport {
    fn name(&self) -> &'static str {
        "BlueZ"
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>> {
        Box::pin(async {
            let adapter = default_adapter().await?;
            let mut out = known_devices(&adapter).await?;
            // AirPods first, then connected devices.
            out.sort_by_key(|d| (!d.is_airpods, !d.connected));
            Ok(out)
        })
    }

    fn host_status(&self) -> BoxFuture<'_, HostStatus> {
        Box::pin(async {
            let mut st = HostStatus::default();
            // Only a permission problem is a setup matter; anything else
            // shows up when connecting.
            if let Err(e) = Socket::<bluer::l2cap::SeqPacket>::new_seq_packet() {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    st.socket_error = Some(e.to_string());
                }
            }
            let adapter = match default_adapter().await {
                Ok(a) => a,
                Err(e) => {
                    st.adapter_error = Some(e.to_string());
                    return st;
                }
            };
            st.powered = adapter.is_powered().await.unwrap_or(false);
            st.apple_identity = matches!(adapter.modalias().await, Ok(Some(m)) if m.vendor == APPLE_VENDOR_ID);
            if let Ok(devs) = known_devices(&adapter).await {
                st.paired_airpods = devs.into_iter().filter(|d| d.paired && looks_like_airpods(d)).collect();
            }
            st
        })
    }

    fn scan(&self, secs: u64) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>> {
        Box::pin(async move {
            let adapter = default_adapter().await?;
            if !adapter.is_powered().await.unwrap_or(false) {
                adapter.set_powered(true).await.map_err(adapter_err)?;
            }
            let before: Vec<String> = known_devices(&adapter).await?.into_iter().map(|d| d.mac).collect();
            info!("scanning for {secs} s");
            {
                // Discovery runs while this stream is alive; bluetoothd
                // records what it finds, so the events are not needed.
                let _discovery = adapter.discover_devices().await.map_err(adapter_err)?;
                sleep(Duration::from_secs(secs)).await;
            }
            let found: Vec<DeviceEntry> = known_devices(&adapter).await?.into_iter().filter(|d| !d.paired).collect();
            let airpods: Vec<DeviceEntry> = found.iter().filter(|d| looks_like_airpods(d)).cloned().collect();
            if !airpods.is_empty() {
                return Ok(airpods);
            }
            Ok(found.into_iter().filter(|d| !before.contains(&d.mac)).collect())
        })
    }

    fn pair<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            let addr = parse_addr(mac)?;
            let adapter = default_adapter().await?;
            let device = adapter.device(addr).map_err(adapter_err)?;
            info!("pairing with {addr}");
            if !device.is_paired().await.unwrap_or(false) {
                match timeout(PAIR_TIMEOUT, device.pair()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => return Err(LinkError::Other(format!("pairing failed: {e}"))),
                    Err(_) => return Err(LinkError::Other("pairing timed out".into())),
                }
            }
            if let Err(e) = device.set_trusted(true).await {
                warn!("could not trust {addr}: {e}");
            }
            // Best effort: the audio profiles may fail on a host without
            // A2DP, and the session pages the buds itself anyway.
            if let Ok(Err(e)) = timeout(CONNECT_TIMEOUT, device.connect()).await {
                debug!("connect after pairing: {e}");
            }
            Ok(())
        })
    }

    fn forget<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            let addr = parse_addr(mac)?;
            let adapter = default_adapter().await?;
            info!("removing pairing with {addr}");
            adapter.remove_device(addr).await.map_err(adapter_err)
        })
    }

    fn bounce<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            let addr = mac.parse::<Address>().map_err(|e| LinkError::Other(format!("bad address {mac}: {e}")))?;
            let session = bluer::Session::new().await.map_err(|e| LinkError::Adapter(e.to_string()))?;
            let adapter = session.default_adapter().await.map_err(|e| LinkError::Adapter(e.to_string()))?;
            let device = adapter.device(addr).map_err(|e| LinkError::Adapter(e.to_string()))?;
            info!("bouncing ACL link to {addr}");
            device.disconnect().await.map_err(|e| LinkError::Adapter(e.to_string()))
        })
    }

    fn connect<'a>(&'a self, mac: &'a str, psm: u16) -> BoxFuture<'a, Result<Link, LinkError>> {
        Box::pin(async move {
            let addr = mac
                .parse::<Address>()
                .map_err(|e| LinkError::Other(format!("bad address {mac}: {e}")))?;
            info!("L2CAP connecting to {addr} PSM {psm:#06x}");
            let target = SocketAddr::new(addr, AddressType::BrEdr, psm);
            let socket = Socket::new_seq_packet()
                .map_err(|e| LinkError::Connect { psm, msg: format!("socket: {e}") })?;
            let seq = match timeout(CONNECT_TIMEOUT, socket.connect(target)).await {
                Ok(Ok(s)) => Arc::new(s),
                Ok(Err(e)) => return Err(LinkError::Connect { psm, msg: explain_connect_error(&e) }),
                Err(_) => return Err(LinkError::Timeout(psm)),
            };

            // Wait until the channel is fully configured (peer CID assigned).
            let start = Instant::now();
            loop {
                match seq.peer_addr() {
                    Ok(peer) if peer.cid != 0 => break,
                    Ok(_) => {}
                    Err(e) if e.raw_os_error() == Some(ENOTCONN) => {
                        return Err(LinkError::Connect { psm, msg: "peer disconnected during setup".into() });
                    }
                    Err(e) => warn!("peer_addr error: {e}"),
                }
                if start.elapsed() >= CONNECT_TIMEOUT {
                    return Err(LinkError::Timeout(psm));
                }
                sleep(POLL_INTERVAL).await;
            }
            info!("L2CAP channel to {addr} PSM {psm:#06x} established");

            let (host_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
            let (in_tx, host_rx) = mpsc::channel::<Vec<u8>>(64);

            let recv_sock = seq.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; RECV_BUF];
                loop {
                    // Stop as soon as the session drops its end of the link:
                    // otherwise this task would pin the socket open until the
                    // *remote* hangs up, the L2CAP channels would leak across a
                    // Disconnect/Connect, and the buds (one ATT client only)
                    // would refuse the next PSM 31 connect.
                    let res = tokio::select! {
                        r = recv_sock.recv(&mut buf) => r,
                        _ = in_tx.closed() => {
                            debug!("PSM {psm:#06x}: session closed the link");
                            break;
                        }
                    };
                    match res {
                        Ok(0) => {
                            info!("PSM {psm:#06x}: remote closed");
                            break;
                        }
                        Ok(n) => {
                            debug!("PSM {psm:#06x} <- {}", hex::encode(&buf[..n]));
                            if in_tx.send(buf[..n].to_vec()).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("PSM {psm:#06x} recv error: {e}");
                            break;
                        }
                    }
                }
                // Last Arc holder (the send task exits when the session drops
                // `tx`) -> the socket is closed here.
                info!("PSM {psm:#06x}: link closed");
            });

            let send_sock = seq;
            tokio::spawn(async move {
                while let Some(data) = out_rx.recv().await {
                    let mut attempt = 0;
                    loop {
                        match send_sock.send(&data).await {
                            Ok(_) => {
                                debug!("PSM {psm:#06x} -> {}", hex::encode(&data));
                                break;
                            }
                            Err(e) if e.raw_os_error() == Some(ENOTCONN) && attempt < SEND_RETRIES => {
                                attempt += 1;
                                warn!("PSM {psm:#06x} send ENOTCONN, retry {attempt}");
                                sleep(POLL_INTERVAL).await;
                            }
                            Err(e) => {
                                error!("PSM {psm:#06x} send error: {e}");
                                return;
                            }
                        }
                    }
                }
            });

            Ok(Link { tx: host_tx, rx: host_rx })
        })
    }
}
