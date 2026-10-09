use slot_store::{parse_stamp, FF_SPEEDS};
use slot_ui::{
    date_time_text, quick_caret_face, quick_label_face, quick_value_face, ClockPicker, QuickRow,
    QuickValue, UndoFace, MENU_PAD,
};

fn inked(f: &UndoFace, x: u32, y: u32) -> bool {
    f.rgba[((y * f.w + x) * 4 + 3) as usize] > 128
}

fn ink_columns(f: &UndoFace) -> (u32, u32) {
    let cols: Vec<u32> = (0..f.w)
        .filter(|&x| (0..f.h).any(|y| inked(f, x, y)))
        .collect();
    (
        *cols.first().expect("an empty face"),
        *cols.last().expect("an empty face"),
    )
}

#[test]
fn the_type_sits_exactly_menu_pad_in_from_both_sides_of_its_face() {
    for f in [
        quick_label_face(QuickRow::FastForwardSound),
        quick_label_face(QuickRow::Rumble),
        quick_value_face("Off", false),
        quick_value_face(QuickValue::Speed6.text(), true),
        quick_value_face("SEP 15 16:35", true),
    ] {
        let (first, last) = ink_columns(&f);
        let right = f.w - 1 - MENU_PAD;
        assert!(
            (MENU_PAD..MENU_PAD + 4).contains(&first),
            "ink starts at {first} of {}",
            f.w
        );
        assert!(
            (right - 4..=right).contains(&last),
            "ink ends at {last} of {}",
            f.w
        );
    }
}

#[test]
fn a_long_label_is_set_as_large_as_a_short_one() {
    let tall = |f: &UndoFace| {
        (0..f.h)
            .filter(|&y| (0..f.w).any(|x| inked(f, x, y)))
            .count()
    };
    let long = tall(&quick_label_face(QuickRow::FastForwardSound));
    let short = tall(&quick_label_face(QuickRow::Rumble));
    assert!(long + 1 >= short, "{long} rows of ink against {short}");
}

#[test]
fn the_fast_forward_row_offers_the_four_ceilings_the_card_can_hold() {
    assert_eq!(FF_SPEEDS, [2, 3, 4, 6]);
    assert_eq!(
        FF_SPEEDS.map(QuickValue::speed),
        [
            Some(QuickValue::Speed2),
            Some(QuickValue::Speed3),
            Some(QuickValue::Speed4),
            Some(QuickValue::Speed6),
        ]
    );
    for other in [1, 5, 7, 9, 16, 28, 255] {
        assert_eq!(
            QuickValue::speed(other),
            None,
            "{other}x is not a value the row has"
        );
    }
}

#[test]
fn only_the_shader_row_has_a_presentation_scale_note() {
    for row in QuickRow::ALL {
        assert_eq!(
            row.note(),
            if row == QuickRow::Shader {
                Some("3X INTEGER")
            } else {
                None
            }
        );
    }
}

#[test]
fn the_rows_run_in_the_order_the_user_chose() {
    assert_eq!(QuickRow::Shader.index(), 3);
    assert_eq!(QuickRow::Shader.down(), QuickRow::ShowFps);
    assert_eq!(QuickRow::ShowFps.label(), "Show FPS");
    assert!(!QuickRow::ShowFps.opens());
    let labels = QuickRow::ALL.map(QuickRow::label);
    assert_eq!(
        labels,
        [
            "Fast Forward",
            "Fast Forward Sound",
            "Colour Correction",
            "Shader",
            "Show FPS",
            "Rumble",
            "Home Wi-Fi",
            "Wi-Fi Networks",
            "RetroAchievements",
            "12-Hour Clock",
            "Date & Time",
            "About"
        ]
    );
    let opens: Vec<QuickRow> = QuickRow::ALL.into_iter().filter(|r| r.opens()).collect();
    assert_eq!(
        opens,
        [
            QuickRow::WifiNetworks,
            QuickRow::RetroAchievements,
            QuickRow::DateTime,
            QuickRow::About
        ]
    );
}

#[test]
fn the_values_read_as_the_menu_prints_them() {
    assert_eq!(
        QuickValue::ALL.map(QuickValue::text),
        ["2×", "3×", "4×", "6×", "On", "Off", "GBA", "Auto"]
    );
    assert_eq!(QuickValue::flag(true), QuickValue::On);
    assert_eq!(QuickValue::flag(false), QuickValue::Off);
}

#[test]
fn the_date_and_time_read_as_a_month_a_day_and_the_carousels_24_hour_clock() {
    let at = |stamp: &str| date_time_text(parse_stamp(stamp).expect("a stamp"));
    assert_eq!(at("2026-09-15_16-35-00"), "SEP 15 16:35");
    assert_eq!(at("2027-01-05_04-07-59"), "JAN 5 04:07");
}

#[test]
fn a_picker_for_a_set_clock_starts_at_the_local_time_and_its_offset() {
    let utc = parse_stamp("2026-09-15_21-35-42").expect("a stamp");
    let p = ClockPicker::local(utc, -300);
    assert_eq!(p.offset_min(), -300);
    assert_eq!(
        p.secs(),
        utc - 42,
        "the picker does not show the minute it was opened in"
    );
    assert!(p.text().starts_with("2026-09-15 16:35"), "{}", p.text());
}

#[test]
fn a_value_is_grey_until_its_row_is_in_hand() {
    let inkiest = |lit| {
        let f = quick_value_face("4×", lit);
        f.rgba
            .chunks(4)
            .max_by_key(|p| p[3])
            .map(|p| [p[0], p[1], p[2]])
            .expect("an empty face")
    };
    assert_eq!(inkiest(true), [0xf6, 0xf4, 0xef]);
    assert_eq!(inkiest(false), [0x9a, 0x9a, 0xa4]);
}

