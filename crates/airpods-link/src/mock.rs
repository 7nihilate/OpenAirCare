//! A scripted fake AirPods Pro 2 so the app can be developed and tested on
//! any machine. It answers the AACP handshake with battery/info packets,
//! echoes control commands and serves the ATT hearing-aid / transparency
//! characteristics from in-memory buffers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use airpods_proto::aacp::{self, control, frame, handshake, opcode};
use airpods_proto::att::{self, pdu, AttPdu};
use airpods_proto::hearing::{self, HearingAidData};
use tokio::sync::mpsc;

use crate::transport::{BoxFuture, DeviceEntry, HostStatus, Link, LinkError, Transport};

pub const MOCK_MAC: &str = "AA:BB:CC:DD:EE:FF";
pub const MOCK_MODEL: &str = "A3048";

/// Shared, inspectable state of the fake device.
#[derive(Debug)]
pub struct MockState {
    /// Every SDU received, tagged with the PSM it arrived on.
    pub received: Vec<(u16, Vec<u8>)>,
    pub hearing_aid_buf: Vec<u8>,
    pub transparency_buf: Vec<u8>,
    pub loud_sound_reduction: u8,
    pub controls: Vec<(control::ControlCommandId, [u8; 4])>,
    /// If set, connects to this PSM fail (simulates missing DeviceID spoof).
    pub refuse_psm: Option<u16>,
    /// If > 0, `refuse_psm` only applies to this many attempts (then the
    /// refusal is lifted), simulating a slow-to-release ATT channel.
    pub refuse_count: u32,
    /// What `devices()` reports as the BlueZ "Connected" flag.
    pub reported_connected: bool,
    /// Number of `bounce()` calls.
    pub bounces: u32,
    /// Device->host sender of the current AACP link, dropped on bounce so
    /// the host sees the link close.
    pub aacp_dev_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Same for the ATT link (tests drop it to simulate the buds closing it).
    pub att_dev_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Echo control commands back (older firmware does; AirPods Pro 3 on
    /// firmware 8A silently apply 0x2C/0x33 without echoing).
    pub echo_controls: bool,
    /// The fake AirPods are bonded with the host (`forget` / `pair` flip it).
    pub paired: bool,
    /// What `host_status()` reports for the Apple vendor ID.
    pub apple_identity: bool,
}

impl Default for MockState {
    fn default() -> Self {
        let mut ha = HearingAidData::default();
        ha.left.eq = [20.0, 25.0, 30.0, 35.0, 40.0, 40.0, 45.0, 50.0];
        ha.right.eq = [15.0, 20.0, 25.0, 30.0, 35.0, 40.0, 40.0, 45.0];
        ha.own_voice_amplification = 0.5;
        let mut hearing_aid_buf = ha.encode_fresh();
        hearing_aid_buf[2] = 0x60; // as a real device reports it
        Self {
            received: Vec::new(),
            hearing_aid_buf,
            transparency_buf: vec![0u8; hearing::transparency::MIN_LEN],
            loud_sound_reduction: 0,
            controls: vec![
                (control::ControlCommandId::ListeningMode, [0x03, 0, 0, 0]),
                (control::ControlCommandId::HearingAid, [0x01, 0x02, 0, 0]),
                (control::ControlCommandId::HearingAssistConfig, [0x02, 0, 0, 0]),
                (control::ControlCommandId::HpsGainSwipe, [0x02, 0, 0, 0]),
            ],
            refuse_psm: None,
            refuse_count: 0,
            reported_connected: true,
            bounces: 0,
            aacp_dev_tx: None,
            att_dev_tx: None,
            echo_controls: true,
            paired: true,
            apple_identity: true,
        }
    }
}

#[derive(Clone, Default)]
pub struct MockTransport {
    pub state: Arc<Mutex<MockState>>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn received(&self, psm: u16) -> Vec<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .received
            .iter()
            .filter(|(p, _)| *p == psm)
            .map(|(_, d)| d.clone())
            .collect()
    }

