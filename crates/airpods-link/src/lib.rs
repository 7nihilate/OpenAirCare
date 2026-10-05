//! Transport + session layer for the AirPods hearing-aid feature.
//!
//! * [`transport`] — the [`Transport`] trait (find devices, open L2CAP links).
//! * [`bluez`] — Linux implementation on top of `bluer` (BlueZ).
//! * [`mock`] — a fake AirPods for development and tests (feature `mock`).
//! * [`session`] — the state machine the UI talks to.

pub mod session;
pub mod transport;

#[cfg(target_os = "linux")]
pub mod bluez;

#[cfg(feature = "mock")]
pub mod mock;

pub use session::{Command, ConnectionState, SessionEvent, SessionHandle, SetupEvent, Snapshot};
pub use transport::{DeviceEntry, HostStatus, LinkError, Transport};

/// Pick the default transport for this platform. Returns `None` when there is
/// no real transport (non-Linux builds), in which case callers should fall
/// back to the mock.
pub fn default_transport() -> Option<std::sync::Arc<dyn Transport>> {
    #[cfg(target_os = "linux")]
    {
        Some(std::sync::Arc::new(bluez::BluezTransport::new()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
