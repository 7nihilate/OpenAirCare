#![cfg(feature = "mock")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use airpods_link::mock::{MockTransport, MOCK_MAC};
use airpods_link::session::{self, Command, ConnectionState, SessionEvent, SetupEvent, Snapshot};
use airpods_proto::aacp::{control, handshake};
use airpods_proto::att;
use airpods_proto::hearing::{Adjustments, Audiogram, HearingAidData};
use tokio::sync::mpsc;

struct Harness {
    mock: MockTransport,
    cmd: mpsc::UnboundedSender<Command>,
    snaps: Arc<Mutex<Vec<Snapshot>>>,
    logs: Arc<Mutex<Vec<String>>>,
    setup: Arc<Mutex<Vec<SetupEvent>>>,
}

impl Harness {
    fn start() -> Self {
        let mock = MockTransport::new();
        let snaps: Arc<Mutex<Vec<Snapshot>>> = Default::default();
        let logs: Arc<Mutex<Vec<String>>> = Default::default();
        let setup: Arc<Mutex<Vec<SetupEvent>>> = Default::default();
        let (s2, l2, u2) = (snaps.clone(), logs.clone(), setup.clone());
        let emit: session::Emit = Arc::new(move |e| match e {
            SessionEvent::Snapshot(s) => s2.lock().unwrap().push(s),
            SessionEvent::Log(l) => l2.lock().unwrap().push(l),
            SessionEvent::Setup(u) => u2.lock().unwrap().push(u),
        });
        let (cmd, rx) = mpsc::unbounded_channel();
        let transport: Arc<dyn airpods_link::Transport> = Arc::new(mock.clone());
        tokio::spawn(session::run(transport, emit, rx));
        Self { mock, cmd, snaps, logs, setup }
    }

    fn last(&self) -> Snapshot {
        self.snaps.lock().unwrap().last().cloned().expect("at least one snapshot")
    }

    /// Index to pass to `wait_until` so only snapshots published from now
    /// on are considered.
    fn mark(&self) -> usize {
        self.snaps.lock().unwrap().len()
    }

    async fn wait_setup<T>(&self, what: &str, f: impl Fn(&SetupEvent) -> Option<T>) -> T {
        for _ in 0..3000 {
            if let Some(v) = self.setup.lock().unwrap().iter().rev().find_map(|e| f(e)) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}\nsetup events: {:?}", self.setup.lock().unwrap());
    }

    async fn wait_until(&self, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        self.wait_from(0, what, f).await
    }

    async fn wait_from(&self, from: usize, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        for _ in 0..3000 {
            if let Some(s) = self.snaps.lock().unwrap().iter().skip(from).rev().find(|s| f(s)) {
                return s.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for: {what}\nlogs: {:?}\nlast: {:?}", self.logs.lock().unwrap(), self.last());
    }
}

#[tokio::test]
async fn connects_handshakes_and_loads_state() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    let s = h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;

    // Handshake order on the AACP PSM.
    let sent = h.mock.received(airpods_proto::aacp::PSM);
    assert_eq!(sent[0], handshake::HANDSHAKE.to_vec());
    assert_eq!(sent[1], handshake::SET_FEATURE_FLAGS.to_vec());
    assert_eq!(sent[2], handshake::REQUEST_NOTIFICATIONS.to_vec());
    assert_eq!(sent[3], handshake::HOST_CAPABILITIES.to_vec());

    assert_eq!(s.device.as_ref().unwrap().mac, MOCK_MAC);
    assert_eq!(s.info.as_ref().unwrap().model_number, "A3048");
    assert_eq!(s.hearing_aid_capable, Some(true));
    assert_eq!(s.battery.len(), 3);
    assert_eq!(s.listening_mode, Some(control::ListeningMode::Transparency));
    assert_eq!(s.hearing_aid_enabled, Some(false));
    assert!(s.hearing_aid_enrolled);
    assert_eq!(s.gain_swipe, Some(false));
    assert!(s.att_ok);
    let d = s.data.expect("hearing aid data loaded");
    assert_eq!(d.left.eq[0], 20.0);

    // ATT bring-up: CCCDs enabled, transparency + hearing aid read.
    let att_sent = h.mock.received(att::PSM);
    assert!(att_sent.contains(&vec![0x12, 0x2B, 0x00, 0x01, 0x00]));
    assert!(att_sent.contains(&vec![0x12, 0x19, 0x00, 0x01, 0x00]));
    assert!(att_sent.contains(&vec![0x0A, 0x2A, 0x00]));
}

#[tokio::test]
async fn enable_sends_both_control_commands() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    let before = h.mock.received(airpods_proto::aacp::PSM).len();

