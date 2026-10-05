//! The session: one async task that owns the AACP and ATT links, tracks
//! device state and exposes a small command/event API to the UI.
//!
//! * UI -> session: [`SessionHandle::send`] (sync, non-blocking).
//! * session -> UI: the `emit` callback receives [`SessionEvent`]s. The
//!   Makepad app wraps it in `Cx::post_action`.
//!
//! Hearing-aid writes are read-modify-write over the last buffer read from
//! the device and are debounced (100 ms) so slider drags do not flood the
//! link.

use std::collections::HashMap;
use std::future::pending;
use std::sync::Arc;
use std::time::Duration;

use airpods_proto::aacp::{self, battery, control, frame, handshake, info, opcode};
use airpods_proto::aacp::control::{ControlCommandId, ListeningMode};
use airpods_proto::att::{self, pdu, AttPdu};
use airpods_proto::hearing::{self, Adjustments, Audiogram, HearingAidData};
use airpods_proto::{BatteryInfo, DeviceInfo};
use log::{debug, error, info as linfo, warn};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Instant};

use crate::transport::{DeviceEntry, HostStatus, Link, LinkError, Transport};

/// Debounce for hearing-aid characteristic writes.
pub const WRITE_DEBOUNCE: Duration = Duration::from_millis(100);
const ATT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
/// The buds accept a single ATT client and take a moment to release the
/// previous one; retry PSM 31 a few times before giving up on it.
const ATT_RETRY_DELAY: Duration = Duration::from_secs(3);
const ATT_RETRIES: u32 = 5;
/// How long "Search for AirPods" on the Setup tab keeps discovery running.
const SCAN_SECS: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting(String),
    Connected,
    /// Connected, waiting to retry after a drop.
    Reconnecting { attempt: u32, in_secs: u64 },
    Error(String),
}

/// Everything the UI needs to render, sent whole on every change.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub backend: &'static str,
    pub state: ConnectionState,
    pub device: Option<DeviceEntry>,
    pub info: Option<DeviceInfo>,
    pub hearing_aid_capable: Option<bool>,
    pub battery: Vec<BatteryInfo>,
    pub listening_mode: Option<ListeningMode>,
    /// `None` until the device has echoed both 0x2C and 0x33.
    pub hearing_aid_enabled: Option<bool>,
    pub hearing_aid_enrolled: bool,
    pub gain_swipe: Option<bool>,
    /// ATT (PSM 31) channel is open; hearing-aid data can be read/written.
    pub att_ok: bool,
    pub att_error: Option<String>,
    pub data: Option<HearingAidData>,
    pub last_error: Option<String>,
    pub auto_reconnect: bool,
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    Snapshot(Snapshot),
    Log(String),
    Setup(SetupEvent),
}

/// Answers to the Setup-tab commands. Errors are already worded for the user.
#[derive(Debug, Clone)]
pub enum SetupEvent {
    Status(HostStatus),
    ScanResult(Result<Vec<DeviceEntry>, String>),
    PairResult { mac: String, result: Result<(), String> },
    ForgetResult { mac: String, result: Result<(), String> },
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Connect to the given MAC, or to the first connected AirPods.
    Connect(Option<String>),
    Disconnect,
    RefreshDevices,
    SetHearingAid(bool),
    SetListeningMode(ListeningMode),
    SetGainSwipe(bool),
    SetAdjustments(Adjustments),
    SetAudiogram(Audiogram),
    /// Set own-voice amplification (blob offset 100). Clamped to the safety
    /// ceilings by the encoder like every other gain field.
    SetOwnVoice(f32),
    ResetAdjustments,
    /// Re-read the hearing-aid characteristic.
    Reload,
    SetAutoReconnect(bool),
    /// Setup tab: report the adapter / Apple identity / pairing state.
    CheckSetup,
    /// Setup tab: look for AirPods in pairing mode.
    ScanForAirpods,
    /// Setup tab: pair with this MAC. Closes the current links first.
    Pair(String),
    /// Setup tab: remove the pairing with this MAC. Closes the current links first.
    Forget(String),
}

