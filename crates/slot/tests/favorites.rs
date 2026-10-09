mod common;

use std::collections::BTreeSet;
use std::path::Path;

use slot::app::{App, Phase, ShelfKind};
use slot::session::Session;
use slot_gfx::TexId;
use slot_input::{Action, Btn, RawEvent};
use slot_store::favorites::{read_favorites, write_favorites, FAVORITES_FILE};
use slot_store::{Cart, Platform};
use slot_ui::Draw;

struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation();
        std::alloc::GlobalAlloc::alloc(&std::alloc::System, layout)
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        count_allocation();
        std::alloc::GlobalAlloc::alloc_zeroed(&std::alloc::System, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        count_allocation();
        std::alloc::GlobalAlloc::realloc(&std::alloc::System, ptr, layout, size)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        std::alloc::GlobalAlloc::dealloc(&std::alloc::System, ptr, layout);
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(draw: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|count| count.set(Some(0)));
    draw();
    ALLOCATIONS.with(|count| count.replace(None).unwrap())
}

fn cart(platform: Platform, stem: &str) -> Cart {
    Cart {
        platform,
        stem: stem.into(),
        rom: format!(
            "Games/{}/{stem}.{}",
            platform.dir_name(),
            platform.extensions()[0]
        )
        .into(),
        label: None,
        title: String::new(),
        code: String::new(),
        shell: None,
    }
}

fn tap(app: &mut App, button: Btn) {
    app.apply(Action::GbaDown(button));
    app.apply(Action::GbaUp(button));
}

fn boot(root: &Path) -> App {
    common::clocked(root);
    App::boot(root)
}

#[test]
fn plain_y_persists_and_select_y_still_only_changes_colour() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    common::clocked(root.path());
    let mut session = Session::boot(root.path().into());
    let key = session.app().selected_key().unwrap();
    session.feed([RawEvent::Down(Btn::Y), RawEvent::Up(Btn::Y)], 10);
    assert!(session.app().is_favorite(&key));
    assert_eq!(
        read_favorites(root.path()).unwrap(),
        BTreeSet::from([key.clone()])
    );
    session.feed([RawEvent::Down(Btn::Select)], 20);
    session.feed([RawEvent::Down(Btn::Y), RawEvent::Up(Btn::Y)], 30);
    session.feed([RawEvent::Up(Btn::Select)], 40);
    assert!(session.app().colour_correction());
    assert!(session.app().is_favorite(&key));
    assert_eq!(
        read_favorites(root.path()).unwrap(),
        BTreeSet::from([key.clone()])
    );
    session.feed([RawEvent::Down(Btn::Y), RawEvent::Up(Btn::Y)], 50);
    assert!(!session.app().is_favorite(&key));
    assert!(read_favorites(root.path()).unwrap().is_empty());
}

#[test]
fn y_is_ignored_by_core_picker_quick_menu_and_clock_overlay() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    let mut app = boot(root.path());
    let key = app.selected_key().unwrap();
    app.set_core_board_faces(TexId::from_raw(1), TexId::from_raw(2));
    tap(&mut app, Btn::Start);
    assert!(app.core_picker().is_some());
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    app = boot(root.path());
    app.apply(Action::QuickMenu);
    assert!(matches!(app.phase(), Phase::QuickMenu { .. }));
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    let unclocked = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    let mut app = App::boot(unclocked.path());
    assert!(matches!(app.phase(), Phase::SetClock { .. }));
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    assert!(!root.path().join(FAVORITES_FILE).exists());
}

#[test]
fn favorites_is_first_in_both_directions_and_empty_platforms_are_skipped() {
    let mut app = App::new(vec![
        cart(Platform::Gba, "Wars"),
        cart(Platform::Gbc, "Zelda"),
    ]);
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gba));
    tap(&mut app, Btn::L1);
    assert_eq!(app.shelf_kind(), ShelfKind::Favorites);
    assert!(app.empty_favorites());
    assert_eq!(app.shelf_platform_name(), Some("FAVORITES"));
    tap(&mut app, Btn::L1);
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gbc));
    tap(&mut app, Btn::R1);
    assert!(app.empty_favorites());
    tap(&mut app, Btn::R1);
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gba));
    tap(&mut app, Btn::R1);
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gbc));
    tap(&mut app, Btn::R1);
    assert!(app.empty_favorites());
    tap(&mut app, Btn::Y);
    app.apply(Action::Insert);
    assert!(matches!(app.phase(), Phase::Shelf));
    let mut none = App::new(Vec::new());
    tap(&mut none, Btn::R1);
    assert!(none.empty_favorites());
    assert_eq!(none.shelf_platform_name(), Some("FAVORITES"));
}

