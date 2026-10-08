//! The cheat list over a game: what SELECT+X asks for, how the list moves and flips, and what
//! it hands back when it closes. Reading and writing the `.cht` file is `Session`'s, and
//! `slot-store`'s own tests hold that.

mod common;

use common::{app_playing_in, tmp_root_with_carts};
use slot::app::App;
use slot_input::{Action, Btn};
use slot_ui::CHEAT_ROWS;

fn press(a: &mut App, btn: Btn) {
    a.apply(Action::GbaDown(btn));
    a.apply(Action::GbaUp(btn));
}

fn list(n: usize) -> Vec<(String, bool)> {
    (0..n).map(|i| (format!("Cheat {i}"), i == 1)).collect()
}

fn playing() -> (tempfile::TempDir, App) {
    let d = tmp_root_with_carts(&["Emerald"]);
    let a = app_playing_in(d.path(), "Emerald");
    (d, a)
}

#[test]
fn select_x_in_a_game_asks_for_the_cheat_list() {
    let (_d, mut a) = playing();
    a.apply(Action::CheatsToggle);
    assert!(a.take_cheats_toggle());
    assert!(!a.take_cheats_toggle(), "asked once, not every frame after");
}

#[test]
fn the_list_flips_cheats_and_hands_back_what_it_closed_on() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(list(3));
    assert!(a.cheat_menu_open());
    press(&mut a, Btn::A);
    assert_eq!(a.cheat_enabled(0), Some(true));
    press(&mut a, Btn::Down);
    press(&mut a, Btn::Left);
    assert_eq!(a.cheat_enabled(1), Some(false));
    press(&mut a, Btn::Down);
    press(&mut a, Btn::Right);
    press(&mut a, Btn::B);
    assert!(!a.cheat_menu_open());
    assert_eq!(a.take_cheat_commit(), Some(vec![true, false, true]));
    assert_eq!(a.take_cheat_commit(), None);
}

#[test]
fn a_list_closed_with_nothing_changed_hands_back_nothing() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(list(3));
    press(&mut a, Btn::Down);
    // A flip and its undo is no change at all.
    press(&mut a, Btn::A);
    press(&mut a, Btn::A);
    a.apply(Action::CheatsToggle);
    assert!(!a.cheat_menu_open(), "SELECT+X closes the list it opened");
    assert_eq!(a.take_cheat_commit(), None);
}

#[test]
fn right_on_a_cheat_already_on_changes_nothing() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(list(3));
    press(&mut a, Btn::Down);
    press(&mut a, Btn::Right);
    press(&mut a, Btn::B);
    assert_eq!(a.take_cheat_commit(), None);
}

#[test]
fn the_window_follows_the_bar_down_a_long_list() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(list(20));
    press(&mut a, Btn::R1);
    let v = a.cheat_menu_view().expect("the list is up");
    assert_eq!((v.row, v.top), (CHEAT_ROWS, 1));
    press(&mut a, Btn::R1);
    press(&mut a, Btn::R1);
    let v = a.cheat_menu_view().expect("the list is up");
    assert_eq!((v.row, v.top), (19, 20 - CHEAT_ROWS), "stops on the last cheat");
    press(&mut a, Btn::Up);
    assert_eq!(a.cheat_menu_view().map(|v| v.top), Some(20 - CHEAT_ROWS));
}

#[test]
fn every_opening_is_a_new_list() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(list(2));
    let first = a.cheat_menu_view().map(|v| v.generation);
    press(&mut a, Btn::B);
    a.open_cheat_menu(list(2));
    assert_ne!(a.cheat_menu_view().map(|v| v.generation), first);
}

#[test]
fn an_empty_list_is_not_put_up() {
    let (_d, mut a) = playing();
    a.open_cheat_menu(Vec::new());
    assert!(!a.cheat_menu_open());
}
