//! Link attempts refuse the running core's cheats before starting or reloading.

mod common;

use slot::app::{App, GameMenu, LinkRow};
use slot_input::{Action, Btn};
use slot_store::Core;
use slot_ui::Toast;

fn playing(core: Core) -> (tempfile::TempDir, App) {
    let root = common::tmp_root_with_carts(&["Emerald"]);
    common::write_retail_header(&root, "Emerald", "POKEMON RUBY", "AXVE");
    let mut app = common::app_playing_in(root.path(), "Emerald");
    app.set_core(core);
    (root, app)
}

#[test]
fn applied_cheats_refuse_both_direct_and_reload_link_attempts() {
    for core in [Core::Gpsp, Core::Mgba] {
        for loaded in ["auto", "rfu"] {
            for role in [LinkRow::Host, LinkRow::Join] {
                let (_root, mut app) = playing(core);
                app.set_link_loaded(loaded);
                app.set_cheats_applied(true);
                common::toggle_link_menu(&mut app);
                if role == LinkRow::Join {
                    app.apply(Action::GbaDown(Btn::Right));
                }
                app.apply(Action::GbaDown(Btn::A));
                assert!(app.refusal_active(app.now()));
                assert_eq!(app.toast(), Some(Toast::TurnCheatsOff));
                assert_eq!(app.game_menu(), Some(GameMenu::Pick(role)));
                assert_eq!(app.take_link_reload(), None);
                assert_eq!(app.link_player(), None);
                assert!(!app.link_active());
                assert!(app.take_link_transport().is_none());
            }
        }
    }
}

#[test]
fn turning_cheats_off_allows_the_reload_again() {
    let (_root, mut app) = playing(Core::Gpsp);
    app.set_link_loaded("rfu");
    app.set_cheats_applied(true);
    common::toggle_link_menu(&mut app);
    app.apply(Action::GbaDown(Btn::A));
    assert_eq!(app.take_link_reload(), None);
    app.set_cheats_applied(false);
    app.apply(Action::GbaDown(Btn::A));
    assert_eq!(app.take_link_reload(), Some(("Emerald".into(), "auto")));
    assert_eq!(app.link_player(), None);
    assert!(matches!(app.game_menu(), Some(GameMenu::Working { .. })));
}

#[test]
fn cheats_applied_by_a_reload_refuse_the_link_before_its_starter() {
    let (_root, mut app) = playing(Core::Gpsp);
    app.set_link_loaded("rfu");
    common::toggle_link_menu(&mut app);
    app.apply(Action::GbaDown(Btn::A));
    assert_eq!(app.take_link_reload(), Some(("Emerald".into(), "auto")));
    assert_eq!(app.link_player(), None);
    app.set_cheats_applied(true);
    app.link_reload_done();
    assert!(app.refusal_active(app.now()));
    assert_eq!(app.toast(), Some(Toast::TurnCheatsOff));
    assert_eq!(app.game_menu(), None);
    assert!(!app.link_active());
    assert!(app.take_link_transport().is_none());
    app.apply(Action::CheatsToggle);
    assert!(app.take_cheats_toggle());
}

#[test]
fn cancelling_a_reload_does_not_allow_cheat_changes_before_it_finishes() {
    let (_root, mut app) = playing(Core::Gpsp);
    app.set_link_loaded("rfu");
    common::toggle_link_menu(&mut app);
    app.apply(Action::GbaDown(Btn::A));
    assert_eq!(app.take_link_reload(), Some(("Emerald".into(), "auto")));
    app.apply(Action::GbaDown(Btn::B));
    app.apply(Action::CheatsToggle);
    assert!(!app.take_cheats_toggle());
    app.link_reload_done();
    app.apply(Action::CheatsToggle);
    assert!(app.take_cheats_toggle());
}