#[test]
fn favorites_sort_by_name_then_platform_then_path_and_support_letter_jumps() {
    let mut app = App::new(vec![
        cart(Platform::Gb, "Beta"),
        cart(Platform::Gba, "Beta"),
        cart(Platform::Gbc, "Alpha"),
        cart(Platform::Gba, "2 Games"),
        cart(Platform::Gba, "! End"),
        Cart {
            rom: "Games/GBA/Beta.GBA".into(),
            ..cart(Platform::Gba, "Beta")
        },
    ]);
    for _ in 0..4 {
        tap(&mut app, Btn::Y);
        tap(&mut app, Btn::Right);
    }
    tap(&mut app, Btn::R1);
    tap(&mut app, Btn::Y);
    tap(&mut app, Btn::R1);
    tap(&mut app, Btn::Y);
    tap(&mut app, Btn::R1);
    assert_eq!(app.selected_stem(), Some("2 Games"));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_stem(), Some("Alpha"));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GBA/Beta.GBA"));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GBA/Beta.gba"));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GB/Beta.gb"));
    tap(&mut app, Btn::Down);
    assert_eq!(app.selected_stem(), Some("! End"));
    tap(&mut app, Btn::Up);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GBA/Beta.GBA"));
    tap(&mut app, Btn::Up);
    assert_eq!(app.selected_stem(), Some("Alpha"));
}

#[test]
fn removing_the_selected_and_last_favorite_keeps_safe_selection_and_faces() {
    let mut app = App::new(vec![
        cart(Platform::Gba, "Alpha"),
        cart(Platform::Gba, "Beta"),
        cart(Platform::Gba, "Gamma"),
    ]);
    let faces: Vec<_> = (11..14).map(TexId::from_raw).collect();
    app.set_faces(faces.clone());
    for _ in 0..3 {
        tap(&mut app, Btn::Y);
        tap(&mut app, Btn::Right);
    }
    tap(&mut app, Btn::L1);
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_stem(), Some("Beta"));
    tap(&mut app, Btn::Y);
    assert_eq!(app.selected_stem(), Some("Gamma"));
    assert_eq!(app.slot_text().as_deref(), Some("FAVORITES"));
    let mut draws = Vec::new();
    app.draw(&mut draws);
    assert!(draws
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == faces[2])));
    assert!(!draws
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == faces[1])));
    tap(&mut app, Btn::Y);
    assert_eq!(app.selected_stem(), Some("Alpha"));
    tap(&mut app, Btn::Y);
    assert!(app.empty_favorites());
    assert_eq!(app.slot_text().as_deref(), Some("FAVORITES"));
    assert_eq!(app.selected_key(), None);
    for button in [Btn::Left, Btn::Right, Btn::Up, Btn::Down, Btn::Y] {
        tap(&mut app, button);
    }
    draws.clear();
    app.draw(&mut draws);
    assert!(!draws
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if faces.contains(tex))));
}

#[test]
fn favorites_does_not_duplicate_games_or_change_single_cart_boot_and_eject() {
    let root = common::tmp_root_with_carts(&["Wars"]);
    let mut app = App::new(slot_store::scan(root.path()).unwrap());
    tap(&mut app, Btn::Y);
    assert!(app.single_cart());
    assert_eq!(app.carts().count(), 1);
    let key = app.selected_key().unwrap();
    write_favorites(root.path(), &BTreeSet::from([key])).unwrap();
    let mut app = boot(root.path());
    assert!(matches!(app.phase(), Phase::Inserting { .. }));
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gba));
    app.on_core_ready();
    app.update(1.0 / 60.0);
    assert!(matches!(app.phase(), Phase::Playing { .. }));
    app.apply(Action::Eject);
    assert!(matches!(app.phase(), Phase::Playing { .. }));
    assert_eq!(app.carts().count(), 1);
}