#[derive(Debug, Clone)]
pub enum DevicesEvent {
    Devices(Vec<DeviceEntry>),
}

#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl SessionHandle {
    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            error!("session task is gone");
        }
    }
}

pub type Emit = Arc<dyn Fn(SessionEvent) + Send + Sync>;

/// Start the session on its own thread with a single-threaded tokio runtime.
pub fn spawn(transport: Arc<dyn Transport>, emit: Emit) -> SessionHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("airpods-session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(run(transport, emit, rx));
        })
        .expect("spawn session thread");
    SessionHandle { tx }
}

/// Run the session loop on the current runtime (used by tests).
pub async fn run(transport: Arc<dyn Transport>, emit: Emit, mut cmd_rx: mpsc::UnboundedReceiver<Command>) {
    let mut s = Session::new(transport, emit);
    s.publish();
    loop {
        // Borrow the receivers separately so the select! branches don't
        // conflict with the handlers below.
        let aacp_rx = s.aacp.as_mut().map(|l| &mut l.rx);
        let att_rx = s.att.as_mut().map(|l| &mut l.rx);
        let flush_at = s.flush_at;
        let reconnect_at = s.reconnect_at;
        let att_retry_at = s.att_retry_at;

        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(cmd) => s.handle_command(cmd).await,
                    None => break,
                }
            }
            pkt = async { match aacp_rx { Some(rx) => rx.recv().await, None => pending().await } } => {
                match pkt {
                    Some(pkt) => s.on_aacp(&pkt),
                    None => s.on_link_closed("AACP").await,
                }
            }
            pdu = async { match att_rx { Some(rx) => rx.recv().await, None => pending().await } } => {
                match pdu {
                    Some(pdu) => s.on_att_unsolicited(&pdu),
                    None => s.on_att_closed("ATT link closed"),
                }
            }
            _ = async { match flush_at { Some(t) => tokio::time::sleep_until(t).await, None => pending().await } } => {
                s.flush_pending().await;
            }
            _ = async { match reconnect_at { Some(t) => tokio::time::sleep_until(t).await, None => pending().await } } => {
                s.reconnect_at = None;
                s.connect(None).await;
            }
            _ = async { match att_retry_at { Some(t) => tokio::time::sleep_until(t).await, None => pending().await } } => {
                s.att_retry_at = None;
                s.open_att().await;
            }
        }
    }
}

struct Session {
    transport: Arc<dyn Transport>,
    emit: Emit,
    snap: Snapshot,
    aacp: Option<Link>,
    att: Option<Link>,
    controls: HashMap<ControlCommandId, Vec<u8>>,
    ha_buf: Option<Vec<u8>>,
    tr_buf: Option<Vec<u8>>,
    pending: Option<HearingAidData>,
    flush_at: Option<Instant>,
    reconnect_at: Option<Instant>,
    reconnect_attempt: u32,
    att_retry_at: Option<Instant>,
    att_attempt: u32,
    /// The ACL link was already bounced once for this device without ATT
    /// coming back; do not loop on it (a bounce also cuts audio profiles).
    bounced: bool,
    /// MAC of the last successfully connected device (for reconnect).
    last_mac: Option<String>,
}

impl Session {
    fn new(transport: Arc<dyn Transport>, emit: Emit) -> Self {
        let backend = transport.name();
        Self {
            transport,
            emit,
            snap: Snapshot {
                backend,
                state: ConnectionState::Disconnected,
                device: None,
                info: None,
                hearing_aid_capable: None,
                battery: Vec::new(),
                listening_mode: None,
                hearing_aid_enabled: None,
                hearing_aid_enrolled: false,
                gain_swipe: None,
                att_ok: false,
                att_error: None,
                data: None,
                last_error: None,
                auto_reconnect: true,
            },
            aacp: None,
            att: None,
            controls: HashMap::new(),
            ha_buf: None,
            tr_buf: None,
            pending: None,
            flush_at: None,
            reconnect_at: None,
            reconnect_attempt: 0,
            att_retry_at: None,
            att_attempt: 0,
            bounced: false,
            last_mac: None,
        }
    }