    h.cmd.send(Command::SetHearingAid(true)).unwrap();
    let s = h.wait_until("enabled", |s| s.hearing_aid_enabled == Some(true)).await;
    assert!(s.hearing_aid_enrolled);

    let sent = h.mock.received(airpods_proto::aacp::PSM);
    assert_eq!(&sent[before][6..], &[0x2C, 0x01, 0x01, 0x00, 0x00]);
    assert_eq!(&sent[before + 1][6..], &[0x33, 0x01, 0x00, 0x00, 0x00]);

    let mark = h.mark();
    h.cmd.send(Command::SetHearingAid(false)).unwrap();
    h.wait_from(mark, "disabled", |s| s.hearing_aid_enabled == Some(false)).await;
    let sent = h.mock.received(airpods_proto::aacp::PSM);
    assert_eq!(&sent[sent.len() - 2][6..], &[0x2C, 0x01, 0x02, 0x00, 0x00]);
    assert_eq!(&sent[sent.len() - 1][6..], &[0x33, 0x02, 0x00, 0x00, 0x00]);
}

#[tokio::test]
async fn slider_spam_is_debounced_into_one_rmw_write() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    let writes_before = h
        .mock
        .received(att::PSM)
        .iter()
        .filter(|p| p.len() > 3 && p[0] == 0x12 && p[1] == 0x2A)
        .count();
    assert_eq!(writes_before, 0);

    for i in 0..20 {
        h.cmd
            .send(Command::SetAdjustments(Adjustments {
                amplification: i as f32 / 20.0,
                balance: -0.25,
                tone: 0.1,
                ambient_noise_reduction: 0.3,
                conversation_boost: true,
            }))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    tokio::time::sleep(session::WRITE_DEBOUNCE + Duration::from_millis(150)).await;

    let writes: Vec<Vec<u8>> = h
        .mock
        .received(att::PSM)
        .into_iter()
        .filter(|p| p.len() > 3 && p[0] == 0x12 && p[1] == 0x2A)
        .collect();
    assert_eq!(writes.len(), 1, "expected exactly one debounced write");
    let value = &writes[0][3..];
    assert_eq!(&value[0..4], &[0x02, 0x02, 0x64, 0x00]);
    let d = HearingAidData::decode(value).unwrap();
    // last amplification 19/20 = 0.95 with balance -0.25 -> left 1.2? no: clamp applies to inputs only
    assert!((d.right.amplification - 0.95).abs() < 1e-6);
    assert!((d.left.amplification - 1.20).abs() < 1e-6);
    assert!(d.left.conversation_boost && d.right.conversation_boost);
    // The original audiogram was preserved by the RMW.
    assert_eq!(d.left.eq[0], 20.0);

    // Device notification after the write is reflected in the snapshot.
    let s = h.wait_until("snapshot updated", |s| s.data.map(|d| d.right.amplification) == Some(0.95)).await;
    assert_eq!(s.data.unwrap().left.eq[1], 25.0);
}