#[test]
fn unknown_paths_are_hidden_but_preserved_on_toggle_and_boot_stays_on_gba() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    let missing = "Games/GBC/Removed.gbc".to_string();
    write_favorites(root.path(), &BTreeSet::from([missing.clone()])).unwrap();
    let mut app = boot(root.path());
    assert_eq!(app.shelf_kind(), ShelfKind::Platform(Platform::Gba));
    tap(&mut app, Btn::Y);
    let key = app.selected_key().unwrap();
    assert_eq!(
        read_favorites(root.path()).unwrap(),
        BTreeSet::from([missing, key.clone()])
    );
    tap(&mut app, Btn::L1);
    assert_eq!(app.selected_key(), Some(key));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_stem(), Some("Wars"));
}

#[test]
fn invalid_unreadable_or_failed_storage_never_toggles_or_overwrites() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    let path = root.path().join(FAVORITES_FILE);
    std::fs::write(&path, b"\xff bad paths").unwrap();
    let mut app = boot(root.path());
    let key = app.selected_key().unwrap();
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    assert_eq!(std::fs::read(&path).unwrap(), b"\xff bad paths");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut app = boot(root.path());
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    assert!(path.is_dir());
    std::fs::remove_dir(&path).unwrap();
    let mut app = boot(root.path());
    std::fs::rename(root.path().join("Config"), root.path().join("HeldConfig")).unwrap();
    tap(&mut app, Btn::Y);
    assert!(!app.is_favorite(&key));
    assert!(!path.exists());
}

#[test]
fn favorite_and_platform_views_share_lazy_labels_and_texture_handles() {
    let carts = vec![cart(Platform::Gba, "Alpha"), cart(Platform::Gb, "Alpha")];
    let mut app = App::new(carts.clone());
    tap(&mut app, Btn::Y);
    app.set_faces(vec![TexId::from_raw(11), TexId::from_raw(12)]);
    let path = "Labels/GBA/Alpha.png".into();
    assert_eq!(
        app.attach_label(&carts[0].rom, path),
        Some(TexId::from_raw(11))
    );
    assert!(app.carts().nth(1).unwrap().label.is_none());
    tap(&mut app, Btn::L1);
    assert_eq!(
        app.selected_cart().unwrap().label.as_deref(),
        Some(Path::new("Labels/GBA/Alpha.png"))
    );
    let mut draws = Vec::new();
    app.draw(&mut draws);
    assert!(draws
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == TexId::from_raw(11))));
    assert!(!draws
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == TexId::from_raw(12))));
    tap(&mut app, Btn::R1);
    assert_eq!(
        app.selected_cart().unwrap().label.as_deref(),
        Some(Path::new("Labels/GBA/Alpha.png"))
    );
}

#[test]
fn same_stem_in_mixed_favorites_seats_and_resumes_the_selected_rom_key() {
    let root = common::tmp_root_with_carts(&["Tetris", "Zzz"]);
    common::write_gb_cart(&root, "Tetris", "TETRIS");
    let mut app = boot(root.path());
    tap(&mut app, Btn::Y);
    tap(&mut app, Btn::R1);
    tap(&mut app, Btn::Y);
    tap(&mut app, Btn::R1);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GBA/Tetris.gba"));
    tap(&mut app, Btn::Right);
    assert_eq!(app.selected_key().as_deref(), Some("Games/GB/Tetris.gb"));
    app.apply(Action::Insert);
    assert_eq!(
        app.seated_cart().unwrap().rom,
        root.path().join("Games/GB/Tetris.gb")
    );
    app.on_core_ready();
    for _ in 0..120 {
        app.update(1.0 / 60.0);
    }
    assert!(matches!(app.phase(), Phase::Playing { .. }));
    assert_eq!(
        slot_store::read_slot_state(root.path()).cart_key.as_deref(),
        Some("Games/GB/Tetris.gb")
    );
    let again = App::boot(root.path());
    assert_eq!(
        again.seated_cart().unwrap().rom,
        root.path().join("Games/GB/Tetris.gb")
    );
    assert_eq!(again.shelf_kind(), ShelfKind::Platform(Platform::Gb));
    let mut state = slot_store::read_slot_state(root.path());
    state.cart_key = Some("Games/GB/Missing.gb".into());
    slot_store::write_slot_state(root.path(), &state).unwrap();
    assert!(App::boot(root.path()).seated_cart().is_none());
}

