//! App and raw-input integration for button-only home network setup.

mod common;

use common::{clocked, tmp_root_with_carts};
use slot::app::{App, Phase};
use slot::link_radio::{RadioJob, RadioJobs};
use slot::session::Session;
use slot::wifi::{
    parse_status, HostWifi, NearbyNetwork, WifiRadio, WifiReply, WifiScreen, WifiStatus,
};
use slot_input::{Action, Btn, RawEvent};
use slot_store::{read_slot_state, read_wifi};
use slot_ui::QuickRow;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn open(app: &mut App) {
    app.apply(Action::QuickMenu);
    for _ in 0..QuickRow::WifiNetworks.index() {
        app.apply(Action::GbaDown(Btn::Down));
    }
    app.apply(Action::GbaDown(Btn::A));
    assert!(app.wifi_screen().is_some());
}

fn await_scan(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.wifi_screen().unwrap().busy {
        app.tick_ms(1000);
        assert!(Instant::now() < deadline, "scan worker did not finish");
        std::thread::yield_now();
    }
}

#[test]
fn network_screen_displays_service_phases_and_refreshes_live_signal() {
    let mut screen = WifiScreen::new(true);
    for (fields, expected) in [
        ("home_phase=ASSOCIATING", "Associating..."),
        ("home_phase=OBTAINING_ADDRESS", "Obtaining an address..."),
        (
            "home_phase=FAILED home_failure=WRONG_PASSWORD",
            "Wrong password",
        ),
        (
            "home_phase=CONNECTED home_ssid=486f6d65 home_ip=192.168.1.2 home_signal_dbm=-54",
            "Connected: -54 dBm (192.168.1.2)",
        ),
        (
            "home_phase=CONNECTED home_ssid=486f6d65 home_ip=192.168.1.2 home_signal_dbm=-65",
            "Connected: -65 dBm (192.168.1.2)",
        ),
        (
            "home_phase=CONNECTED home_ssid=486f6d65 home_ip=192.168.1.2 home_signal_quality=71",
            "Connected: 71% (192.168.1.2)",
        ),
    ] {
        screen.reply(WifiReply::Status(Ok(parse_status(&format!(
            "0 home_enabled=true {fields}"
        )))));
        assert_eq!(screen.status, expected);
    }
    let legacy = "0 home_enabled=true home_ssid=486f6d65 home_ip=192.168.1.2 home_error=";
    let status = parse_status(legacy);
    assert_eq!(parse_status(&status.to_wire()), status);
    assert!(status.phase.is_none() && status.signal.is_none());
    screen.reply(WifiReply::Status(Ok(status)));
    assert_eq!(screen.status, "Connected: 192.168.1.2 (Home)");
}

#[test]
fn network_forgotten_message_survives_idle_status_polls() {
    let mut screen = WifiScreen::new(true);
    let idle = parse_status("0 home_enabled=true home_phase=IDLE");
    screen.reply(WifiReply::Scan(Ok((Vec::new(), Vec::new(), idle.clone()))));
    screen.reply(WifiReply::Changed(Ok(Vec::new())));
    assert_eq!(screen.status, "Network forgotten");
    for _ in 0..2 {
        screen.reply(WifiReply::Status(Ok(idle.clone())));
        assert_eq!(screen.status, "Network forgotten");
    }
}

#[test]
fn forgetting_a_connected_network_preserves_confirmation_on_idle() {
    let mut screen = WifiScreen::new(true);
    screen.reply(WifiReply::Status(Ok(parse_status(
        "0 home_enabled=true home_phase=CONNECTED home_ssid=486f6d65 home_ip=192.168.1.2",
    ))));
    assert_eq!(screen.status, "Connected: 192.168.1.2 (Home)");
    screen.reply(WifiReply::Changed(Ok(Vec::new())));
    assert_eq!(screen.status, "Network forgotten");
    for _ in 0..2 {
        screen.reply(WifiReply::Status(Ok(parse_status(
            "0 home_enabled=true home_phase=IDLE",
        ))));
        assert_eq!(screen.status, "Network forgotten");
    }
}

#[test]
fn unsupported_network_message_survives_idle_status_polls() {
    let mut screen = WifiScreen::new(true);
    let idle = parse_status("0 home_enabled=true home_phase=IDLE");
    screen.reply(WifiReply::Scan(Ok((
        vec![NearbyNetwork {
            ssid: "Office".into(),
            signal: -40,
            secured: true,
            supported: false,
        }],
        Vec::new(),
        idle.clone(),
    ))));
    assert!(screen.input(Btn::A).is_none());
    let expected = "Only WPA2 personal or open networks are supported";
    assert_eq!(screen.status, expected);
    for _ in 0..2 {
        screen.reply(WifiReply::Status(Ok(idle.clone())));
        assert_eq!(screen.status, expected);
    }
}