    fn publish(&self) {
        (self.emit)(SessionEvent::Snapshot(self.snap.clone()));
    }

    fn log(&self, msg: impl Into<String>) {
        let msg = msg.into();
        linfo!("{msg}");
        (self.emit)(SessionEvent::Log(msg));
    }

    fn set_state(&mut self, st: ConnectionState) {
        self.snap.state = st;
        self.publish();
    }

    // ---- commands -------------------------------------------------------

    async fn handle_command(&mut self, cmd: Command) {
        debug!("command: {cmd:?}");
        match cmd {
            Command::Connect(mac) => {
                self.reconnect_at = None;
                self.reconnect_attempt = 0;
                self.connect(mac).await;
            }
            Command::Disconnect => {
                self.reconnect_at = None;
                self.disconnect("disconnected by user");
                self.set_state(ConnectionState::Disconnected);
            }
            Command::RefreshDevices => match self.transport.devices().await {
                Ok(devs) => {
                    let names: Vec<String> = devs
                        .iter()
                        .map(|d| format!("{} [{}]{}{}", d.name, d.mac, if d.connected { " connected" } else { "" }, if d.is_airpods { " AirPods" } else { "" }))
                        .collect();
                    self.log(format!("devices: {}", if names.is_empty() { "none".into() } else { names.join("; ") }));
                }
                Err(e) => self.log(format!("device list failed: {e}")),
            },
            // The setup commands run as their own tasks: a scan or a pairing
            // takes many seconds and must not stall the links.
            Command::CheckSetup => {
                let (t, emit) = (self.transport.clone(), self.emit.clone());
                tokio::spawn(async move {
                    emit(SessionEvent::Setup(SetupEvent::Status(t.host_status().await)));
                });
            }
            Command::ScanForAirpods => {
                let (t, emit) = (self.transport.clone(), self.emit.clone());
                tokio::spawn(async move {
                    let result = t.scan(SCAN_SECS).await.map_err(|e| e.to_string());
                    emit(SessionEvent::Setup(SetupEvent::ScanResult(result)));
                });
            }
            Command::Pair(mac) => {
                self.release_for_setup("pairing");
                let (t, emit) = (self.transport.clone(), self.emit.clone());
                tokio::spawn(async move {
                    let result = t.pair(&mac).await.map_err(|e| e.to_string());
                    emit(SessionEvent::Setup(SetupEvent::PairResult { mac, result }));
                    emit(SessionEvent::Setup(SetupEvent::Status(t.host_status().await)));
                });
            }
            Command::Forget(mac) => {
                self.release_for_setup("removing the pairing");
                let (t, emit) = (self.transport.clone(), self.emit.clone());
                tokio::spawn(async move {
                    let result = t.forget(&mac).await.map_err(|e| e.to_string());
                    emit(SessionEvent::Setup(SetupEvent::ForgetResult { mac, result }));
                    emit(SessionEvent::Setup(SetupEvent::Status(t.host_status().await)));
                });
            }
            Command::SetHearingAid(on) => self.set_hearing_aid(on).await,
            Command::SetListeningMode(mode) => {
                self.send_control(ControlCommandId::ListeningMode, &control::listening_mode(mode)).await;
            }
            Command::SetGainSwipe(on) => {
                self.send_control(ControlCommandId::HpsGainSwipe, &control::gain_swipe(on)).await;
                self.assume_control(ControlCommandId::HpsGainSwipe, &control::gain_swipe(on));
            }
            Command::SetAdjustments(adj) => {
                if let Some(mut d) = self.pending.or(self.snap.data) {
                    adj.apply_to(&mut d);
                    self.queue_write(d);
                } else {
                    self.log("no hearing-aid data loaded yet");
                }
            }
            Command::SetAudiogram(ag) => {
                if let Some(mut d) = self.pending.or(self.snap.data) {
                    ag.apply_to(&mut d);
                    self.queue_write(d);
                } else {
                    self.log("no hearing-aid data loaded yet");
                }
            }
            Command::SetOwnVoice(v) => {
                if let Some(mut d) = self.pending.or(self.snap.data) {
                    d.own_voice_amplification = v;
                    self.queue_write(d);
                } else {
                    self.log("no hearing-aid data loaded yet");
                }
            }
            Command::ResetAdjustments => {
                if let Some(mut d) = self.pending.or(self.snap.data) {
                    Adjustments::RESET.apply_to(&mut d);
                    self.queue_write(d);
                }
            }
            Command::Reload => {
                self.pending = None;
                self.flush_at = None;
                self.read_hearing_aid().await;
                // Also re-read customized transparency (handle 0x18): it has
                // the same amplification/balance/tone layout minus the 4-byte
                // header, and is where the buds may apply gain changes made
                // from the stem swipe. Logged for diagnostics.
                match self.att_request(&pdu::read_req(att::TRANSPARENCY)).await {
                    Ok(AttPdu::ReadRsp(v)) => {
                        if v.len() >= hearing::transparency::MIN_LEN {
                            let f = |o: usize| f32::from_le_bytes([v[o], v[o + 1], v[o + 2], v[o + 3]]);
                            self.log(format!(
                                "transparency: enabled={} L amp {:.2} tone {:.2} R amp {:.2}; hearing aid: L amp {:.2} R amp {:.2}",
                                hearing::transparency::is_enabled(&v).unwrap_or(false),
                                f(32), f(36), f(80),
                                self.snap.data.map(|d| d.left.amplification).unwrap_or(f32::NAN),
                                self.snap.data.map(|d| d.right.amplification).unwrap_or(f32::NAN),
                            ));
                        }
                        self.tr_buf = Some(v);
                    }
                    Ok(other) => warn!("transparency read: {other:?}"),
                    Err(e) => self.log(format!("transparency read failed: {e}")),
                }
                if std::env::var_os("OPENAIRCARE_ATT_SCAN").is_some() {
                    self.scan_handles().await;
                }
                self.publish();
            }
            Command::SetAutoReconnect(v) => {
                self.snap.auto_reconnect = v;
                if !v {
                    self.reconnect_at = None;
                }
                self.publish();
            }
        }
    }