#[tokio::test]
async fn audiogram_write_and_reload() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    let ag = Audiogram { left: [10.0; 8], right: [5.0, 10.0, 15.0, 20.0, 25.0, 30.0, 35.0, 40.0] };
    h.cmd.send(Command::SetAudiogram(ag)).unwrap();
    tokio::time::sleep(session::WRITE_DEBOUNCE + Duration::from_millis(100)).await;
    h.cmd.send(Command::Reload).unwrap();
    let s = h.wait_until("reloaded", |s| s.data.map(|d| d.audiogram()) == Some(ag)).await;
    assert_eq!(s.data.unwrap().right.eq[7], 40.0);
    assert_eq!(h.mock.state.lock().unwrap().hearing_aid_buf[2], 0x64);
}

#[tokio::test]
async fn att_refused_keeps_aacp_connected() {
    let h = Harness::start();
    h.mock.state.lock().unwrap().refuse_psm = Some(att::PSM);
    h.cmd.send(Command::Connect(None)).unwrap();
    let s = h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    assert!(!s.att_ok);
    assert!(s.att_error.as_deref().unwrap_or("").contains("refused"));
    assert!(s.data.is_none());
    assert_eq!(s.listening_mode, Some(control::ListeningMode::Transparency));
}

#[tokio::test]
async fn paired_but_not_connected_airpods_are_paged_directly() {
    // BlueZ may report the buds as disconnected (audio profiles failing,
    // link flapping); the L2CAP connect brings the link up by itself.
    let h = Harness::start();
    h.mock.state.lock().unwrap().reported_connected = false;
    h.cmd.send(Command::Connect(None)).unwrap();
    let s = h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    assert_eq!(s.device.as_ref().unwrap().mac, MOCK_MAC);
    assert!(h.logs.lock().unwrap().iter().any(|l| l.contains("paging it directly")));
}

#[tokio::test]
async fn failed_first_connect_schedules_a_retry() {
    let h = Harness::start();
    {
        let mut st = h.mock.state.lock().unwrap();
        st.reported_connected = false;
        st.refuse_psm = Some(airpods_proto::aacp::PSM);
    }
    h.cmd.send(Command::Connect(None)).unwrap();
    let s = h
        .wait_until("retry scheduled", |s| matches!(s.state, ConnectionState::Reconnecting { attempt: 1, .. }))
        .await;
    assert!(s.last_error.as_deref().unwrap_or("").contains("AACP"));

    // Turning auto-reconnect off cancels the pending retry.
    h.cmd.send(Command::SetAutoReconnect(false)).unwrap();
    let s = h.wait_until("auto off", |s| !s.auto_reconnect).await;
    assert!(matches!(s.state, ConnectionState::Reconnecting { .. }));
}

#[tokio::test]
async fn enable_without_echo_still_updates_snapshot() {
    let h = Harness::start();
    h.mock.state.lock().unwrap().echo_controls = false;
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;
    assert_eq!(h.last().hearing_aid_enabled, Some(false));

    h.cmd.send(Command::SetHearingAid(true)).unwrap();
    let s = h.wait_until("assumed enabled", |s| s.hearing_aid_enabled == Some(true)).await;
    assert!(s.hearing_aid_enrolled);
    // The device really got both commands even though it said nothing.
    let sent = h.mock.received(airpods_proto::aacp::PSM);
    assert_eq!(&sent[sent.len() - 2][6..], &[0x2C, 0x01, 0x01, 0x00, 0x00]);
    assert_eq!(&sent[sent.len() - 1][6..], &[0x33, 0x01, 0x00, 0x00, 0x00]);

    h.cmd.send(Command::SetGainSwipe(true)).unwrap();
    h.wait_until("swipe assumed", |s| s.gain_swipe == Some(true)).await;
    let mark = h.mark();
    h.cmd.send(Command::SetHearingAid(false)).unwrap();
    h.wait_from(mark, "assumed disabled", |s| s.hearing_aid_enabled == Some(false)).await;
}

