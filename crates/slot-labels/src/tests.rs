use super::*;
use image::{ImageFormat, Rgb, RgbImage};
use std::io::Cursor;

fn cart(stem: &str) -> Cart {
    Cart {
        platform: slot_store::Platform::Gba,
        stem: stem.into(),
        rom: format!("Games/GBA/{stem}.gba").into(),
        label: None,
        title: "".into(),
        code: "".into(),
        shell: None,
    }
}

fn image_bytes(w: u32, h: u32) -> Vec<u8> {
    // The top and bottom have different colours: catches upside-down/top-centering errors.
    let im = RgbImage::from_fn(w, h, |_, y| {
        if y < h / 2 {
            Rgb([255, 0, 0])
        } else {
            Rgb([0, 0, 255])
        }
    });
    let mut bytes = Cursor::new(Vec::new());
    im.write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn no_intro_names_match_database_punctuation() {
    for (stem, db) in [
        (
            "Legend of Zelda, The - A Link to the Past & Four Swords (USA)",
            "The Legend of Zelda: A Link to the Past and Four Swords",
        ),
        (
            "WarioWare, Inc. - Mega Microgame$! (USA)",
            "WarioWare, Inc.: Mega Microgame$!",
        ),
        ("Aladdin (USA) (En,Fr,De,Es)", "Disney's Aladdin"),
        (
            "Pokémon - Emerald Version (USA) [!]\u{200B}",
            "Pokemon: Emerald Version",
        ),
    ] {
        assert_eq!(source::title(stem), source::normalize(db));
    }
}

#[test]
fn title_resolution_requires_unique_exact_gba_match() {
    let item = |id, name, platform| {
        format!("<a href='/games/details/{id}-game'><h3>{name}</h3><p>{platform}</p></a>")
    };
    let correct = item(1, "Mario Kart: Super Circuit", "Nintendo Game Boy Advance");
    let page = format!(
        "{}{}{}",
        correct,
        item(2, "Mario Kart: Super Circuit", "Nintendo DS"),
        item(
            3,
            "Mario Kart: Super Circuit 2",
            "Nintendo Game Boy Advance"
        )
    );
    assert_eq!(
        source::game_id(&page, "mario kart super circuit", Platform::Gba).unwrap(),
        vec![1]
    );
    assert_eq!(
        source::game_id(
            &(correct.clone() + &item(4, "Mario Kart: Super Circuit", "Nintendo Game Boy Advance")),
            "mario kart super circuit",
            Platform::Gba
        )
        .unwrap(),
        vec![1, 4]
    );
    assert_eq!(
        source::game_id(
            &(correct.clone() + &correct),
            "mario kart super circuit",
            Platform::Gba
        )
        .unwrap(),
        vec![1]
    );
    assert!(source::game_id(
        "<html>changed layout</html>",
        "mario kart super circuit",
        Platform::Gba
    )
    .is_err());
}

#[test]
fn artwork_is_cartridge_front_for_the_requested_region() {
    let page = r#"
        <a href="https://images.launchbox-app.com/box.png" data-title="Test - Box - Front Image (North America)"></a>
        <a href="https://images.launchbox-app.com/eu.png" data-title="Test - Cart - Front Image (Europe)"></a>
        <a href="https://evil.invalid/us.png" data-title="Test - Cart - Front Image (North America)"></a>
        <a href="https://images.launchbox-app.com/us.png" data-title="Test - Cart - Front Image (North America)"></a>
    "#;
    assert!(source::image_url(page, &cart("Test (USA)"))
        .unwrap()
        .ends_with("/us.png"));
    assert!(source::image_url(page, &cart("Test (Europe)"))
        .unwrap()
        .ends_with("/eu.png"));
    assert!(source::image_url(page, &cart("Test (Japan)"))
        .unwrap()
        .ends_with("/us.png"));
    for url in [
        "http://images.launchbox-app.com/x",
        "https://images.launchbox-app.com.evil/x",
        "file:///tmp/x",
    ] {
        assert!(!source::allowed_url(url));
    }
}

#[test]
fn a_game_boy_cart_matches_its_own_platform_first_and_the_other_second() {
    let item = |id, name, platform| {
        format!("<a href='/games/details/{id}-game'><h3>{name}</h3><p>{platform}</p></a>")
    };
    let page = item(1, "Tetris", "Nintendo Game Boy")
        + &item(2, "Tetris", "Nintendo Game Boy Color")
        + &item(3, "Tetris", "Nintendo Game Boy Advance")
        + &item(4, "Wario Land 3", "Nintendo Game Boy Color");
    assert_eq!(
        source::game_id(&page, "tetris", Platform::Gb).unwrap(),
        vec![1]
    );
    assert_eq!(
        source::game_id(&page, "tetris", Platform::Gbc).unwrap(),
        vec![2]
    );
    assert_eq!(
        source::game_id(&page, "tetris", Platform::Gba).unwrap(),
        vec![3]
    );
    assert_eq!(
        source::game_id(&page, "wario land 3", Platform::Gb).unwrap(),
        vec![4]
    );
    assert!(source::game_id(&page, "wario land 3", Platform::Gba).is_err());
}

#[test]
fn search_queries_keep_punctuation_and_keys_fold_common_variants() {
    for (stem, wanted) in [
        ("Cruis'n Velocity (USA)", "Cruis'n Velocity"),
        ("Pac-Man Collection (USA)", "Pac-Man Collection"),
        ("Power Rangers S.P.D. (USA)", "Power Rangers S.P.D."),
        ("Tron 2.0 - Killer App (USA)", "Tron 2.0 Killer App"),
        (
            "Lilo & Stitch 2 - Haemsterviel Havoc (USA)",
            "Lilo & Stitch 2 Haemsterviel Havoc",
        ),
        ("A - B + . & C (USA)", "A B & C"),
    ] {
        assert_eq!(source::query(stem), wanted);
    }
    let item = |id, name| {
        format!("<a href='/games/details/{id}-game'><h3>{name}</h3><p>Nintendo Game Boy Advance</p></a>")
    };
    assert_eq!(
        source::game_id(
            &item(1, "Megaman Battle Network"),
            "mega man battle network",
            Platform::Gba
        )
        .unwrap(),
        vec![1]
    );
    assert_eq!(
        source::game_id(
            &item(2, "Hämsterviel Havoc"),
            "haemsterviel havoc",
            Platform::Gba
        )
        .unwrap(),
        vec![2]
    );
    assert_eq!(
        source::game_id(
            &(item(3, "Megaman Battle Network") + &item(4, "Mega Man Battle Network")),
            "mega man battle network",
            Platform::Gba
        )
        .unwrap(),
        vec![4]
    );
}

#[test]
fn year_retry_does_not_change_non_year_hyphens() {
    assert_eq!(
        source::year_query("NFL Blitz 20-02"),
        Some("NFL Blitz 2002".into())
    );
    assert_eq!(source::year_query("Mother 1-2"), None);
}

#[test]
fn regional_fallback_uses_regionless_before_world() {
    let regionless = "<a href='https://images.launchbox-app.com/plain.png' data-title='Test - Cart - Front Image'></a>";
    let world = "<a href='https://images.launchbox-app.com/world.png' data-title='Test - Cart - Front Image (World)'></a>";
    assert!(
        source::image_url(&(world.to_owned() + regionless), &cart("Test (Europe)"))
            .unwrap()
            .ends_with("/plain.png")
    );
    assert!(source::image_url(world, &cart("Test (Europe)"))
        .unwrap()
        .ends_with("/world.png"));
}

#[test]
fn aliases_are_keyed_by_rom_titles() {
    for (stem, key, id) in [
        ("Invincible Iron Man, The (USA, Europe)", "invincible iron man", 10770),
        ("Rayman - 10th Anniversary (USA)", "rayman 10th anniversary", 3325),
        ("Three-in-One Pack - Connect Four + Perfection + Trouble (USA)", "three in one pack connect four perfection trouble", 21686),
        ("Three-in-One Pack - Risk + Battleship + Clue (USA)", "three in one pack risk battleship clue", 18148),
        ("Three-in-One Pack - Sorry! + Aggravation + Scrabble Junior (USA)", "three in one pack sorry aggravation scrabble junior", 91944),
        ("Crash & Spyro Superpack - Spyro - Season of Ice + Crash Bandicoot - The Huge Adventure (USA)", "crash and spyro superpack spyro season of ice crash bandicoot the huge adventure", 18386),
        ("Pokemon - Ruby Version (USA, Europe) (Rev 2)", "pokemon ruby version", 2241),
        ("Oriental Blue - Ao no Tengai (Japan) [T-En]", "oriental blue ao no tengai", 30529),
        ("Mega Man Battle Network 6 - Cybeast Gregar (USA)", "mega man battle network 6 cybeast gregar", 6641),
        ("Tron 2.0 - Killer App (USA)", "tron 2 0 killer app", 3907),
        ("Yggdra Union - We'll Never Fight Alone (USA)", "yggdra union well never fight alone", 3817),
    ] {
        assert_eq!(source::title(stem), key, "{stem}");
        assert_eq!(source::known(key), Some(id));
    }
}

enum Reply {
    Page(&'static str),
    Network(&'static str),
    Unavailable(&'static str),
}

struct Fixture {
    calls: Vec<String>,
    replies: Vec<(&'static str, Reply)>,
}

impl Transport for Fixture {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, Error> {
        self.calls.push(url.into());
        match self.replies.iter().find(|(needle, _)| url.contains(needle)) {
            Some((_, Reply::Page(page))) => Ok(page.as_bytes().to_vec()),
            Some((_, Reply::Network(message))) => Err(Error::Network((*message).into())),
            Some((_, Reply::Unavailable(message))) => Err(Error::Unavailable((*message).into())),
            None => Ok(Vec::new()),
        }
    }
}

#[test]
fn resolve_tries_duplicate_candidates_and_short_circuits_searches() {
    let mut http = Fixture {
        calls: vec![],
        replies: vec![
            ("Duplicate%20Game", Reply::Page("<a href='/games/details/1-game'><h3>Duplicate Game</h3><p>Nintendo Game Boy Advance</p></a><a href='/games/details/2-game'><h3>Duplicate Game</h3><p>Nintendo Game Boy Advance</p></a>")),
            ("images/1", Reply::Unavailable("404")),
            ("images/2", Reply::Page("<a href='https://images.launchbox-app.com/cart.png' data-title='Game - Cart - Front Image (North America)'></a>")),
        ],
    };
    assert!(
        source::resolve(&mut http, &cart("Duplicate Game (USA)"), None)
            .unwrap()
            .ends_with("/cart.png")
    );
    assert!(http
        .calls
        .iter()
        .any(|call| call.ends_with("/games/images/1")));
    assert!(http
        .calls
        .iter()
        .any(|call| call.ends_with("/games/images/2")));

    assert_eq!(http.calls.len(), 3);
}

#[test]
fn resolve_spends_gallery_budget_on_distinct_candidates() {
    let mut http = Fixture {
        calls: vec![],
        replies: vec![
            ("Fallback%20Game", Reply::Page("<a href='/games/details/1-game'><h3>Fallback Game</h3><p>Nintendo Game Boy Advance</p></a><a href='/games/details/2-game'><h3>Fallback Game</h3><p>Nintendo Game Boy Advance</p></a>")),
            ("fallback%20game", Reply::Page("<a href='/games/details/1-game'><h3>Fallback Game</h3><p>Nintendo Game Boy Advance</p></a><a href='/games/details/2-game'><h3>Fallback Game</h3><p>Nintendo Game Boy Advance</p></a><a href='/games/details/3-game'><h3>Fallback Game</h3><p>Nintendo Game Boy Advance</p></a>")),
            ("images/1", Reply::Page("")),
            ("images/2", Reply::Page("")),
            ("images/3", Reply::Page("<a href='https://images.launchbox-app.com/cart.png' data-title='Game - Cart - Front Image (North America)'></a>")),
        ],
    };

    assert!(source::resolve(
        &mut http,
        &cart("fallback game (USA)"),
        Some("Fallback Game")
    )
    .unwrap()
    .ends_with("/cart.png"));
    assert_eq!(
        http.calls
            .iter()
            .filter(|call| call.ends_with("/games/images/1"))
            .count(),
        1
    );
    assert_eq!(
        http.calls
            .iter()
            .filter(|call| call.ends_with("/games/images/2"))
            .count(),
        1
    );
    assert!(http
        .calls
        .iter()
        .any(|call| call.ends_with("/games/images/3")));
}

#[test]
fn resolve_keeps_going_after_network_and_gallery_errors() {
    let mut http = Fixture {
        calls: vec![],
        replies: vec![
            ("Network%20Game%20Deluxe", Reply::Page("<a href='/games/details/1-game'><h3>Network Game Deluxe</h3><p>Nintendo Game Boy Advance</p></a>")),
            ("Network%20Game%20Original", Reply::Network("search failed")),
            ("network%20game%20deluxe", Reply::Page("<a href='/games/details/2-game'><h3>Network Game Deluxe</h3><p>Nintendo Game Boy Advance</p></a>")),
            ("images/1", Reply::Page("")),
            ("images/2", Reply::Page("<a href='https://images.launchbox-app.com/cart.png' data-title='Game - Cart - Front Image (North America)'></a>")),
        ],
    };
    assert!(source::resolve(
        &mut http,
        &cart("Network Game - Original (USA)"),
        Some("Network Game - Deluxe"),
    )
    .unwrap()
    .ends_with("/cart.png"));
    assert!(http
        .calls
        .iter()
        .any(|call| call.ends_with("/games/images/1")));
    assert!(http
        .calls
        .iter()
        .any(|call| call.ends_with("/games/images/2")));
}

#[test]
fn resolve_returns_network_when_no_art_is_found() {
    let mut http = Fixture {
        calls: vec![],
        replies: vec![("Network%20Only", Reply::Network("search failed"))],
    };
    assert!(matches!(
        source::resolve(&mut http, &cart("Network Only (USA)"), None),
        Err(Error::Network(_))
    ));
}

#[test]
fn bracketed_stem_never_retries_only_its_prefix() {
    let mut http = Fixture {
        calls: vec![],
        replies: vec![],
    };
    assert!(source::resolve(
        &mut http,
        &cart("Golden Sun - The Lost Age (USA) [!]"),
        None
    )
    .is_err());
    assert!(!http
        .calls
        .iter()
        .any(|call| call.ends_with("id=Golden%20Sun")));
}

#[test]
fn crop_produces_exact_rgb_png_and_rejects_bad_input() {
    for (platform, sizes) in [
        (
            Platform::Gba,
            &[
                (1000, 574),
                (600, 355),
                (473, 283),
                (800, 465),
                (300, 176),
                (320, 187),
            ][..],
        ),
        (Platform::Gb, &[(796, 906), (674, 759)][..]),
        (Platform::Gbc, &[(800, 916)][..]),
    ] {
        let (lw, lh) = label_size(platform);
        for (w, h) in sizes {
            let png = artwork::prepare(&image_bytes(*w, *h), platform).unwrap();
            assert_eq!(image::guess_format(&png).unwrap(), ImageFormat::Png);
            let im = image::load_from_memory(&png).unwrap().to_rgb8();
            assert_eq!(im.dimensions(), (lw, lh));
            assert_eq!(im.get_pixel(lw / 2, 0), &Rgb([255, 0, 0]));
            assert_eq!(im.get_pixel(lw / 2, lh - 1), &Rgb([0, 0, 255]));
        }
    }
    assert!(artwork::prepare(b"<html>Error</html>", Platform::Gba).is_err());
    assert!(artwork::prepare(&image_bytes(100, 100), Platform::Gba).is_err());
    assert!(artwork::prepare(&image_bytes(5000, 2), Platform::Gba).is_err());
    // A cart scan of the other shape is not this platform's cart.
    assert!(artwork::prepare(&image_bytes(1000, 574), Platform::Gb).is_err());
    assert!(artwork::prepare(&image_bytes(796, 906), Platform::Gba).is_err());
}

#[test]
fn special_gba_crop_uses_the_cartridge_area() {
    let inside = Rgb([12, 34, 56]);
    let outside = Rgb([210, 180, 140]);
    let image = RgbImage::from_fn(999, 925, |x, y| {
        if (100..900).contains(&x) && (468..883).contains(&y) {
            inside
        } else {
            outside
        }
    });
    let mut input = Cursor::new(Vec::new());
    image.write_to(&mut input, ImageFormat::Png).unwrap();

    let png = artwork::prepare(&input.into_inner(), Platform::Gba).unwrap();
    let output = image::load_from_memory(&png).unwrap().to_rgb8();
    let (width, height) = output.dimensions();
    for x in [20, width / 2, width - 21] {
        for y in [20, height / 2, height - 21] {
            assert_eq!(output.get_pixel(x, y), &inside, "sample at ({x}, {y})");
        }
    }
}

struct Fake {
    calls: Vec<String>,
    image: Vec<u8>,
}
impl Transport for Fake {
    fn get(&mut self, url: &str) -> Result<Vec<u8>, Error> {
        self.calls.push(url.into());
        if url.contains("/games/images/") {
            Ok(br#"<a href="https://images.launchbox-app.com/cart.png" data-title="Advance Wars - Cart - Front Image (North America)"></a>"#.to_vec())
        } else {
            Ok(self.image.clone())
        }
    }
}

#[test]
fn end_to_end_writes_exact_filename_and_skips_existing_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let cart = cart("Advance Wars (USA) (Rev 1)");
    let mut http = Fake {
        calls: vec![],
        image: image_bytes(600, 355),
    };
    let mut cache = HashMap::new();
    let file = prepare(&mut http, &mut cache, dir.path(), &cart, None).unwrap();
    assert_eq!(
        file,
        dir.path().join("Labels/GBA/Advance Wars (USA) (Rev 1).png")
    );
    let bytes = std::fs::read(&file).unwrap();
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 266);
    assert_eq!(http.calls.len(), 2);
    prepare(&mut http, &mut cache, dir.path(), &cart, None).unwrap();
    assert_eq!(http.calls.len(), 2);
    assert_eq!(std::fs::read(file).unwrap(), bytes);
}

#[test]
fn a_custom_label_wins_a_publish_race_and_no_partial_files_remain() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("label.png");
    std::fs::write(&file, b"custom").unwrap();
    publish(&file, b"download").unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"custom");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn corrupt_download_is_not_published_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let cart = cart("Advance Wars (USA)");
    let mut http = Fake {
        calls: vec![],
        image: b"truncated".to_vec(),
    };
    let mut cache = HashMap::new();
    assert!(prepare(&mut http, &mut cache, dir.path(), &cart, None).is_err());
    assert!(!dir.path().join("Labels").exists());
    http.image = image_bytes(600, 355);
    assert!(prepare(&mut http, &mut cache, dir.path(), &cart, None).is_ok());
    assert_eq!(
        http.calls.len(),
        3,
        "retry should reuse resolved source URL"
    );
}

#[test]
#[ignore = "live public artwork service; run manually"]
fn live_download() {
    let dir = tempfile::tempdir().unwrap();
    let mut downloader = Downloader::default();
    for name in [
        "Advance Wars (USA)",
        "Mario Kart - Super Circuit (USA)",
        "Lara Croft Tomb Raider - Legend (USA)",
        "Lara Croft Tomb Raider - The Prophecy (USA)",
        "Teenage Mutant Ninja Turtles (USA)",
        "Tom and Jerry - The Magic Ring (USA)",
    ] {
        let path = downloader.prepare(dir.path(), &cart(name)).unwrap();
        let image = image::open(path).unwrap();
        assert_eq!((image.width(), image.height()), (266, 138));
    }
}

#[test]
fn legacy_card_publish_is_complete_and_keeps_existing_labels() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("label.png");
    let make = |bytes: &[u8]| {
        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(bytes).unwrap();
        tmp.as_file().sync_all().unwrap();
        tmp
    };
    publish_legacy(make(b"first complete image"), &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"first complete image");
    publish_legacy(make(b"replacement"), &target).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"first complete image");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn verified_identity_selects_artwork_but_preserves_rom_filename() {
    let dir = tempfile::tempdir().unwrap();
    let cart = cart("Renamed by me (USA)");
    let mut http = Fake {
        calls: vec![],
        image: image_bytes(600, 355),
    };
    let file = prepare(
        &mut http,
        &mut HashMap::new(),
        dir.path(),
        &cart,
        Some("Advance Wars"),
    )
    .unwrap();
    assert!(http.calls[0].ends_with("/games/images/2367"));
    assert_eq!(file.file_name().unwrap(), "Renamed by me (USA).png");
    assert_eq!(
        source::title("Lara Croft Tomb Raider - Legend (USA)"),
        "tomb raider legend"
    );
    assert_eq!(
        source::title("Lara Croft Tomb Raider - The Prophecy (USA)"),
        "tomb raider the prophecy"
    );
}
