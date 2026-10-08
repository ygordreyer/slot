//! App and raw-input integration for button-only home network setup.

mod common;

use common::{clocked, tmp_root_with_carts};
use slot::app::{App, Phase};
use slot::link_radio::{RadioJob, RadioJobs};
use slot::session::Session;
use slot::wifi::{HostWifi, NearbyNetwork, WifiRadio, WifiStatus};
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