    fn queue_write(&mut self, d: HearingAidData) {
        // Optimistic UI update; the device notification will confirm.
        self.snap.data = Some(d);
        self.pending = Some(d);
        self.flush_at = Some(Instant::now() + WRITE_DEBOUNCE);
        self.publish();
    }

    async fn flush_pending(&mut self) {
        self.flush_at = None;
        let Some(d) = self.pending.take() else { return };
        let Some(mut buf) = self.ha_buf.clone() else {
            self.log("cannot write: no device buffer");
            return;
        };
        if let Err(e) = d.encode_into(&mut buf) {
            self.log(format!("encode failed: {e}"));
            return;
        }
        match self.att_request(&pdu::write_req(att::HEARING_AID, &buf)).await {
            Ok(AttPdu::WriteRsp) => {
                self.ha_buf = Some(buf);
                self.log("hearing-aid settings written");
            }
            Ok(other) => self.log(format!("unexpected write reply: {other:?}")),
            Err(e) => {
                self.log(format!("hearing-aid write failed: {e}"));
                self.snap.last_error = Some(e.to_string());
                self.publish();
            }
        }
    }

    async fn set_hearing_aid(&mut self, on: bool) {
        // Mirrors the Android app: 0x2C then 0x33, then make sure customized
        // transparency is off (the two are mutually exclusive on the device).
        self.send_control(ControlCommandId::HearingAid, &control::hearing_aid(on)).await;
        sleep(Duration::from_millis(50)).await;
        self.send_control(ControlCommandId::HearingAssistConfig, &control::hearing_assist(on)).await;
        // AirPods Pro 3 (fw 8A) apply these but never echo them back, so the
        // snapshot would otherwise keep the stale value and the UI toggle
        // would snap back. Assume the commanded value; a real echo (older
        // firmware does send one) simply overwrites it.
        self.assume_control(ControlCommandId::HearingAid, &control::hearing_aid(on));
        self.assume_control(ControlCommandId::HearingAssistConfig, &control::hearing_assist(on));
        if on {
            if let Some(mut tr) = self.tr_buf.clone() {
                if hearing::transparency::is_enabled(&tr) == Some(true) && hearing::transparency::set_enabled(&mut tr, false) {
                    match self.att_request(&pdu::write_req(att::TRANSPARENCY, &tr)).await {
                        Ok(_) => {
                            self.tr_buf = Some(tr);
                            self.log("customized transparency disabled");
                        }
                        Err(e) => self.log(format!("could not disable customized transparency: {e}")),
                    }
                }
            }
        }
    }