#[test]
fn idle_status_clears_stale_connection_phase_messages() {
    for (phase, expected) in [
        ("ASSOCIATING", "Associating..."),
        ("AUTHENTICATING", "Authenticating..."),
        ("OBTAINING_ADDRESS", "Obtaining an address..."),
    ] {
        let mut screen = WifiScreen::new(true);
        screen.reply(WifiReply::Status(Ok(parse_status(&format!(
            "0 home_enabled=true home_phase={phase}"
        )))));
        assert_eq!(screen.status, expected);
        screen.reply(WifiReply::Status(Ok(parse_status(
            "0 home_enabled=true home_phase=IDLE",
        ))));
        assert_eq!(screen.status, "Choose a network");
    }
}

#[test]
fn idle_status_clears_a_dropped_connection_message() {
    let mut screen = WifiScreen::new(true);
    screen.reply(WifiReply::Status(Ok(parse_status(
        "0 home_enabled=true home_phase=CONNECTED home_ssid=486f6d65 home_ip=192.168.1.2",
    ))));
    assert_eq!(screen.status, "Connected: 192.168.1.2 (Home)");
    screen.reply(WifiReply::Status(Ok(parse_status(
        "0 home_enabled=true home_phase=IDLE",
    ))));
    assert_eq!(screen.status, "Choose a network");
}

#[test]
fn initial_idle_status_preserves_scan_messages() {
    let mut screen = WifiScreen::new(true);
    let idle = parse_status("0 home_enabled=true home_phase=IDLE");
    assert!(screen.observed.phase.is_none());
    assert_eq!(screen.status, "Scanning...");
    screen.reply(WifiReply::Status(Ok(idle.clone())));
    assert_eq!(screen.status, "Scanning...");
    screen.reply(WifiReply::Scan(Ok((Vec::new(), Vec::new(), idle.clone()))));
    assert_eq!(screen.status, "Choose a network");
    screen.reply(WifiReply::Status(Ok(idle)));
    assert_eq!(screen.status, "Choose a network");
}

#[test]
fn request_error_messages_survive_idle_status_polls() {
    for reply in [
        WifiReply::Scan(Err("COMMAND_FAILED".into())),
        WifiReply::Changed(Err("COMMAND_FAILED".into())),
    ] {
        let mut screen = WifiScreen::new(true);
        let idle = parse_status("0 home_enabled=true home_phase=IDLE");
        screen.reply(WifiReply::Status(Ok(idle.clone())));
        screen.reply(reply);
        let expected = "Wi-Fi request failed. Try again";
        assert_eq!(screen.status, expected);
        for _ in 0..2 {
            screen.reply(WifiReply::Status(Ok(idle.clone())));
            assert_eq!(screen.status, expected);
        }
    }
}

