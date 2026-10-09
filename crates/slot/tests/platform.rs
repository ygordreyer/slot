mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use slot::app::Phase;
use slot::session::Session;
use slot_input::{Btn, RawEvent};
use slot_store::{core_for_platform, Core, Platform, StateRing};

fn seated_core(root: &Path, stem: &str) -> Core {
    core_for_platform(root, stem, Platform::Gb)
}

fn seat_and_autosave(root: &Path) {
    common::clocked(root);
    let mut s = Session::boot(root.to_path_buf());
    s.feed([RawEvent::Down(Btn::A)], 16);
    s.feed([RawEvent::Up(Btn::A)], 32);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut now = 32;
    while !matches!(s.app().phase(), Phase::Playing { .. }) {
        assert!(Instant::now() < deadline, "the cart never seated");
        now += 16;
        s.feed([], now);
        s.update(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(1));
    }

    s.app_mut().tick_ms(60_000);
    s.app_mut().settle_saves();
}

#[test]
fn a_game_boy_carts_autosave_lands_under_gb_and_never_under_gba() {
    let d = common::tmp_root_with_gb_carts(&["Tetris", "Zelda"]);

    seat_and_autosave(d.path());

    assert!(
        StateRing::new(
            d.path(),
            Platform::Gb,
            seated_core(d.path(), "Tetris"),
            "Tetris"
        )
        .read_resume()
        .unwrap()
        .is_some(),
        "the Game Boy cart's autosave did not land under its own platform's directory"
    );
    assert!(
        StateRing::new(
            d.path(),
            Platform::Gba,
            seated_core(d.path(), "Tetris"),
            "Tetris"
        )
        .read_resume()
        .unwrap()
        .is_none(),
        "the Game Boy cart's autosave was filed as a GBA cart's, which is where a GBA game \
         of the same name keeps its own"
    );

    assert!(
        d.path().join("Saves/GB/Tetris.sav").is_file(),
        "the Game Boy cart's battery save did not land under its own platform's directory"
    );
    assert!(
        !d.path().join("Saves/GBA/Tetris.sav").exists(),
        "the Game Boy cart's battery save was written where a GBA game of the same name \
         keeps its own"
    );
}

#[test]
fn a_hand_organised_game_boy_card_scans_seats_saves_and_resumes() {
    let d = common::tmp_root_with_gb_carts(&["Tetris", "Zelda"]);

    seat_and_autosave(d.path());

    let resume = StateRing::new(
        d.path(),
        Platform::Gb,
        seated_core(d.path(), "Tetris"),
        "Tetris",
    )
    .read_resume()
    .unwrap();
    assert!(resume.is_some(), "nothing was written back to resume from");

    let again = slot::app::App::boot(d.path());
    let cart = again
        .seated_cart()
        .expect("the next boot came up with an empty slot");
    assert_eq!(
        cart.platform,
        Platform::Gb,
        "the card came back holding a cart for the wrong machine"
    );
    assert_eq!(cart.stem, "Tetris");
    assert!(
        cart.rom.ends_with("Games/GB/Tetris.gb"),
        "the rom the slot is holding is {:?}, which is not the one in the Game Boy folder",
        cart.rom
    );
}

#[test]
fn a_same_named_favorite_launches_and_saves_under_its_own_platform() {
    let d = common::tmp_root_with_carts(&["Tetris", "Zzz"]);
    common::write_gb_cart(&d, "Tetris", "TETRIS");
    common::clocked(d.path());
    let keys = std::collections::BTreeSet::from([
        "Games/GBA/Tetris.gba".to_string(),
        "Games/GB/Tetris.gb".to_string(),
    ]);
    slot_store::favorites::write_favorites(d.path(), &keys).unwrap();
    let mut session = Session::boot(d.path().to_path_buf());
    session.feed([RawEvent::Down(Btn::L1), RawEvent::Up(Btn::L1)], 10);
    session.feed([RawEvent::Down(Btn::Right), RawEvent::Up(Btn::Right)], 20);
    assert_eq!(
        session.app().selected_key().as_deref(),
        Some("Games/GB/Tetris.gb")
    );
    session.feed([RawEvent::Down(Btn::A)], 30);
    session.feed([RawEvent::Up(Btn::A)], 40);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut now = 40;
    while !matches!(session.app().phase(), Phase::Playing { .. }) {
        assert!(Instant::now() < deadline, "the favorite never seated");
        now += 16;
        session.feed([], now);
        session.update(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        session.app().seated_cart().unwrap().rom,
        d.path().join("Games/GB/Tetris.gb")
    );
    session.app_mut().tick_ms(60_000);
    session.app_mut().settle_saves();
    assert!(d.path().join("Saves/GB/Tetris.sav").is_file());
    assert!(!d.path().join("Saves/GBA/Tetris.sav").exists());
    let state = slot_store::read_slot_state(d.path());
    assert_eq!(state.cart_key.as_deref(), Some("Games/GB/Tetris.gb"));
    assert_eq!(state.cart.as_deref(), Some("Tetris"));
}