    fn entry(&self) -> DeviceEntry {
        let st = self.state.lock().unwrap();
        DeviceEntry {
            mac: MOCK_MAC.into(),
            name: "Mock AirPods Pro".into(),
            connected: st.reported_connected,
            is_airpods: true,
            paired: st.paired,
        }
    }

    fn info_packet() -> Vec<u8> {
        let mut p = vec![0x00, 0x00, 0x00, 0x00];
        p.extend_from_slice(b"\x01\x02\x00Mock AirPods Pro\x00");
        p.extend_from_slice(MOCK_MODEL.as_bytes());
        p.extend_from_slice(b"\x00Apple Inc.\x00MOCKSERIAL\x007E93\x007E93\x001.0.0\x00");
        frame::build(opcode::INFORMATION, &p)
    }

    fn battery_packet() -> Vec<u8> {
        frame::build(
            opcode::BATTERY_INFO,
            &[
                0x03, //
                0x04, 0x01, 87, 0x02, 0x01, // left
                0x02, 0x01, 91, 0x02, 0x01, // right
                0x08, 0x01, 64, 0x01, 0x01, // case
            ],
        )
    }
}

impl Transport for MockTransport {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>> {
        Box::pin(async { Ok(vec![self.entry()]) })
    }

    fn host_status(&self) -> BoxFuture<'_, HostStatus> {
        Box::pin(async {
            let entry = self.entry();
            HostStatus {
                adapter_error: None,
                powered: true,
                apple_identity: self.state.lock().unwrap().apple_identity,
                socket_error: None,
                paired_airpods: if entry.paired { vec![entry] } else { vec![] },
            }
        })
    }

    fn scan(&self, _secs: u64) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let entry = self.entry();
            Ok(if entry.paired { vec![] } else { vec![entry] })
        })
    }

    fn pair<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            if mac != MOCK_MAC {
                return Err(LinkError::NoDevice);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            self.state.lock().unwrap().paired = true;
            Ok(())
        })
    }

    fn forget<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            if mac != MOCK_MAC {
                return Err(LinkError::NoDevice);
            }
            self.state.lock().unwrap().paired = false;
            Ok(())
        })
    }

    fn bounce<'a>(&'a self, _mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async move {
            let mut st = self.state.lock().unwrap();
            st.bounces += 1;
            // A bounce clears whatever made the buds refuse the channel.
            st.refuse_psm = None;
            st.refuse_count = 0;
            // The AACP link drops with it: the session sees the link close.
            if let Some(tx) = st.aacp_dev_tx.take() {
                drop(tx);
            }
            Ok(())
        })
    }

    fn connect<'a>(&'a self, mac: &'a str, psm: u16) -> BoxFuture<'a, Result<Link, LinkError>> {
        Box::pin(async move {
            if mac != MOCK_MAC {
                return Err(LinkError::NoDevice);
            }
            {
                let mut st = self.state.lock().unwrap();
                if st.refuse_psm == Some(psm) {
                    if st.refuse_count > 0 {
                        st.refuse_count -= 1;
                        if st.refuse_count == 0 {
                            st.refuse_psm = None;
                        }
                    }
                    return Err(LinkError::Connect { psm, msg: "Connection refused (os error 111)".into() });
                }
            }
            let (host_tx, mut dev_rx) = mpsc::channel::<Vec<u8>>(64); // host -> device
            let (dev_tx, host_rx) = mpsc::channel::<Vec<u8>>(64); // device -> host
            let state = self.state.clone();
            {
                let mut st = state.lock().unwrap();
                if psm == aacp::PSM {
                    st.aacp_dev_tx = Some(dev_tx.clone());
                } else if psm == att::PSM {
                    st.att_dev_tx = Some(dev_tx.clone());
                }
            }
            // Only the shared state keeps a device->host sender, so tests (and
            // `bounce`) can close the link by dropping that one entry.
            drop(dev_tx);
            tokio::spawn(async move {
                while let Some(sdu) = dev_rx.recv().await {
                    state.lock().unwrap().received.push((psm, sdu.clone()));
                    let replies = match psm {
                        aacp::PSM => handle_aacp(&state, &sdu),
                        att::PSM => handle_att(&state, &sdu),
                        _ => vec![],
                    };
                    for r in replies {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        let tx = {
                            let st = state.lock().unwrap();
                            if psm == aacp::PSM { st.aacp_dev_tx.clone() } else { st.att_dev_tx.clone() }
                        };
                        let Some(tx) = tx else { return };
                        if tx.send(r).await.is_err() {
                            return;
                        }
                    }
                }
                // host hung up: drop our sender too so a bounce can close the link
                let mut st = state.lock().unwrap();
                if psm == aacp::PSM {
                    st.aacp_dev_tx = None;
                } else if psm == att::PSM {
                    st.att_dev_tx = None;
                }
            });
            Ok(Link { tx: host_tx, rx: host_rx })
        })
    }
}