#[test]
fn enabling_uses_the_ordered_home_queue_and_scan_waits_for_its_reply() {
    struct Jobs {
        calls: Arc<Mutex<Vec<RadioJob>>>,
        done: mpsc::Sender<mpsc::Sender<()>>,
    }
    impl RadioJobs for Jobs {
        fn ask(&mut self, job: RadioJob) {
            self.calls.lock().unwrap().push(job);
        }
        fn home_on(&mut self) -> mpsc::Receiver<()> {
            self.ask(RadioJob::Home(true));
            let (done, ready) = mpsc::channel();
            self.done.send(done).unwrap();
            ready
        }
        fn warmed(&self) -> bool {
            false
        }
    }
    struct Radio(Arc<Mutex<Vec<&'static str>>>);
    impl WifiRadio for Radio {
        fn enable(&mut self) -> Result<(), String> {
            panic!("Enable bypassed Home queue")
        }
        fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String> {
            self.0.lock().unwrap().push("scan");
            Ok(Vec::new())
        }
        fn reload(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn connect(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn status(&mut self) -> Result<WifiStatus, String> {
            Ok(WifiStatus::default())
        }
    }
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let mut app = App::boot(root.path());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let scans = Arc::new(Mutex::new(Vec::new()));
    let (done, ready) = mpsc::channel();
    app.set_radio_jobs(Box::new(Jobs {
        calls: calls.clone(),
        done,
    }));
    app.set_wifi_radio(Box::new(Radio(scans.clone())));
    open(&mut app);
    app.apply(Action::GbaDown(Btn::A));
    assert!(read_slot_state(root.path()).home_wifi_enabled);
    assert_eq!(*calls.lock().unwrap(), vec![RadioJob::Home(true)]);
    assert!(scans.lock().unwrap().is_empty());
    ready
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .send(())
        .unwrap();
    await_scan(&mut app);
    assert_eq!(*scans.lock().unwrap(), vec!["scan"]);
}

#[test]
fn turning_on_from_network_screen_persists_and_open_network_connects() {
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let mut app = App::boot(root.path());
    app.set_wifi_radio(Box::new(HostWifi {
        networks: vec![NearbyNetwork {
            ssid: "Guest".into(),
            signal: -40,
            secured: false,
            supported: true,
        }],
        ..Default::default()
    }));
    open(&mut app);
    assert!(!app.wifi_screen().unwrap().enabled);
    app.apply(Action::GbaDown(Btn::A));
    assert!(read_slot_state(root.path()).home_wifi_enabled);
    await_scan(&mut app);
    app.apply(Action::GbaDown(Btn::A));
    await_scan(&mut app);
    assert_eq!(read_wifi(root.path()).unwrap()[0].ssid, "Guest");
    assert_eq!(read_wifi(root.path()).unwrap()[0].password, None);
    app.apply(Action::GbaDown(Btn::B));
    assert!(app.wifi_screen().is_none());
    assert!(matches!(
        app.phase(),
        Phase::QuickMenu {
            row: QuickRow::WifiNetworks
        }
    ));
}

#[test]
fn select_reveals_and_following_buttons_edit_instead_of_triggering_chords() {
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let mut session = Session::boot(root.path().into());
    open(session.app_mut());
    session.feed([RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)], 100);
    await_scan(session.app_mut());
    session.feed([RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)], 200);
    session.feed([RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)], 300);
    session.feed([RawEvent::Down(Btn::Start), RawEvent::Up(Btn::Start)], 400);
    assert!(session
        .app()
        .wifi_screen()
        .unwrap()
        .keyboard
        .as_ref()
        .unwrap()
        .masked());
    let before = read_slot_state(root.path());
    session.feed([RawEvent::Down(Btn::Select), RawEvent::Down(Btn::Up)], 500);
    let keyboard = session
        .app()
        .wifi_screen()
        .unwrap()
        .keyboard
        .as_ref()
        .unwrap();
    assert!(!keyboard.masked());
    assert_eq!(keyboard.focus(), 30);
    assert_eq!(read_slot_state(root.path()).brightness, before.brightness);
    session.feed([RawEvent::Up(Btn::Up), RawEvent::Up(Btn::Select)], 510);
    session.feed([RawEvent::Down(Btn::Y), RawEvent::Up(Btn::Y)], 600);
    assert_eq!(
        session
            .app()
            .wifi_screen()
            .unwrap()
            .keyboard
            .as_ref()
            .unwrap()
            .key(0),
        ' '
    );
    assert_eq!(
        read_slot_state(root.path()).colour_correction,
        before.colour_correction
    );
}

#[test]
fn raw_l2_discards_password_entry_without_changing_saved_networks() {
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let saved = vec![slot_store::WifiNetwork {
        ssid: "Saved".into(),
        password: Some("password".into()),
    }];
    slot_store::write_wifi(root.path(), &saved).unwrap();
    let path = root.path().join("Config/wifi.toml");
    let before = std::fs::read(&path).unwrap();
    let mut session = Session::boot(root.path().into());
    session.app_mut().set_wifi_radio(Box::new(HostWifi {
        networks: vec![NearbyNetwork {
            ssid: "New".into(),
            signal: -40,
            secured: true,
            supported: true,
        }],
        ..Default::default()
    }));
    open(session.app_mut());
    session.feed([RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)], 100);
    await_scan(session.app_mut());
    session.feed([RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)], 200);
    for i in 0..8 {
        session.feed(
            [RawEvent::Down(Btn::A), RawEvent::Up(Btn::A)],
            300 + i * 100,
        );
    }
    assert_eq!(
        session
            .app()
            .wifi_screen()
            .unwrap()
            .keyboard
            .as_ref()
            .unwrap()
            .text(),
        "11111111"
    );
    session.feed([RawEvent::Down(Btn::L2)], 1100);
    let screen = session.app().wifi_screen().unwrap();
    assert!(screen.keyboard.is_none());
    assert!(!screen.busy);
    assert_eq!(read_wifi(root.path()).unwrap(), saved);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    session.feed([RawEvent::Up(Btn::L2)], 1200);
    assert!(session.app().wifi_screen().unwrap().keyboard.is_none());
    assert_eq!(read_wifi(root.path()).unwrap(), saved);
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
