use std::sync::{mpsc, Arc};

use slot_achievements::{GameAchievementSnapshot, GameAchievementState};
use slot_gfx::{Compositor, Draw, TexId, OUT_H, OUT_W};
use slot_input::{Action, Btn};
use slot_ui::{text, CartFace};

use crate::session::Session;

pub const ROWS: usize = 5;
const DESCRIPTION_LINES: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub row: usize,
    pub top: usize,
    pub detail: bool,
    pub description_page: usize,
}

impl Selection {
    pub fn clamp(&mut self, len: usize) {
        self.row = self.row.min(len.saturating_sub(1));
        self.top = self.top.min(len.saturating_sub(ROWS));
        if self.row < self.top {
            self.top = self.row;
        }
        if self.row >= self.top + ROWS {
            self.top = self.row + 1 - ROWS;
        }
        if len == 0 {
            self.detail = false;
        }
    }

    pub fn input(&mut self, action: Action, len: usize) {
        if self.detail {
            match action {
                Action::GbaDown(Btn::Up) | Action::GbaDown(Btn::L1) => {
                    self.description_page = self.description_page.saturating_sub(1)
                }
                Action::GbaDown(Btn::Down) | Action::GbaDown(Btn::R1) => {
                    self.description_page = self.description_page.saturating_add(1)
                }
                _ => {}
            }
        } else {
            match action {
                Action::GbaDown(Btn::Up) => self.row = self.row.saturating_sub(1),
                Action::GbaDown(Btn::Down) => self.row = self.row.saturating_add(1),
                Action::GbaDown(Btn::L1) => self.row = self.row.saturating_sub(ROWS),
                Action::GbaDown(Btn::R1) => self.row = self.row.saturating_add(ROWS),
                Action::GbaDown(Btn::A) if len != 0 => {
                    self.detail = true;
                    self.description_page = 0;
                }
                _ => {}
            }
        }
        self.clamp(len);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ScreenKey {
    Picker(bool),
    Linked,
    Achievements(u64, u64, u64, Selection),
}

struct Request {
    serial: u64,
    key: ScreenKey,
    snapshot: Option<Arc<GameAchievementSnapshot>>,
}

struct Built {
    serial: u64,
    key: ScreenKey,
    face: CartFace,
    description_pages: usize,
}

pub(crate) struct Screen {
    requests: mpsc::Sender<Request>,
    built: mpsc::Receiver<Built>,
    snapshot: Option<Arc<GameAchievementSnapshot>>,
    snapshot_key: (u64, u64),
    wanted: Option<ScreenKey>,
    displayed: Option<ScreenKey>,
    serial: u64,
    waiting: bool,
    texture: Option<TexId>,
}

impl Screen {
    pub fn new() -> Self {
        let (requests, inbox) = mpsc::channel::<Request>();
        let (outbox, built) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("slot-ra-screen".into())
            .spawn(move || {
                while let Ok(mut request) = inbox.recv() {
                    while let Ok(newer) = inbox.try_recv() {
                        request = newer;
                    }
                    let (face, description_pages) =
                        raster(&request.key, request.snapshot.as_deref());
                    if outbox
                        .send(Built {
                            serial: request.serial,
                            key: request.key,
                            face,
                            description_pages,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        Self {
            requests,
            built,
            snapshot: None,
            snapshot_key: (0, 0),
            wanted: None,
            displayed: None,
            serial: 0,
            waiting: false,
            texture: None,
        }
    }

    fn view_key(
        &mut self,
        session: &Session,
        fetch: impl FnOnce(&Session) -> Option<Arc<GameAchievementSnapshot>>,
    ) -> Option<ScreenKey> {
        let app = session.app();
        if app.game_picker().is_none() && app.achievement_screen().is_none() {
            return None;
        }
        if self.wanted.is_none() {
            self.snapshot = None;
            self.snapshot_key = (0, 0);
            self.displayed = None;
        }
        if let Some(link) = app.game_picker() {
            return Some(ScreenKey::Picker(link));
        }
        if app.link_active() || app.link_player().is_some() {
            self.snapshot = None;
            return Some(ScreenKey::Linked);
        }
        let identity = session.achievement_snapshot_key();
        if identity != self.snapshot_key {
            self.snapshot = None;
            self.snapshot_key = identity;
        }
        if identity.0 != 0 {
            if let Some(snapshot) =
                fetch(session).filter(|snapshot| snapshot.generation == identity.0)
            {
                self.snapshot = Some(snapshot);
            }
        }
        if let Some(selection) = app.achievement_screen() {
            if self.snapshot.is_none()
                || (!app.achievement_platform_supported()
                    && self
                        .snapshot
                        .as_ref()
                        .is_some_and(|s| s.state != GameAchievementState::NotSupportedPlatform))
            {
                let account = session.achievement_account_state();
                let state = if !app.achievement_platform_supported() {
                    GameAchievementState::NotSupportedPlatform
                } else if account.busy {
                    GameAchievementState::SigningIn
                } else if !account.enabled {
                    GameAchievementState::Disabled
                } else if !account.signed_in {
                    GameAchievementState::SignedOut
                } else {
                    GameAchievementState::Loading
                };
                self.snapshot = Some(Arc::new(GameAchievementSnapshot::empty(
                    identity.0,
                    state,
                    app.seated_cart()
                        .map_or("ACHIEVEMENTS", |cart| cart.title.as_str())
                        .to_owned(),
                )));
            }
            let snapshot = self.snapshot.as_ref().unwrap();
            Some(ScreenKey::Achievements(
                identity.0,
                identity.1,
                snapshot.revision,
                selection,
            ))
        } else {
            None
        }
    }

    pub fn update(&mut self, session: &mut Session, compositor: &mut Compositor) {
        let Some(key) = self.view_key(session, Session::achievement_snapshot) else {
            self.wanted = None;
            return;
        };
        let key = Some(key);
        session
            .app_mut()
            .observe_achievement_count(self.snapshot.as_ref().map_or(0, |s| s.achievements.len()));
        if key != self.wanted {
            self.wanted = key.clone();
            self.serial = self.serial.wrapping_add(1);
        }
        while let Ok(built) = self.built.try_recv() {
            self.waiting = false;
            if built.serial != self.serial || Some(&built.key) != self.wanted.as_ref() {
                continue;
            }
            if let Some(texture) = self.texture {
                compositor.update_texture(texture, built.face.w, built.face.h, &built.face.rgba);
            } else {
                self.texture =
                    Some(compositor.create_texture(built.face.w, built.face.h, &built.face.rgba));
            }
            self.displayed = Some(built.key);
            session
                .app_mut()
                .observe_achievement_description_pages(built.description_pages);
        }
        if !self.waiting && self.wanted != self.displayed {
            if let Some(key) = self.wanted.clone() {
                self.waiting = self
                    .requests
                    .send(Request {
                        serial: self.serial,
                        key,
                        snapshot: self.snapshot.clone(),
                    })
                    .is_ok();
            }
        }
    }

    pub fn draw(&self, session: &Session, out: &mut Vec<Draw>) {
        if session.app().game_picker().is_none() && session.app().achievement_screen().is_none() {
            return;
        }
        out.push(Draw::Rect {
            x: 0.0,
            y: 0.0,
            w: OUT_W as f32,
            h: OUT_H as f32,
            colour: slot_ui::opening(),
        });
        let compatible = match (&self.displayed, &self.wanted) {
            (Some(ScreenKey::Picker(_)), Some(ScreenKey::Picker(_))) => true,
            (Some(ScreenKey::Linked), Some(ScreenKey::Linked)) => true,
            (
                Some(ScreenKey::Achievements(g1, e1, _, s1)),
                Some(ScreenKey::Achievements(g2, e2, _, s2)),
            ) => g1 == g2 && e1 == e2 && s1.detail == s2.detail,
            _ => false,
        };
        if let Some(texture) = self.texture.filter(|_| compatible) {
            out.push(Draw::Tex {
                x: 0.0,
                y: 0.0,
                w: OUT_W as f32,
                h: OUT_H as f32,
                tex: texture,
                alpha: 1.0,
            });
        }
    }
}

fn status(state: &GameAchievementState) -> &str {
    match state {
        GameAchievementState::Disabled => "ACHIEVEMENTS ARE OFF",
        GameAchievementState::SignedOut => "NOT SIGNED IN",
        GameAchievementState::SigningIn | GameAchievementState::Loading => "LOADING",
        GameAchievementState::Unrecognized => "GAME NOT RECOGNISED BY RETROACHIEVEMENTS",
        GameAchievementState::NoAchievements => "NO ACHIEVEMENTS FOR THIS GAME",
        GameAchievementState::Ready => "ONLINE",
        GameAchievementState::ServerBusy => "SERVER BUSY, RETRYING",
        GameAchievementState::Offline(_) => "OFFLINE",
        GameAchievementState::Error(message) => message,
        GameAchievementState::NotSupportedPlatform => "ONLY GBA GAMES SUPPORT ACHIEVEMENTS",
    }
}

fn sync_status(snapshot: &GameAchievementSnapshot) -> String {
    let state = status(&snapshot.state).to_uppercase();
    // The label wrapper splits on whitespace, so a newline would not break the line.
    match snapshot.unsynced_unlocks {
        0 => state,
        1 => format!("{state}, 1 UNLOCK WAITING TO SYNC"),
        n => format!("{state}, {n} UNLOCKS WAITING TO SYNC"),
    }
}

fn put_text(
    face: &mut CartFace,
    value: &str,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    lines: usize,
    px: f32,
) {
    let Some(font) = text::label_font() else {
        return;
    };
    let layout = text::fit_box(font, value, w as f32, h as f32, lines, px, 12.0);
    let mut rgba = vec![0; (w * h * 4) as usize];
    text::draw_centred(&mut rgba, w, h, &layout, [245, 245, 245]);
    stamp(face, &rgba, w, h, x, y);
}

fn stamp(face: &mut CartFace, rgba: &[u8], w: u32, h: u32, x: u32, y: u32) {
    for row in 0..h.min(face.h.saturating_sub(y)) {
        for col in 0..w.min(face.w.saturating_sub(x)) {
            let src = ((row * w + col) * 4) as usize;
            let dst = (((y + row) * face.w + x + col) * 4) as usize;
            let alpha = rgba[src + 3] as u32;
            for c in 0..3 {
                face.rgba[dst + c] = ((rgba[src + c] as u32 * alpha
                    + face.rgba[dst + c] as u32 * (255 - alpha))
                    / 255) as u8;
            }
            face.rgba[dst + 3] = face.rgba[dst + 3].max(rgba[src + 3]);
        }
    }
}

fn bar(face: &mut CartFace, y: u32, h: u32) {
    for row in y..(y + h).min(face.h) {
        for pixel in face.rgba[(row * face.w * 4) as usize..((row + 1) * face.w * 4) as usize]
            .chunks_exact_mut(4)
        {
            pixel.copy_from_slice(&[60, 60, 60, 235]);
        }
    }
}

fn list_title(title: &str) -> Option<text::Layout> {
    let font = text::label_font()?;
    let mut layout = text::fit(font, title, 440.0, 1, 22.0, 18.0);
    let whole = title
        .to_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let first = layout.lines.first().cloned().unwrap_or_default();
    if first != whole {
        let mut kept = first;
        while !kept.is_empty()
            && text::line_width(
                font,
                &format!("{}...", kept.trim_end()),
                layout.px,
                layout.tracking,
            ) > 440.0
        {
            kept.pop();
        }
        layout.lines = vec![format!("{}...", kept.trim_end())];
    }
    Some(layout)
}

fn unlock_time(seconds: u64) -> String {
    let Ok(seconds) = i64::try_from(seconds) else {
        return "UNLOCK TIME NOT AVAILABLE".into();
    };
    let (year, month, day) = slot_store::civil_from_days(seconds / 86400);
    format!(
        "UNLOCKED {year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds / 3600 % 24,
        seconds / 60 % 60
    )
}

fn description_lines(description: &str) -> Vec<String> {
    text::label_font()
        .map(|font| text::fit(font, description, 648.0, usize::MAX, 20.0, 20.0).lines)
        .unwrap_or_default()
}

fn raster(key: &ScreenKey, snapshot: Option<&GameAchievementSnapshot>) -> (CartFace, usize) {
    let mut face = CartFace {
        w: OUT_W,
        h: OUT_H,
        rgba: vec![0; (OUT_W * OUT_H * 4) as usize],
    };
    for pixel in face.rgba.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[18, 18, 18, 235]);
    }
    if let ScreenKey::Picker(link) = key {
        put_text(&mut face, "GAME MENU", 32, 70, 656, 50, 1, 30.0);
        bar(&mut face, if *link { 252 } else { 184 }, 60);
        put_text(&mut face, "ACHIEVEMENTS", 32, 184, 656, 60, 1, 30.0);
        put_text(&mut face, "LINK", 32, 252, 656, 60, 1, 30.0);
        put_text(
            &mut face,
            "UP / DOWN   A OPEN   B BACK",
            32,
            430,
            656,
            30,
            1,
            16.0,
        );
        return (face, 1);
    }
    if let ScreenKey::Linked = key {
        put_text(&mut face, "ACHIEVEMENTS", 32, 70, 656, 50, 1, 30.0);
        put_text(&mut face, "PAUSED WHILE LINKED", 32, 210, 656, 60, 1, 26.0);
        put_text(&mut face, "B DONE", 32, 430, 656, 30, 1, 16.0);
        return (face, 1);
    }
    let Some(snapshot) = snapshot else {
        return (face, 1);
    };
    let ScreenKey::Achievements(_, _, _, selection) = key else {
        unreachable!()
    };
    put_text(&mut face, &snapshot.game_title, 32, 10, 656, 38, 1, 26.0);
    let summary = &snapshot.summary;
    put_text(
        &mut face,
        &format!(
            "UNLOCKED {} / {}   POINTS {} / {}   SOFTCORE",
            summary.unlocked, summary.total, summary.points_earned, summary.points_total
        ),
        24,
        48,
        672,
        30,
        1,
        18.0,
    );
    put_text(&mut face, &sync_status(snapshot), 24, 80, 672, 34, 2, 18.0);
    if selection.detail {
        if let Some(a) = snapshot.achievements.get(selection.row) {
            put_text(&mut face, &a.title, 32, 118, 656, 48, 2, 24.0);
            let detail = format!(
                "{} POINTS  {}{}",
                a.points,
                if a.unlocked { "UNLOCKED" } else { "LOCKED" },
                if a.supported { "" } else { "  UNSUPPORTED" }
            );
            put_text(&mut face, &detail, 32, 166, 656, 26, 1, 16.0);
            let lines = description_lines(&a.description);
            let pages = lines.len().div_ceil(DESCRIPTION_LINES).max(1);
            let page = selection.description_page.min(pages - 1);
            // Preserve the already wrapped lines instead of feeding them through wrapping again.
            if text::label_font().is_some() {
                let layout = text::Layout {
                    lines: lines
                        .into_iter()
                        .skip(page * DESCRIPTION_LINES)
                        .take(DESCRIPTION_LINES)
                        .collect(),
                    px: 20.0,
                    tracking: 2.0,
                };
                let mut rgba = vec![0; 648 * 230 * 4];
                text::draw_centred(&mut rgba, 648, 230, &layout, [245, 245, 245]);
                stamp(&mut face, &rgba, 648, 230, 36, 194);
            }
            let time = a.unlocked_at.map_or_else(
                || "UNLOCK TIME NOT AVAILABLE".into(),
                |seconds| unlock_time(seconds),
            );
            put_text(&mut face, &time, 32, 420, 656, 24, 1, 14.0);
            put_text(
                &mut face,
                &format!("L1 / R1 PAGE {} / {}   B BACK", page + 1, pages),
                32,
                448,
                656,
                26,
                1,
                16.0,
            );
            return (face, pages);
        }
    }
    for (slot, a) in snapshot
        .achievements
        .iter()
        .skip(selection.top)
        .take(ROWS)
        .enumerate()
    {
        let y = 120 + slot as u32 * 60;
        if selection.top + slot == selection.row {
            bar(&mut face, y, 56);
        }
        let mut badge = a
            .badge_path
            .as_deref()
            .and_then(slot_achievements::load_badge)
            .map(|image| {
                let mut rgba = vec![0; 40 * 40 * 4];
                for by in 0..40 {
                    for bx in 0..40 {
                        let src = (((by * image.height / 40) * image.width + bx * image.width / 40)
                            * 4) as usize;
                        let dst = ((by * 40 + bx) * 4) as usize;
                        rgba[dst..dst + 4].copy_from_slice(&image.rgba[src..src + 4]);
                    }
                }
                rgba
            })
            .unwrap_or_else(|| [125, 125, 125, 255].repeat(40 * 40));
        if !a.unlocked {
            for pixel in badge.chunks_exact_mut(4) {
                let grey = ((pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32) / 6) as u8;
                pixel[..3].fill(grey);
            }
        }
        stamp(&mut face, &badge, 40, 40, 24, y + 8);
        if a.unlocked {
            let mut check = vec![0; 24 * 24 * 4];
            for x in 0..18usize {
                let y = if x < 6 { 10 + x } else { 21 - x };
                for thickness in 0..3 {
                    let dst = ((y + thickness) * 24 + x + 2) * 4;
                    check[dst..dst + 4].copy_from_slice(&[245, 245, 245, 255]);
                }
            }
            stamp(&mut face, &check, 24, 24, 70, y + 16);
        }
        if let Some(layout) = list_title(&a.title) {
            let mut rgba = vec![0; 440 * 30 * 4];
            text::draw_centred(&mut rgba, 440, 30, &layout, [245, 245, 245]);
            stamp(&mut face, &rgba, 440, 30, 100, y + 2);
        }
        let progress = if !a.supported {
            "UNSUPPORTED".into()
        } else {
            a.progress
                .map_or(String::new(), |(value, target)| format!("{value}/{target}"))
        };
        put_text(&mut face, &progress, 100, y + 32, 440, 22, 1, 14.0);
        put_text(
            &mut face,
            &format!("{} PTS", a.points),
            552,
            y + 8,
            144,
            40,
            1,
            20.0,
        );
    }
    put_text(
        &mut face,
        "UP / DOWN   L1 / R1 PAGE   A DETAILS   B DONE",
        24,
        440,
        672,
        32,
        1,
        16.0,
    );
    (face, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_session() -> (tempfile::TempDir, Session) {
        let root = tempfile::tempdir().unwrap();
        crate::root::ensure(root.path());
        std::fs::write(root.path().join("Games/GBA/Example.gba"), vec![0; 256]).unwrap();
        slot_store::write_slot_state(
            root.path(),
            &slot_store::SlotState {
                cart: Some("Example".into()),
                clock_set: true,
                ..Default::default()
            },
        )
        .unwrap();
        let mut session = Session::boot(root.path().into());
        session.app_mut().on_core_ready();
        for _ in 0..120 {
            session.app_mut().update(1.0 / 60.0);
        }
        assert!(matches!(
            session.app().phase(),
            crate::app::Phase::Playing { .. }
        ));
        (root, session)
    }

    #[test]
    fn closed_viewer_does_not_fetch_or_retire_the_cached_catalog() {
        let (_root, mut session) = playing_session();
        let mut screen = Screen::new();
        let stale = Arc::new(GameAchievementSnapshot::empty(
            42,
            GameAchievementState::Ready,
            "Previous game".into(),
        ));
        screen.snapshot = Some(stale.clone());
        screen.snapshot_key = (42, 3);
        screen.displayed = Some(ScreenKey::Achievements(42, 3, 0, Selection::default()));
        for _ in 0..120 {
            assert_eq!(
                screen.view_key(&session, |_| panic!("closed viewer fetched a snapshot")),
                None
            );
            assert_eq!(screen.snapshot_key, (42, 3));
            assert_eq!(Arc::strong_count(&stale), 2);
        }
        session.app_mut().apply(Action::GameMenu);
        assert_eq!(
            screen.view_key(&session, |_| panic!("picker fetched a snapshot")),
            Some(ScreenKey::Picker(false))
        );
        assert!(screen.snapshot.is_none());
        assert!(screen.displayed.is_none());
        assert_eq!(screen.snapshot_key, (0, 0));
        assert_eq!(Arc::strong_count(&stale), 1);
        screen.wanted = Some(ScreenKey::Picker(false));
        session.app_mut().apply(Action::GbaDown(Btn::A));
        assert!(matches!(
            screen.view_key(&session, Session::achievement_snapshot),
            Some(ScreenKey::Achievements(..))
        ));
        assert_eq!(
            screen.snapshot.as_ref().unwrap().game_title,
            session.app().seated_cart().unwrap().title
        );
        assert_ne!(
            screen.snapshot.as_ref().unwrap().game_title,
            "Previous game"
        );
    }

    #[test]
    fn linked_viewer_shows_paused_instead_of_a_ready_catalog() {
        let (_root, mut session) = playing_session();
        session.app_mut().begin_link(0);
        session.app_mut().apply(Action::GameMenu);
        session.app_mut().apply(Action::GbaDown(Btn::A));
        assert!(session.app().achievement_screen().is_some());
        let mut screen = Screen::new();
        screen.wanted = Some(ScreenKey::Picker(false));
        screen.snapshot = Some(Arc::new(GameAchievementSnapshot::empty(
            42,
            GameAchievementState::Ready,
            "Previous game".into(),
        )));
        assert_eq!(
            screen.view_key(&session, |_| panic!("linked viewer fetched a catalog")),
            Some(ScreenKey::Linked)
        );
        assert!(screen.snapshot.is_none());
        let (face, pages) = raster(&ScreenKey::Linked, None);
        assert_eq!(pages, 1);
        assert!(
            face.rgba[(210 * OUT_W * 4) as usize..(270 * OUT_W * 4) as usize]
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 150)
        );
    }

    #[test]
    fn paging_and_empty_lists_stay_in_bounds() {
        let mut selection = Selection::default();
        selection.input(Action::GbaDown(Btn::R1), 12);
        assert_eq!((selection.row, selection.top), (5, 1));
        for _ in 0..10 {
            selection.input(Action::GbaDown(Btn::R1), 12);
        }
        assert_eq!((selection.row, selection.top), (11, 7));
        selection.input(Action::GbaDown(Btn::L1), 12);
        assert_eq!(selection.row, 6);
        selection.clamp(0);
        assert_eq!((selection.row, selection.top), (0, 0));
        selection.input(Action::GbaDown(Btn::A), 0);
        assert!(!selection.detail);
    }

    #[test]
    fn long_descriptions_preserve_every_word_across_pages() {
        let description =
            "A very long achievement description with every word retained. ".repeat(100);
        let lines = description_lines(&description);
        assert!(lines.len() > DESCRIPTION_LINES);
        assert_eq!(
            lines.join(" "),
            description
                .to_uppercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
    }

    #[test]
    fn unlock_times_are_readable_utc_and_statuses_use_plain_words() {
        assert_eq!(unlock_time(0), "UNLOCKED 1970-01-01 00:00 UTC");
        assert_eq!(unlock_time(951827696), "UNLOCKED 2000-02-29 12:34 UTC");
        assert_eq!(
            status(&GameAchievementState::Disabled),
            "ACHIEVEMENTS ARE OFF"
        );
        assert_eq!(status(&GameAchievementState::SignedOut), "NOT SIGNED IN");
        assert_eq!(status(&GameAchievementState::Ready), "ONLINE");
        assert_eq!(
            status(&GameAchievementState::ServerBusy),
            "SERVER BUSY, RETRYING"
        );
        assert_eq!(
            status(&GameAchievementState::Unrecognized),
            "GAME NOT RECOGNISED BY RETROACHIEVEMENTS"
        );
        assert_eq!(
            status(&GameAchievementState::NoAchievements),
            "NO ACHIEVEMENTS FOR THIS GAME"
        );
        assert_eq!(
            status(&GameAchievementState::Offline("details".into())),
            "OFFLINE"
        );
        assert_eq!(
            status(&GameAchievementState::NotSupportedPlatform),
            "ONLY GBA GAMES SUPPORT ACHIEVEMENTS"
        );
        let mut snapshot =
            GameAchievementSnapshot::empty(1, GameAchievementState::Ready, "Test".into());
        assert_eq!(sync_status(&snapshot), "ONLINE");
        snapshot.unsynced_unlocks = 2;
        assert_eq!(sync_status(&snapshot), "ONLINE, 2 UNLOCKS WAITING TO SYNC");
        snapshot.state = GameAchievementState::ServerBusy;
        assert_eq!(
            sync_status(&snapshot),
            "SERVER BUSY, RETRYING, 2 UNLOCKS WAITING TO SYNC"
        );
        snapshot.state = GameAchievementState::Offline(String::new());
        assert_eq!(sync_status(&snapshot), "OFFLINE, 2 UNLOCKS WAITING TO SYNC");
        snapshot.unsynced_unlocks = 1;
        assert_eq!(sync_status(&snapshot), "OFFLINE, 1 UNLOCK WAITING TO SYNC");
        snapshot.state = GameAchievementState::Error("Sign in again".into());
        snapshot.unsynced_unlocks = 0;
        assert_eq!(sync_status(&snapshot), "SIGN IN AGAIN");
    }

    #[test]
    #[ignore = "writes achievement screen previews to SCRATCH_PNG_DIR"]
    fn render_achievement_screen_previews() {
        let output =
            std::path::PathBuf::from(std::env::var_os("SCRATCH_PNG_DIR").expect("SCRATCH_PNG_DIR"));
        std::fs::create_dir_all(&output).unwrap();
        let mut snapshot = GameAchievementSnapshot::empty(
            1,
            GameAchievementState::Ready,
            "POKEMON EMERALD".into(),
        );
        snapshot.achievements = (0..40).map(|id| slot_achievements::AchievementView {
            id,
            title: if id == 2 { "An unusually long achievement title that still fits on the list".into() } else { format!("Achievement number {}", id+1) },
            description: "Catch a Pokemon in every area and complete the adventure. This description remains available in full when it requires several pages. ".repeat(8),
            points: 10,
            unlocked: id < 2 || (5..15).contains(&id),
            unlocked_at: (id < 2 || (5..15).contains(&id)).then_some(951827696),
            badge_path: None,
            progress: (id == 3).then_some((12,50)),
            supported: id != 4,
        }).collect();
        snapshot.summary = slot_achievements::AchievementSummary {
            total: 40,
            unlocked: 12,
            points_total: 400,
            points_earned: 120,
            unsupported: 1,
        };
        for (name, key) in [
            ("root", ScreenKey::Picker(false)),
            (
                "list",
                ScreenKey::Achievements(1, 0, 0, Selection::default()),
            ),
            (
                "detail",
                ScreenKey::Achievements(
                    1,
                    0,
                    0,
                    Selection {
                        detail: true,
                        ..Default::default()
                    },
                ),
            ),
        ] {
            let (face, _) = raster(&key, Some(&snapshot));
            let file = std::fs::File::create(output.join(format!("{name}.png"))).unwrap();
            let mut encoder = png::Encoder::new(file, face.w, face.h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&face.rgba)
                .unwrap();
        }
    }

    #[test]
    fn description_pages_fit_and_list_titles_ellipsize_at_readable_type() {
        let font = text::label_font().unwrap();
        assert!(
            font.horizontal_line_metrics(20.0).unwrap().new_line_size * DESCRIPTION_LINES as f32
                <= 230.0
        );
        let layout = list_title(&"An unusually long title ".repeat(20)).unwrap();
        assert!(layout.px >= 18.0);
        assert!(layout.lines[0].ends_with("..."));
        assert!(text::line_width(font, &layout.lines[0], layout.px, layout.tracking) <= 440.0);
    }
}