#[test]
fn the_arrows_are_faces_the_height_of_a_value() {
    for right in [false, true] {
        let caret = quick_caret_face(right);
        assert!(
            caret.w > 0 && caret.rgba.chunks(4).any(|p| p[3] > 0),
            "an empty arrow"
        );
        assert_eq!(caret.h, quick_value_face("On", true).h);
    }
}

#[test]
fn scrolling_keeps_every_selection_and_its_bar_clear_of_the_legend() {
    use slot_ui::{quick_window, Draw, QuickMenu, QUICK_PITCH, QUICK_ROWS, QUICK_TOP};
    for row in QuickRow::ALL {
        let top = quick_window(row);
        assert!(top <= row.index() && row.index() < top + QUICK_ROWS);
        assert!(top + QUICK_ROWS <= QuickRow::ALL.len());
        let mut draws = Vec::new();
        QuickMenu {
            row,
            values: [None; QuickRow::ALL.len()],
            carets: [false; 2],
            clock: None,
            shader: None,
            faces: None,
        }
        .draw(&mut draws);
        let Draw::Rect { y, h, .. } = draws[1] else {
            panic!("no selection bar")
        };
        assert!(y >= 40.0 && y + h < 427.0);
    }
    assert!(QUICK_TOP + QUICK_ROWS as f32 * QUICK_PITCH <= 427.0);
    assert_eq!(quick_window(QuickRow::ALL[0]), 0);
    assert_eq!(
        quick_window(*QuickRow::ALL.last().unwrap()),
        QuickRow::ALL.len() - QUICK_ROWS
    );
}

#[test]
fn scroll_chevrons_show_only_hidden_rows_and_clear_text_and_legend() {
    use slot_ui::{quick_window, Draw, QuickMenu, QUICK_PITCH, QUICK_ROWS, QUICK_TOP};
    for row in QuickRow::ALL {
        let mut draws = Vec::new();
        QuickMenu {
            row,
            values: [None; QuickRow::ALL.len()],
            carets: [false; 2],
            clock: None,
            shader: None,
            faces: None,
        }
        .draw(&mut draws);
        let hints: Vec<_> = draws[2..]
            .iter()
            .map(|d| match d {
                Draw::Rect { y, h, .. } => (*y, *h),
                _ => panic!("unexpected hint"),
            })
            .collect();
        assert_eq!(
            hints.iter().any(|(y, _)| *y < QUICK_TOP),
            quick_window(row) > 0
        );
        let bottom = QUICK_TOP + QUICK_PITCH * QUICK_ROWS as f32;
        assert_eq!(
            hints.iter().any(|(y, _)| *y >= bottom),
            quick_window(row) + QUICK_ROWS < QuickRow::ALL.len()
        );
        assert!(hints
            .iter()
            .all(|(y, h)| (*y + *h < QUICK_TOP || *y >= bottom) && *y + *h <= 427.0));
    }
}

#[test]
fn shader_keeps_the_standard_label_and_a_dim_inline_note_on_its_baseline() {
    let standard = quick_value_face("Shader", true);
    let shader = quick_label_face(QuickRow::Shader);
    assert_eq!(shader.h, standard.h);
    assert!(shader.w > standard.w);
    for y in 0..standard.h as usize {
        let src = y * standard.w as usize * 4;
        let dst = y * shader.w as usize * 4;
        assert_eq!(
            &shader.rgba[dst..dst + standard.w as usize * 4],
            &standard.rgba[src..src + standard.w as usize * 4],
            "standard label moved or changed on scanline {y}"
        );
    }
    let font = slot_ui::text::label_font().unwrap();
    let metrics = font.horizontal_line_metrics(30.0).unwrap();
    let baseline = (standard.h as f32 - metrics.new_line_size) / 2.0 + metrics.ascent;
    let mut pen = standard.w as f32;
    let mut expected = vec![0u8; shader.rgba.len()];
    for ch in "3X INTEGER".chars() {
        let (m, cov) = font.rasterize(ch, 12.0);
        let x = (pen + m.xmin as f32).round() as usize;
        let y = (baseline - (m.height as f32 + m.ymin as f32)).round() as usize;
        for gy in 0..m.height {
            for gx in 0..m.width {
                let a = cov[gy * m.width + gx];
                if a > 0 {
                    let at = ((y + gy) * shader.w as usize + x + gx) * 4;
                    expected[at..at + 4].copy_from_slice(&[0x9a, 0x9a, 0xa4, a]);
                }
            }
        }
        pen += m.advance_width;
    }
    let mut note_pixels = 0;
    for y in 0..shader.h as usize {
        let start = (y * shader.w as usize + standard.w as usize) * 4;
        let end = (y + 1) * shader.w as usize * 4;
        assert_eq!(&shader.rgba[start..end], &expected[start..end]);
        note_pixels += shader.rgba[start..end]
            .chunks(4)
            .filter(|p| p[3] > 0)
            .count();
    }
    assert!(note_pixels > 0);
    assert!(
        shader.w - 2 * MENU_PAD <= 328,
        "inline note exceeds the label column"
    );
}

#[test]
fn rumble_label_pixels_stay_at_the_previous_output() {
    let face = quick_label_face(QuickRow::Rumble);
    let hash = face.rgba.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    });
    assert_eq!((face.w, face.h), (176, 40));
    assert_eq!(hash, 0x3c079a9db557b8fe);
    assert_eq!(face.rgba, quick_value_face("Rumble", true).rgba);
}