    /// Record a control value we just sent as if the device had echoed it
    /// (only while connected; no-op otherwise).
    fn assume_control(&mut self, id: ControlCommandId, data: &[u8; 4]) {
        if self.aacp.is_none() {
            return;
        }
        let trimmed = match control::parse_raw(&control::build(id, data)) {
            Some((_, v)) => v,
            None => return,
        };
        self.on_control(id, trimmed);
    }

    async fn send_control(&mut self, id: ControlCommandId, data: &[u8; 4]) {
        let Some(link) = &self.aacp else {
            self.log("not connected");
            return;
        };
        let pkt = control::build(id, data);
        if let Err(e) = link.send(&pkt).await {
            self.log(format!("send {id:?} failed: {e}"));
        } else {
            debug!("sent {id:?} {}", hex::encode(data));
        }
    }

    // ---- connection lifecycle -----------------------------------------

    async fn connect(&mut self, mac: Option<String>) {
        if self.aacp.is_some() {
            self.log("already connected");
            return;
        }
        self.snap.last_error = None;
        self.set_state(ConnectionState::Connecting("looking for AirPods".into()));

        let device = match self.pick_device(mac.or_else(|| self.last_mac.clone())).await {
            Ok(d) => d,
            Err(e) => {
                self.fail(e.to_string());
                return;
            }
        };
        self.snap.device = Some(device.clone());
        self.set_state(ConnectionState::Connecting(format!("opening AACP to {}", device.name)));

        let aacp = match self.transport.connect(&device.mac, aacp::PSM).await {
            Ok(l) => l,
            Err(e) => {
                self.fail(format!("AACP: {e}"));
                return;
            }
        };
        self.aacp = Some(aacp);
        self.set_state(ConnectionState::Connecting("handshake".into()));

        for (pkt, delay) in handshake::sequence() {
            if let Some(link) = &self.aacp {
                if let Err(e) = link.send(pkt).await {
                    self.fail(format!("handshake send failed: {e}"));
                    return;
                }
            }
            // Drain anything the device already sent while we wait.
            let deadline = Instant::now() + delay;
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let Some(link) = self.aacp.as_mut() else { return };
                match timeout(deadline - now, link.rx.recv()).await {
                    Ok(Some(pkt)) => self.on_aacp(&pkt),
                    Ok(None) => {
                        self.fail("AACP link closed during handshake".into());
                        return;
                    }
                    Err(_) => break,
                }
            }
        }
        self.last_mac = Some(device.mac.clone());
        self.reconnect_attempt = 0;