#[tokio::test(start_paused = true)]
async fn att_refused_once_is_retried() {
    let h = Harness::start();
    {
        let mut st = h.mock.state.lock().unwrap();
        st.refuse_psm = Some(att::PSM);
        st.refuse_count = 1;
    }
    h.cmd.send(Command::Connect(None)).unwrap();
    let s = h.wait_until("connected without ATT", |s| s.state == ConnectionState::Connected).await;
    assert!(!s.att_ok);
    assert!(s.att_error.as_deref().unwrap_or("").contains("retrying"));
    // Paused clock: the 3 s retry fires as soon as the runtime idles.
    let s = h.wait_until("ATT retried", |s| s.att_ok && s.data.is_some()).await;
    assert_eq!(s.data.unwrap().left.eq[0], 20.0);
}

#[tokio::test(start_paused = true)]
async fn att_refused_persistently_bounces_the_link_once() {
    let h = Harness::start();
    {
        let mut st = h.mock.state.lock().unwrap();
        st.refuse_psm = Some(att::PSM);
        st.refuse_count = 0; // refuse until bounced
    }
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected without ATT", |s| s.state == ConnectionState::Connected && !s.att_ok).await;
    // 5 attempts x 3 s, then the bounce; the AACP link drops, the session
    // reconnects (2 s back-off) and this time ATT comes up.
    let s = h.wait_until("ATT after bounce", |s| s.att_ok && s.data.is_some()).await;
    assert_eq!(s.state, ConnectionState::Connected);
    assert_eq!(h.mock.state.lock().unwrap().bounces, 1);
    assert!(h.logs.lock().unwrap().iter().any(|l| l.contains("bouncing")));
}

#[tokio::test(start_paused = true)]
async fn att_closed_by_device_is_reopened() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected && s.att_ok).await;
    let mark = h.mark();
    // The device drops the ATT channel.
    let tx = h.mock.state.lock().unwrap().att_dev_tx.take();
    drop(tx);
    h.wait_from(mark, "att closed", |s| !s.att_ok).await;
    let s = h.wait_from(mark, "att reopened", |s| s.att_ok).await;
    assert_eq!(s.state, ConnectionState::Connected);
}

#[tokio::test]
async fn check_setup_reports_host_status() {
    let h = Harness::start();
    h.mock.state.lock().unwrap().apple_identity = false;
    h.cmd.send(Command::CheckSetup).unwrap();
    let st = h
        .wait_setup("status", |e| match e {
            SetupEvent::Status(s) => Some(s.clone()),
            _ => None,
        })
        .await;
    assert!(!st.apple_identity);
    assert!(!st.ready());
    assert_eq!(st.paired_airpods[0].mac, MOCK_MAC);
}

#[tokio::test]
async fn forget_scan_and_pair_round_trip() {
    let h = Harness::start();
    h.cmd.send(Command::Connect(None)).unwrap();
    h.wait_until("connected", |s| s.state == ConnectionState::Connected).await;

    // Removing the pairing drops the links first and reports the new status.
    let mark = h.mark();
    h.cmd.send(Command::Forget(MOCK_MAC.into())).unwrap();
    h.wait_from(mark, "disconnected", |s| s.state == ConnectionState::Disconnected).await;
    h.wait_setup("unpaired status", |e| match e {
        SetupEvent::Status(s) if s.paired_airpods.is_empty() => Some(()),
        _ => None,
    })
    .await;

    h.cmd.send(Command::ScanForAirpods).unwrap();
    let found = h
        .wait_setup("scan result", |e| match e {
            SetupEvent::ScanResult(Ok(d)) => Some(d.clone()),
            _ => None,
        })
        .await;
    assert_eq!(found.len(), 1);
    assert!(!found[0].paired);

    h.cmd.send(Command::Pair(found[0].mac.clone())).unwrap();
    h.wait_setup("paired", |e| match e {
        SetupEvent::PairResult { result: Ok(()), .. } => Some(()),
        _ => None,
    })
    .await;
    let st = h
        .wait_setup("ready status", |e| match e {
            SetupEvent::Status(s) if s.ready() => Some(s.clone()),
            _ => None,
        })
        .await;
    assert_eq!(st.paired_airpods[0].mac, MOCK_MAC);
}
