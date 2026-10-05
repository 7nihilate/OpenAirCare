//! Transport abstraction: something that can find AirPods and open a
//! packet-oriented (L2CAP SEQPACKET) channel to a given PSM.
//!
//! A transport pumps the socket into a pair of tokio channels so that the
//! session loop can `select!` over several links without caring how the
//! bytes move. The `rx` side closes when the remote hangs up.

use std::future::Future;
use std::pin::Pin;

use tokio::sync::mpsc;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("no paired AirPods found (pair them on the Setup tab first)")]
    NoDevice,
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("connect to PSM {psm:#06x} failed: {msg}")]
    Connect { psm: u16, msg: String },
    #[error("connect to PSM {0:#06x} timed out")]
    Timeout(u16),
    #[error("link closed")]
    Closed,
    #[error("ATT request timed out")]
    AttTimeout,
    #[error("ATT error response: request {request_opcode:#04x} handle {handle:#06x} code {code:#04x}")]
    AttError { request_opcode: u8, handle: u16, code: u8 },
    #[error("not connected")]
    NotConnected,
    #[error("{0}")]
    Other(String),
}

/// A bonded device as seen by the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceEntry {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    /// Advertises the AACP service UUID.
    pub is_airpods: bool,
    /// Bonded with this host (a device seen in a scan is listed but not paired).
    pub paired: bool,
}

/// What the Setup tab needs to know about this computer's Bluetooth.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStatus {
    /// `None` when a Bluetooth adapter was found, otherwise why not.
    pub adapter_error: Option<String>,
    pub powered: bool,
    /// The adapter advertises Apple's vendor ID (`DeviceID = bluetooth:004C:...`).
    /// AirPods refuse the Hearing Health channel (PSM 31) without it.
    pub apple_identity: bool,
    /// Set when this process is not allowed to open an L2CAP socket.
    pub socket_error: Option<String>,
    pub paired_airpods: Vec<DeviceEntry>,
}

impl HostStatus {
    /// Nothing left to do on the Setup tab.
    pub fn ready(&self) -> bool {
        self.adapter_error.is_none()
            && self.apple_identity
            && self.socket_error.is_none()
            && !self.paired_airpods.is_empty()
    }
}

/// Same rule as `setup.sh`: the AACP UUID is only known once BlueZ has
/// talked to the device, so fall back to the name.
pub fn looks_like_airpods(d: &DeviceEntry) -> bool {
    d.is_airpods || d.name.to_ascii_lowercase().contains("airpods")
}

fn unsupported<T>() -> Result<T, LinkError> {
    Err(LinkError::Other("not supported by this Bluetooth backend".into()))
}

/// One open L2CAP channel.
pub struct Link {
    pub tx: mpsc::Sender<Vec<u8>>,
    pub rx: mpsc::Receiver<Vec<u8>>,
}

impl Link {
    pub async fn send(&self, sdu: &[u8]) -> Result<(), LinkError> {
        self.tx.send(sdu.to_vec()).await.map_err(|_| LinkError::Closed)
    }
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Transport: Send + Sync {
    /// Human readable backend name for the UI ("BlueZ", "mock").
    fn name(&self) -> &'static str;

    /// List bonded devices, flagging AirPods (AACP UUID present).
    fn devices(&self) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>>;

    /// Open an L2CAP SEQPACKET channel to `mac` on `psm`.
    fn connect<'a>(&'a self, mac: &'a str, psm: u16) -> BoxFuture<'a, Result<Link, LinkError>>;

    /// Drop the whole ACL link to `mac` (all channels). AirPods keep
    /// refusing PSM 31 once they closed it themselves until the link is
    /// re-established; the session calls this as a last resort.
    fn bounce<'a>(&'a self, mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>>;

    /// Check the adapter, the Apple identity and the pairing for the Setup tab.
    fn host_status(&self) -> BoxFuture<'_, HostStatus> {
        Box::pin(async {
            HostStatus { adapter_error: Some("not supported by this Bluetooth backend".into()), ..Default::default() }
        })
    }

    /// Discover nearby devices for `secs` seconds and return the unpaired
    /// ones that look like AirPods (or, failing that, anything newly seen).
    fn scan(&self, _secs: u64) -> BoxFuture<'_, Result<Vec<DeviceEntry>, LinkError>> {
        Box::pin(async { unsupported() })
    }

    /// Pair with, trust and connect to `mac`.
    fn pair<'a>(&'a self, _mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async { unsupported() })
    }

    /// Remove the pairing with `mac`.
    fn forget<'a>(&'a self, _mac: &'a str) -> BoxFuture<'a, Result<(), LinkError>> {
        Box::pin(async { unsupported() })
    }
}
