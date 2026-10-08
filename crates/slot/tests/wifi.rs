//! App and raw-input integration for button-only home network setup.

mod common;

use common::{clocked, tmp_root_with_carts};
use slot::app::{App, Phase};
use slot::session::Session;
use slot::wifi::{HostWifi, NearbyNetwork};
use slot_input::{Action, Btn, RawEvent};
use slot_store::{read_slot_state, read_wifi};
use slot_ui::QuickRow;
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