        // ATT is optional: it needs the host to present Apple's vendor ID.
        self.set_state(ConnectionState::Connecting("opening ATT".into()));
        self.att_attempt = 0;
        self.open_att().await;
        self.set_state(ConnectionState::Connected);
    }

    /// Open (or re-open) the ATT channel to the current device. On failure
    /// schedules a few retries: the buds refuse PSM 31 for a second or two
    /// after another client (a previous run, the Python script) closed it.
    async fn open_att(&mut self) {
        if self.aacp.is_none() || self.att.is_some() {
            return;
        }
        let Some(mac) = self.snap.device.as_ref().map(|d| d.mac.clone()) else { return };
        self.att_attempt += 1;
        match self.transport.connect(&mac, att::PSM).await {
            Ok(l) => {
                self.att = Some(l);
                self.snap.att_ok = true;
                self.snap.att_error = None;
                self.bounced = false;
                self.log("ATT channel open");
                self.init_att().await;
                self.publish();
            }
            Err(e) => {
                self.snap.att_ok = false;
                if self.att_attempt >= ATT_RETRIES && !self.bounced {
                    // The buds keep refusing PSM 31 after they closed it (or a
                    // previous client did) until the ACL link is re-made.
                    self.bounced = true;
                    self.log("ATT still refused; bouncing the Bluetooth link once");
                    match self.transport.bounce(&mac).await {
                        // AACP drops with the link -> on_link_closed -> reconnect.
                        Ok(()) => {}
                        Err(e) => self.log(format!("bounce failed: {e}")),
                    }
                    self.snap.att_error = Some(format!("{e} (reconnecting)"));
                    self.publish();
                    return;
                }
                let retry = self.att_attempt < ATT_RETRIES;
                self.snap.att_error = Some(if retry {
                    format!("{e} (retrying, attempt {}/{})", self.att_attempt, ATT_RETRIES)
                } else {
                    e.to_string()
                });
                self.log(format!("ATT unavailable: {e}"));
                if retry {
                    self.att_retry_at = Some(Instant::now() + ATT_RETRY_DELAY);
                }
                self.publish();
            }
        }
    }

    async fn pick_device(&self, mac: Option<String>) -> Result<DeviceEntry, LinkError> {
        let devices = self.transport.devices().await?;
        if let Some(mac) = mac {
            if let Some(d) = devices.iter().find(|d| d.mac.eq_ignore_ascii_case(&mac)) {
                return Ok(d.clone());
            }
            // Not in the list (e.g. transport without discovery): try anyway.
            return Ok(DeviceEntry { mac, name: "AirPods".into(), connected: true, is_airpods: true, paired: true });
        }
        // Prefer AirPods BlueZ already has an ACL link to, but fall back to
        // any paired pair: opening the L2CAP socket pages the device itself,
        // and on hosts where the audio profiles fail (e.g. a Pi with no A2DP
        // sink) BlueZ drops the link seconds after it comes up anyway.
        if let Some(d) = devices.iter().find(|d| d.is_airpods && d.connected) {
            return Ok(d.clone());
        }
        match devices.into_iter().find(|d| d.is_airpods) {
            Some(d) => {
                self.log(format!("{} [{}] is paired but not connected; paging it directly", d.name, d.mac));
                Ok(d)
            }
            None => Err(LinkError::NoDevice),
        }
    }

    async fn init_att(&mut self) {
        for (name, h) in [("hearing aid", att::HEARING_AID_CCCD), ("transparency", att::TRANSPARENCY_CCCD)] {
            match self.att_request(&pdu::cccd_enable(h)).await {
                Ok(_) => debug!("notifications enabled for {name}"),
                Err(e) => self.log(format!("enable {name} notifications failed: {e}")),
            }
        }
        match self.att_request(&pdu::read_req(att::TRANSPARENCY)).await {
            Ok(AttPdu::ReadRsp(v)) => self.tr_buf = Some(v),
            Ok(other) => warn!("transparency read: {other:?}"),
            Err(e) => self.log(format!("transparency read failed: {e}")),
        }
        self.read_hearing_aid().await;
    }

    /// Diagnostic (env `OPENAIRCARE_ATT_SCAN`): read every handle 0x0001..=0x0060
    /// and log the ones that answer, to find where a firmware keeps values
    /// we do not know about yet.
    async fn scan_handles(&mut self) {
        for h in 0x0001u16..=0x0060 {
            match self.att_request(&pdu::read_req(h)).await {
                Ok(AttPdu::ReadRsp(v)) => linfo!("ATT scan {h:#06x} ({} bytes): {}", v.len(), hex::encode(&v)),
                Ok(other) => linfo!("ATT scan {h:#06x}: {other:?}"),
                Err(LinkError::AttError { code, .. }) => debug!("ATT scan {h:#06x}: error {code:#04x}"),
                Err(e) => {
                    self.log(format!("ATT scan stopped at {h:#06x}: {e}"));
                    return;
                }
            }
        }
        linfo!("ATT scan done");
    }

    async fn read_hearing_aid(&mut self) {
        match self.att_request(&pdu::read_req(att::HEARING_AID)).await {
            Ok(AttPdu::ReadRsp(v)) => self.apply_ha_buffer(v),
            Ok(other) => self.log(format!("hearing-aid read: unexpected {other:?}")),
            Err(e) => {
                self.log(format!("hearing-aid read failed: {e}"));
                self.snap.last_error = Some(e.to_string());
            }
        }
    }

    fn apply_ha_buffer(&mut self, v: Vec<u8>) {
        match HearingAidData::decode(&v) {
            Ok(d) => {
                self.snap.data = Some(d);
                self.ha_buf = Some(v);
            }
            Err(e) => self.log(format!("hearing-aid data ({} bytes) not decodable: {e}", v.len())),
        }
    }

    fn fail(&mut self, msg: String) {
        error!("{msg}");
        (self.emit)(SessionEvent::Log(msg.clone()));
        self.disconnect(&msg);
        self.snap.last_error = Some(msg.clone());
        self.set_state(ConnectionState::Error(msg));
        self.schedule_reconnect();
    }

    /// Drop the links and any pending retry so the session does not page
    /// the buds while they are being paired or removed.
    fn release_for_setup(&mut self, why: &str) {
        self.reconnect_at = None;
        self.disconnect(why);
        self.set_state(ConnectionState::Disconnected);
    }

    fn disconnect(&mut self, why: &str) {
        if self.aacp.is_some() || self.att.is_some() {
            self.log(format!("closing links: {why}"));
        }
        self.aacp = None;
        self.att = None;
        self.att_retry_at = None;
        self.controls.clear();
        self.ha_buf = None;
        self.tr_buf = None;
        self.pending = None;
        self.flush_at = None;
        self.snap.att_ok = false;
        self.snap.battery.clear();
        self.snap.listening_mode = None;
        self.snap.hearing_aid_enabled = None;
        self.snap.hearing_aid_enrolled = false;
        self.snap.gain_swipe = None;
        self.snap.data = None;
    }

    async fn on_link_closed(&mut self, which: &str) {
        self.fail(format!("{which} link closed by device"));
    }

    fn on_att_closed(&mut self, why: &str) {
        self.att = None;
        self.snap.att_ok = false;
        self.snap.att_error = Some(why.into());
        self.pending = None;
        self.flush_at = None;
        self.log(why);
        // AirPods Pro 3 drop the ATT channel on their own after a few
        // minutes; get it back (retries, then a link bounce).
        if self.aacp.is_some() {
            self.att_attempt = 0;
            self.att_retry_at = Some(Instant::now() + ATT_RETRY_DELAY);
        }
        self.publish();
    }

    fn schedule_reconnect(&mut self) {
        // Retry whenever we know which device to talk to: either it worked
        // before, or discovery found a paired pair that just did not answer
        // (in the case, out of range, link flapping).
        if !self.snap.auto_reconnect || (self.last_mac.is_none() && self.snap.device.is_none()) {
            return;
        }
        self.reconnect_attempt += 1;
        let secs = (RECONNECT_MIN.as_secs() << (self.reconnect_attempt - 1).min(4)).min(RECONNECT_MAX.as_secs());
        self.reconnect_at = Some(Instant::now() + Duration::from_secs(secs));
        self.set_state(ConnectionState::Reconnecting { attempt: self.reconnect_attempt, in_secs: secs });
    }

    // ---- inbound --------------------------------------------------------

    fn on_aacp(&mut self, sdu: &[u8]) {
        // Real buds answer with a longer SDU (01 00 04 00 followed by 14
        // bytes of capability data); only the prefix is fixed.
        if sdu.starts_with(&handshake::HANDSHAKE_ACK) {
            debug!("handshake acknowledged");
            return;
        }
        for pkt in frame::split_packets(sdu) {
            match frame::opcode(pkt) {
                Some(opcode::BATTERY_INFO) => {
                    if let Some(b) = battery::parse(pkt) {
                        self.snap.battery = b;
                        self.publish();
                    }
                }
                Some(opcode::INFORMATION) => {
                    if let Some(i) = info::parse(pkt) {
                        self.snap.hearing_aid_capable = Some(airpods_proto::model::supports_hearing_aid(&i.model_number));
                        self.log(format!("device: {} ({})", i.name, i.model_number));
                        self.snap.info = Some(i);
                        self.publish();
                    }
                }
                Some(opcode::CONTROL_COMMAND) => {
                    if let Some(c) = control::parse(pkt) {
                        self.on_control(c.id, c.value);
                    }
                }
                Some(op) => debug!("AACP opcode {op:#04x} ignored ({} bytes)", pkt.len()),
                None => debug!("unframed AACP data: {}", hex::encode(pkt)),
            }
        }
    }

    fn on_control(&mut self, id: ControlCommandId, value: Vec<u8>) {
        debug!("control {id:?} = {}", hex::encode(&value));
        self.controls.insert(id, value);
        let ha = self.controls.get(&ControlCommandId::HearingAid).map(|v| v.as_slice());
        let assist = self.controls.get(&ControlCommandId::HearingAssistConfig).map(|v| v.as_slice());
        self.snap.hearing_aid_enabled = if ha.is_some() && assist.is_some() {
            Some(control::is_hearing_aid_enabled(ha, assist))
        } else {
            None
        };
        self.snap.hearing_aid_enrolled = control::is_hearing_aid_enrolled(ha);
        self.snap.listening_mode = self
            .controls
            .get(&ControlCommandId::ListeningMode)
            .and_then(|v| v.first())
            .and_then(|b| ListeningMode::from_u8(*b));
        self.snap.gain_swipe = self
            .controls
            .get(&ControlCommandId::HpsGainSwipe)
            .and_then(|v| v.first())
            .map(|b| *b == control::ON);
        self.publish();
    }

    /// Notifications (and stray responses) arriving outside a request.
    fn on_att_unsolicited(&mut self, raw: &[u8]) {
        match pdu::parse(raw) {
            Some(AttPdu::Notification { handle, value }) => self.on_att_notification(handle, value),
            other => debug!("unsolicited ATT pdu: {other:?}"),
        }
    }

    fn on_att_notification(&mut self, handle: u16, value: Vec<u8>) {
        match handle {
            att::HEARING_AID => {
                if self.pending.is_none() {
                    self.apply_ha_buffer(value);
                    self.publish();
                } else {
                    // Keep the device bytes for RMW but let the pending UI values win.
                    if value.len() >= hearing::MIN_LEN {
                        self.ha_buf = Some(value);
                    }
                }
            }
            att::TRANSPARENCY => self.tr_buf = Some(value),
            _ => debug!("notification on handle {handle:#06x}: {}", hex::encode(&value)),
        }
    }

    /// Send an ATT request and wait for its response, handling any
    /// notifications that arrive in between.
    async fn att_request(&mut self, req: &[u8]) -> Result<AttPdu, LinkError> {
        let Some(link) = self.att.as_mut() else { return Err(LinkError::NotConnected) };
        link.send(req).await?;
        let deadline = Instant::now() + ATT_TIMEOUT;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(LinkError::AttTimeout);
            }
            let Some(link) = self.att.as_mut() else { return Err(LinkError::NotConnected) };
            let raw = match timeout(deadline - now, link.rx.recv()).await {
                Ok(Some(raw)) => raw,
                Ok(None) => {
                    self.on_att_closed("ATT link closed");
                    return Err(LinkError::Closed);
                }
                Err(_) => return Err(LinkError::AttTimeout),
            };
            match pdu::parse(&raw) {
                Some(AttPdu::Notification { handle, value }) => self.on_att_notification(handle, value),
                Some(AttPdu::Error { request_opcode, handle, code }) => {
                    return Err(LinkError::AttError { request_opcode, handle, code })
                }
                Some(p) => return Ok(p),
                None => continue,
            }
        }
    }
}