#[test]
fn eject_clears_persisted_cart_identity_and_does_not_reseat_on_reboot() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    let mut app = boot(root.path());
    app.set_snapshot(common::StubSnapshot::boxed());
    app.apply(Action::Insert);
    app.on_core_ready();
    for _ in 0..120 {
        app.update(1.0 / 60.0);
    }
    assert!(matches!(app.phase(), Phase::Playing { .. }));
    let state = slot_store::read_slot_state(root.path());
    assert_eq!(state.cart.as_deref(), Some("Wars"));
    assert_eq!(state.cart_key.as_deref(), Some("Games/GBA/Wars.gba"));

    app.apply(Action::Eject);
    assert!(matches!(app.phase(), Phase::Ejecting { .. }));
    for _ in 0..120 {
        app.update(1.0 / 60.0);
    }
    assert!(matches!(app.phase(), Phase::Shelf));
    let state = slot_store::read_slot_state(root.path());
    assert_eq!(state.cart, None);
    assert_eq!(state.cart_key, None);
    assert_eq!(state.cart_platform, None);
    let again = App::boot(root.path());
    assert!(matches!(again.phase(), Phase::Shelf));
    assert!(again.seated_cart().is_none());
}

#[test]
fn empty_persisted_cart_ignores_a_stale_valid_cart_key_on_boot() {
    let root = common::tmp_root_with_carts(&["Wars", "Zzz"]);
    std::fs::write(
        root.path().join("Config/slot.state"),
        "cart=\ncart_key=Games/GBA/Wars.gba\ncart_platform=gba\nbrightness=5\nblue_light=0\nvolume=5\nmuted=0\nclock_set=1\nutc_offset_min=0\n",
    ).unwrap();
    let state = slot_store::read_slot_state(root.path());
    assert!(state.clock_set);
    assert_eq!(state.cart, None);
    assert_eq!(state.cart_key.as_deref(), Some("Games/GBA/Wars.gba"));
    let app = App::boot(root.path());
    assert!(matches!(app.phase(), Phase::Shelf));
    assert!(app.seated_cart().is_none());
}

#[test]
fn shelf_frames_borrow_cached_identity_and_favorites_without_allocating() {
    let root = common::tmp_root_with_carts(&["Alpha", "Beta"]);
    common::write_gb_cart(&root, "Alpha", "ALPHA");
    let mut app = boot(root.path());
    let star = TexId::from_raw(80);
    app.set_favorite_star(star);
    tap(&mut app, Btn::Y);
    let mut row = Vec::with_capacity(128);
    for textured in [false, true] {
        if textured {
            app.set_faces((11..14).map(TexId::from_raw).collect());
        }
        for favorites in [false, true] {
            if favorites {
                tap(&mut app, Btn::L1);
            }
            row.clear();
            app.draw(&mut row);
            assert!(row
                .iter()
                .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == star)));
            assert_eq!(
                allocations_during(|| {
                    for _ in 0..120 {
                        row.clear();
                        app.draw(&mut row);
                    }
                }),
                0
            );
            if favorites {
                tap(&mut app, Btn::R1);
            }
        }
    }
    app.set_core_board_faces(TexId::from_raw(90), TexId::from_raw(91));
    tap(&mut app, Btn::Start);
    assert!(app.core_picker().is_some());
    row.clear();
    app.draw(&mut row);
    assert_eq!(
        allocations_during(|| {
            for _ in 0..120 {
                row.clear();
                app.draw(&mut row);
            }
        }),
        0
    );
}

#[test]
fn empty_favorites_message_is_centered_without_gpu_access() {
    let mut app = App::new(vec![cart(Platform::Gba, "Alpha")]);
    let text = TexId::from_raw(90);
    app.set_empty_favorites_face(text, 900);
    tap(&mut app, Btn::L1);
    let mut row = Vec::new();
    app.draw(&mut row);
    let (x, y, w, h) = row
        .iter()
        .find_map(|draw| match *draw {
            Draw::Tex {
                x, y, w, h, tex, ..
            } if tex == text => Some((x, y, w, h)),
            _ => None,
        })
        .expect("empty Favorites text missing");
    assert_eq!(x + w / 2.0, slot_gfx::OUT_W as f32 / 2.0);
    assert_eq!(y + h / 2.0, slot_gfx::OUT_H as f32 / 2.0);
    assert!(w <= slot_gfx::OUT_W as f32 - 48.0);
    tap(&mut app, Btn::R1);
    tap(&mut app, Btn::Y);
    tap(&mut app, Btn::L1);
    row.clear();
    app.draw(&mut row);
    assert!(!row
        .iter()
        .any(|draw| matches!(draw, Draw::Tex { tex, .. } if *tex == text)));
}