fn handle_aacp(state: &Arc<Mutex<MockState>>, sdu: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if sdu == handshake::HANDSHAKE {
        out.push(handshake::HANDSHAKE_ACK.to_vec());
        out.push(MockTransport::info_packet());
        out.push(MockTransport::battery_packet());
        let controls = state.lock().unwrap().controls.clone();
        for (id, v) in controls {
            out.push(control::build(id, &v).to_vec());
        }
        return out;
    }
    for pkt in frame::split_packets(sdu) {
        if let Some((Some(id), _)) = control::parse_raw(pkt) {
            let mut v = [0u8; 4];
            v.copy_from_slice(&pkt[7..11]);
            let mut st = state.lock().unwrap();
            if let Some(slot) = st.controls.iter_mut().find(|(i, _)| *i == id) {
                slot.1 = v;
            } else {
                st.controls.push((id, v));
            }
            if st.echo_controls {
                out.push(control::build(id, &v).to_vec());
            }
        }
    }
    out
}

fn handle_att(state: &Arc<Mutex<MockState>>, sdu: &[u8]) -> Vec<Vec<u8>> {
    let mut st = state.lock().unwrap();
    let Some((&op, rest)) = sdu.split_first() else { return vec![] };
    let handle = if rest.len() >= 2 { u16::from_le_bytes([rest[0], rest[1]]) } else { 0 };
    match op {
        pdu::OP_READ_REQ => {
            let value = match handle {
                att::HEARING_AID => st.hearing_aid_buf.clone(),
                att::TRANSPARENCY => st.transparency_buf.clone(),
                att::LOUD_SOUND_REDUCTION => vec![st.loud_sound_reduction],
                _ => return vec![vec![pdu::OP_ERROR_RSP, op, rest[0], rest[1], 0x0A]],
            };
            let mut r = vec![pdu::OP_READ_RSP];
            r.extend_from_slice(&value);
            vec![r]
        }
        pdu::OP_WRITE_REQ => {
            let value = &rest[2..];
            match handle {
                att::HEARING_AID => {
                    st.hearing_aid_buf = value.to_vec();
                    let mut ntf = vec![pdu::OP_HANDLE_VALUE_NTF, rest[0], rest[1]];
                    ntf.extend_from_slice(value);
                    vec![vec![pdu::OP_WRITE_RSP], ntf]
                }
                att::TRANSPARENCY => {
                    st.transparency_buf = value.to_vec();
                    vec![vec![pdu::OP_WRITE_RSP]]
                }
                att::LOUD_SOUND_REDUCTION => {
                    st.loud_sound_reduction = value.first().copied().unwrap_or(0);
                    vec![vec![pdu::OP_WRITE_RSP]]
                }
                att::HEARING_AID_CCCD | att::TRANSPARENCY_CCCD => vec![vec![pdu::OP_WRITE_RSP]],
                _ => vec![vec![pdu::OP_ERROR_RSP, op, rest[0], rest[1], 0x0A]],
            }
        }
        pdu::OP_EXCHANGE_MTU_REQ => vec![vec![pdu::OP_EXCHANGE_MTU_RSP, 0x00, 0x02]],
        _ => vec![],
    }
}

/// Convenience for tests: decode a notification/response value.
pub fn parse_att(pdu: &[u8]) -> Option<AttPdu> {
    pdu::parse(pdu)
}
