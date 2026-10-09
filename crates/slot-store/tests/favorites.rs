mod common;

use std::collections::BTreeSet;

use slot_store::favorites::{read_favorites, write_favorites, FavoritesError, FAVORITES_FILE};

#[test]
fn missing_file_is_empty() {
    let root = common::tmp_root();
    assert!(read_favorites(root.path()).unwrap().is_empty());
    assert!(!root.path().join(FAVORITES_FILE).exists());
}

#[test]
fn paths_round_trip_sorted_with_unicode_and_special_characters() {
    let root = common::tmp_root();
    let keys: BTreeSet<_> = [
        "Games/GBA/ポケモン (日本).gba",
        "Games/GB/Zelda's #1 = Special; [USA].gb",
        "Games/GBA/Advance Wars (USA).gba",
        "Games/GBC/Removed ROM.gbc",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    write_favorites(root.path(), &keys).unwrap();
    assert_eq!(read_favorites(root.path()).unwrap(), keys);
    let text = std::fs::read_to_string(root.path().join(FAVORITES_FILE)).unwrap();
    assert_eq!(
        text,
        keys.iter().map(|k| format!("{k}\n")).collect::<String>()
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("Config"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn malformed_file_is_reported_and_never_replaced() {
    let root = common::tmp_root();
    let path = root.path().join(FAVORITES_FILE);
    for bytes in [
        b"\xff".as_slice(),
        b"/Games/GBA/Game.gba\n",
        b"Games/../Game.gba\n",
        b"Games//GBA/Game.gba\n",
        b"\n",
        b"Games/GBA/Bad\0.gba\n",
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            read_favorites(root.path()),
            Err(FavoritesError::Malformed)
        ));
        assert!(matches!(
            write_favorites(root.path(), &BTreeSet::new()),
            Err(FavoritesError::Malformed)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn unreadable_file_is_distinct_and_never_replaced() {
    let root = common::tmp_root();
    let path = root.path().join(FAVORITES_FILE);
    std::fs::create_dir(&path).unwrap();
    assert!(matches!(
        read_favorites(root.path()),
        Err(FavoritesError::Read(_))
    ));
    assert!(matches!(
        write_favorites(root.path(), &BTreeSet::new()),
        Err(FavoritesError::Read(_))
    ));
    assert!(path.is_dir());
}

#[test]
fn failed_write_is_distinct_and_does_not_leave_a_partial_file() {
    let root = tempfile::tempdir().unwrap();
    let keys = BTreeSet::from(["Games/GBA/Game.gba".to_string()]);
    assert!(matches!(
        write_favorites(root.path(), &keys),
        Err(FavoritesError::Write(_))
    ));
    assert!(!root.path().join(FAVORITES_FILE).exists());
}

#[test]
fn slot_state_remembers_a_rom_key_without_changing_the_save_stem() {
    let root = common::tmp_root();
    let state = slot_store::SlotState {
        cart: Some("Cheats = On".into()),
        cart_key: Some("Games/GBA/Cheats = On.gba".into()),
        ..Default::default()
    };
    slot_store::write_slot_state(root.path(), &state).unwrap();
    assert_eq!(slot_store::read_slot_state(root.path()), state);
}
