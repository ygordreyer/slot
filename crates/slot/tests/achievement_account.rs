mod common;

use common::{clocked, tmp_root_with_carts};
use slot::app::{App, Phase};
use slot::session::Session;
use slot_input::{Action, Btn, RawEvent};
use slot_ui::QuickRow;

fn open(app: &mut App) {
    app.apply(Action::QuickMenu);
    for _ in 0..QuickRow::RetroAchievements.index() {
        app.apply(Action::GbaDown(Btn::Down));
    }
    app.apply(Action::GbaDown(Btn::A));
    assert!(app.account_screen().is_some());
}

fn press(session: &mut Session, button: Btn, now: u64) {
    session.feed([RawEvent::Down(button), RawEvent::Up(button)], now);
}

#[test]
fn account_keyboard_handles_select_without_game_chords_and_back_returns_to_settings() {
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let mut session = Session::boot(root.path().into());
    open(session.app_mut());
    press(&mut session, Btn::Down, 100);
    press(&mut session, Btn::Down, 200);
    press(&mut session, Btn::A, 300);
    press(&mut session, Btn::A, 400);
    assert_eq!(
        session
            .app()
            .account_screen()
            .unwrap()
            .keyboard
            .as_ref()
            .unwrap()
            .display(),
        "*|"
    );
    session.feed([RawEvent::Down(Btn::Select)], 500);
    press(&mut session, Btn::Right, 600);
    press(&mut session, Btn::A, 700);
    session.feed([RawEvent::Up(Btn::Select)], 800);
    assert_eq!(
        session
            .app()
            .account_screen()
            .unwrap()
            .keyboard
            .as_ref()
            .unwrap()
            .display(),
        "ab|"
    );
    press(&mut session, Btn::Start, 900);
    assert!(session.app().account_screen().unwrap().keyboard.is_none());
    press(&mut session, Btn::B, 1000);
    assert!(session.app().account_screen().is_none());
    assert!(matches!(
        session.app().phase(),
        Phase::QuickMenu {
            row: QuickRow::RetroAchievements
        }
    ));
    assert!(!root.path().join("Config/retroachievements.toml").exists());
}

#[test]
fn closing_the_lid_drops_the_account_editor() {
    let root = tmp_root_with_carts(&["Emerald", "Fusion"]);
    clocked(root.path());
    let mut app = App::boot(root.path());
    open(&mut app);
    app.apply(Action::GbaDown(Btn::Down));
    app.apply(Action::GbaDown(Btn::Down));
    app.apply(Action::GbaDown(Btn::A));
    app.apply(Action::GbaDown(Btn::A));
    assert!(app.account_screen().unwrap().keyboard.is_some());
    app.apply(Action::LidClose);
    assert!(app.account_screen().is_none());
}
