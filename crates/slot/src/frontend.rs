use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use slot_gfx::{Compositor, Draw, ShaderChoice, TexId, OUT_H, OUT_W};
use slot_input::{InputSource, Millis};
use slot_power::{Platform, Power};
use slot_store::{format_stamp, SHADER_LCD, SHADER_OFF};
use slot_ui::{
    arrows_hint_face, badge_face, cart_face, cart_shadow, chip_face, chip_shadow_face,
    gb_cart_shadow, hint_face, icon_face, menu_face, photo_face, quick_caret_face,
    quick_label_face, quick_legend_faces, quick_value_face, set_clock_hint_face, socket_face,
    sticker_face, title_face, toast_face, wallpaper_face, word_face, GbShell, Icon, LinkBadge,
    QuickMenuFaces, QuickRow, QuickValue, StickerFields, Toast, UndoFace, ALERT_PX, BOLT_PX,
    HUD_ICON_PX, HUD_INK, LEGEND,
};
use slot_ui::{cheat_label_face, cheat_legend_faces, date_time_text_as, hhmm_as, CHEAT_ROWS};

use crate::app::{App, LinkRow, Phase};
use crate::build_info::Build;
use crate::face_builder::FaceBuilder;
use crate::link_art_builder::LinkArtBuilder;
use crate::link_screen::{LinkSprites, Sprite};
use crate::link_start::{LinkFail, LinkStep};
use crate::session::Session;
use crate::wallpaper;

const DOZE_TIMEOUT: Duration = Duration::from_secs(180);
const EXIT_TIMEOUT: Duration = Duration::from_secs(8);

const ALERT_INK: [u8; 3] = [0xf0, 0xb4, 0x3c];

pub struct Frontend {
    labels: crate::labels::Labels,
    achievements: crate::achievement_ui::Notifications,
    session: Session,
    exit_saved: Option<bool>,
    start: Instant,
    last: Instant,
    draws: Vec<Draw>,
    polaroid_texes: Vec<TexId>,
    title_tex: Option<TexId>,
    faces: FaceBuilder,
    link_art: LinkArtBuilder,
    link_art_done: bool,
    core_asked: Option<String>,
    core_board_tex: Option<TexId>,
    core_lid_tex: Option<TexId>,
    core_built: Option<String>,
    undo_tex: Option<TexId>,
    switcher: Switcher,
    clocks: Clocks,
    about: AboutFace,
    quick_clock: QuickClock,
    /// Shader's value in the quick menu. The same shape as the clock's: a line of menu type in
    /// both inks and the text it was built for.
    quick_shader: QuickClock,
    wifi_shown: Option<crate::wifi::WifiScreen>,
    wifi_tex: Option<TexId>,
    account_tex: Option<TexId>,
    account_shown: Option<u64>,
    cheats: CheatFaces,
    shader_params: ParameterFaces,
}

/// The cheat list's faces: one texture per window row, reused as the list scrolls, with which
/// list and which cheat each was last built for; and the count over the rows.
#[derive(Default)]
struct ParameterFaces {
    rows: Vec<[Option<TexId>; 2]>,
    shown: Vec<String>,
    generation: Option<u64>,
}

impl ParameterFaces {
    fn prepare(&mut self, app: &App) -> bool {
        if !app.shader_params_open() {
            self.shown.fill(String::new());
            return false;
        }
        let generation = app.shader_params_generation();
        if self.generation != Some(generation) {
            self.shown.fill(String::new());
            self.generation = Some(generation);
        }
        true
    }
}

#[derive(Default)]
struct CheatFaces {
    rows: Vec<Option<TexId>>,
    built: Vec<Option<(u64, usize)>>,
    count: Option<TexId>,
    counted: String,
}

#[derive(Default)]
struct QuickClock {
    dim: Option<TexId>,
    lit: Option<TexId>,
    shown: String,
}

#[derive(Default)]
struct AboutFace {
    tex: Option<TexId>,
    battery: Option<u8>,
}

#[derive(Default)]
struct Clocks {
    line: Option<TexId>,
    hint: Option<TexId>,
    shelf: Option<TexId>,
    picked: Option<String>,
    shown: String,
    battery: String,
    battery_tex: Option<TexId>,
    platform: String,
    platform_tex: Option<TexId>,
}

#[derive(Default)]
struct Switcher {
    open: bool,
    titled: Option<String>,
}

impl Frontend {
    pub fn boot(platform: Box<dyn Platform>) -> Self {
        let now = Instant::now();
        let root = platform.root().to_path_buf();
        let mut session = Session::boot(root.clone());
        let labels = crate::labels::Labels::spawn(root, session.app().carts().cloned().collect());
        session
            .app_mut()
            .set_power(Power::new(platform, DOZE_TIMEOUT));
        Frontend {
            labels,
            achievements: crate::achievement_ui::Notifications::new(),
            session,
            exit_saved: None,
            start: now,
            last: now,
            draws: Vec::new(),
            polaroid_texes: Vec::new(),
            title_tex: None,
            faces: FaceBuilder::spawn(),
            link_art: LinkArtBuilder::spawn(),
            link_art_done: false,
            core_asked: None,
            core_board_tex: None,
            core_lid_tex: None,
            core_built: None,
            undo_tex: None,
            switcher: Switcher::default(),
            clocks: Clocks::default(),
            about: AboutFace::default(),
            quick_clock: QuickClock::default(),
            quick_shader: QuickClock::default(),
            account_tex: None,
            account_shown: None,
            wifi_shown: None,
            wifi_tex: None,
            shader_params: ParameterFaces {
                rows: vec![[None; 2]; CHEAT_ROWS],
                shown: vec![String::new(); CHEAT_ROWS],
                ..ParameterFaces::default()
            },
            cheats: CheatFaces {
                rows: vec![None; CHEAT_ROWS],
                built: vec![None; CHEAT_ROWS],
                ..CheatFaces::default()
            },
        }
    }

    /// Initial cart faces, the HUD glyphs and the key caps. All of it
    pub fn upload_faces(&mut self, compositor: &mut Compositor) {
        let faces = self
            .session
            .app()
            .carts()
            .map(|c| {
                let f = cart_face(c);
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        self.session.app_mut().set_faces(faces);
        let star = slot_ui::favorite_star_face();
        let star = compositor.create_texture(star.w, star.h, &star.rgba);
        self.session.app_mut().set_favorite_star(star);
        let empty = word_face(crate::app::EMPTY_FAVORITES_TEXT);
        let empty_id = compositor.create_texture(empty.w, empty.h, &empty.rgba);
        self.session
            .app_mut()
            .set_empty_favorites_face(empty_id, empty.w);
        let icons = Icon::ALL
            .iter()
            .map(|i| {
                let f = icon_face(*i, HUD_ICON_PX, HUD_INK);
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        self.session.app_mut().set_icon_faces(icons);
        let link_badges = LinkBadge::FACES
            .iter()
            .map(|b| {
                let (badge, ink) = (
                    b.badge().expect("a face has a glyph"),
                    b.colour().expect("and a colour"),
                );
                let f = badge_face(badge, HUD_ICON_PX, ink);
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        self.session.app_mut().set_link_badge_faces(link_badges);
        let alert = icon_face(Icon::Alert, ALERT_PX, ALERT_INK);
        let alert = compositor.create_texture(alert.w, alert.h, &alert.rgba);
        self.session.app_mut().set_alert_face(alert);
        // Upload before shutdown, while the GPU is available.
        let f = menu_face("Powering Down");
        let face = (compositor.create_texture(f.w, f.h, &f.rgba), f.w, f.h);
        self.session.app_mut().set_shutdown_face(face);
        // legend. At boot, so moving through the menu or changing a
        let mut up = |f: UndoFace| (compositor.create_texture(f.w, f.h, &f.rgba), f.w, f.h);
        let labels = QuickRow::ALL
            .iter()
            .map(|r| up(quick_label_face(*r)))
            .collect();
        let values = QuickValue::ALL
            .iter()
            .map(|v| [false, true].map(|lit| up(quick_value_face(v.text(), lit))))
            .collect();
        let carets = [false, true].map(|right| up(quick_caret_face(right)));
        let legend = quick_legend_faces().map(|f| {
            let (tex, w, _) = up(f);
            (tex, w)
        });
        self.session.app_mut().set_quick_menu_faces(QuickMenuFaces {
            labels,
            values,
            carets,
            legend,
        });
        let parameter_legend = slot_ui::parameter_legend_faces().map(|f| {
            let (tex, w, _) = up(f);
            (tex, w)
        });
        self.session
            .app_mut()
            .set_parameter_legend(parameter_legend);
        let cheat_legend = cheat_legend_faces().map(|f| {
            let (tex, w, _) = up(f);
            (tex, w)
        });
        self.session.app_mut().set_cheat_legend_faces(cheat_legend);
        // The open cart's parts that never change: each socket, the chip seated in each, the
        // blank chip in flight and its shadow, in `Core::ALL` order. At boot like the power
        // menu's rows, so the first frame of a lid coming off is not spent in a rasteriser.
        let sockets = slot_store::Core::ALL
            .iter()
            .map(|c| {
                let f = socket_face(*c);
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        let chips = slot_store::Core::ALL
            .iter()
            .map(|c| {
                let f = chip_face(Some(*c));
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        let blank = chip_face(None);
        let blank = compositor.create_texture(blank.w, blank.h, &blank.rgba);
        let shadow = chip_shadow_face();
        let shadow = compositor.create_texture(shadow.w, shadow.h, &shadow.rgba);
        self.session
            .app_mut()
            .set_core_part_faces(sockets, chips, blank, shadow);
        let legend = [
            hint_face("B", "Cancel"),
            arrows_hint_face("Swap"),
            hint_face("A", "Choose"),
        ]
        .into_iter()
        .map(|f| (compositor.create_texture(f.w, f.h, &f.rgba), f.w))
        .collect();
        self.session.app_mut().set_core_legend_faces(legend);
        let roles = menu_faces(compositor, LinkRow::ALL.iter().map(|r| r.text()));
        self.session.app_mut().set_link_menu_faces(roles);
        if let Some(linked) = menu_faces(compositor, ["Linked"].into_iter()).pop() {
            self.session.app_mut().set_link_linked_face(linked);
        }
        let legend = [
            hint_face("B", "Cancel"),
            hint_face("SELECT", "Mode"),
            arrows_hint_face("Swap"),
            hint_face("A", "Link"),
            hint_face("A", "OK"),
            hint_face("B", "Back"),
            hint_face("A", "End Link"),
        ]
        .into_iter()
        .map(|f| (compositor.create_texture(f.w, f.h, &f.rgba), f.w))
        .collect();
        self.session.app_mut().set_link_legend_faces(legend);
        let steps = menu_faces(compositor, LinkStep::ALL.iter().map(|s| s.line()));
        self.session.app_mut().set_link_step_faces(steps);
        let fails = menu_faces(compositor, LinkFail::SHOWN.iter().map(|f| f.line()));
        self.session.app_mut().set_link_fail_faces(fails);
        let toasts = Toast::ALL
            .iter()
            .map(|t| {
                let f = toast_face(*t);
                compositor.create_texture(f.w, f.h, &f.rgba)
            })
            .collect();
        self.session.app_mut().set_toast_faces(toasts);
        let legend = legend_faces(compositor, &LEGEND);
        self.session.app_mut().set_legend_faces(legend);
        let hint = set_clock_hint_face();
        self.clocks.hint = Some(compositor.create_texture(hint.w, hint.h, &hint.rgba));
        let shadow = cart_shadow();
        let id = compositor.create_texture(shadow.w, shadow.h, &shadow.rgba);
        self.session.app_mut().set_cart_shadow(id);
        let notched = gb_cart_shadow(GbShell::Notched);
        let notched = compositor.create_texture(notched.w, notched.h, &notched.rgba);
        let rounded = gb_cart_shadow(GbShell::Rounded);
        let rounded = compositor.create_texture(rounded.w, rounded.h, &rounded.rgba);
        self.session.app_mut().set_gb_cart_shadows(notched, rounded);
        let bolt = icon_face(Icon::Charging, BOLT_PX, HUD_INK);
        let bolt_id = compositor.create_texture(bolt.w, bolt.h, &bolt.rgba);
        self.session.app_mut().set_bolt_face(bolt_id);
        let wifi = icon_face(Icon::Wifi, 18.0, HUD_INK);
        let wifi_id = compositor.create_texture(wifi.w, wifi.h, &wifi.rgba);
        self.session.app_mut().set_wifi_face(wifi_id);
        self.upload_wallpaper(compositor);
    }

    fn upload_wallpaper(&mut self, compositor: &mut Compositor) {
        let app = self.session.app();
        let seed = app.wall_secs().unsigned_abs();
        let Some(rgba) = app
            .root()
            .and_then(|root| wallpaper::pick(root, seed))
            .and_then(|path| wallpaper_face(&path))
        else {
            return;
        };
        let id = compositor.create_texture(OUT_W, OUT_H, &rgba);
        self.session.app_mut().set_wallpaper(id);
    }

    pub fn render(&mut self, compositor: &mut Compositor, window: (u32, u32)) {
        self.compose(compositor);
        compositor.end_frame(window);
    }

    pub fn compose(&mut self, compositor: &mut Compositor) {
        compositor.set_blue_light(self.session.app().blue_light());
        compositor.set_shake(self.session.app().screen_shake());
        compositor.set_screen_power(self.session.app().screen_power());
        compositor.set_game_source_rect(self.session.app().source_rect());
        let (source, actual) = self.session.app().shader_source();
        compositor.set_game_shader_source(source, actual);
        compositor.begin_frame();
        if let Some(frame) = self.session.frame() {
            compositor.upload_game(&frame);
            crate::latency::taken();
        }
        self.sync_labels(compositor);
        sync_clock(self.session.app_mut(), compositor, &mut self.clocks);
        sync_about(self.session.app_mut(), compositor, &mut self.about);
        sync_quick_clock(self.session.app_mut(), compositor, &mut self.quick_clock);
        sync_quick_shader(self.session.app_mut(), compositor, &mut self.quick_shader);
        sync_shader(&mut self.session, compositor);
        sync_cheats(self.session.app_mut(), compositor, &mut self.cheats);
        if let Some(screen) = self.session.app().account_screen() {
            if self.account_shown != Some(screen.revision()) {
                let face = screen.face();
                self.account_shown = Some(screen.revision());
                let tex = upload(compositor, &mut self.account_tex, face);
                self.session.app_mut().set_account_panel_face(tex);
            } else if let Some(tex) = self.account_tex {
                self.session.app_mut().set_account_panel_face(tex);
            }
        } else {
            self.account_shown = None;
        }
        if let Some(screen) = self.session.app().wifi_screen() {
            if self.wifi_shown.as_ref() != Some(screen) {
                let face = screen.face();
                self.wifi_shown = Some(screen.clone());
                let tex = upload(compositor, &mut self.wifi_tex, face);
                self.session.app_mut().set_wifi_panel_face(tex);
            } else if let Some(tex) = self.wifi_tex {
                self.session.app_mut().set_wifi_panel_face(tex);
            }
        } else {
            self.wifi_shown = None;
        }
        sync_shader_params(&mut self.session, compositor, &mut self.shader_params);
        sync_core_picker(
            self.session.app_mut(),
            compositor,
            &self.faces,
            &mut self.core_asked,
            &mut self.core_board_tex,
            &mut self.core_lid_tex,
            &mut self.core_built,
        );
        if !self.link_art_done {
            if let Some(art) = self.link_art.take() {
                let mut up = |f: &slot_ui::CartFace| Sprite {
                    tex: compositor.create_texture(f.w, f.h, &f.rgba),
                    w: f.w,
                    h: f.h,
                };
                let sprites = LinkSprites {
                    port: up(&art.port),
                    plug_host: up(&art.plug_host),
                    plug_join: up(&art.plug_join),
                    adapter: up(&art.adapter),
                    arcs_right: [
                        up(&art.arcs_right[0]),
                        up(&art.arcs_right[1]),
                        up(&art.arcs_right[2]),
                    ],
                    arcs_left: [
                        up(&art.arcs_left[0]),
                        up(&art.arcs_left[1]),
                        up(&art.arcs_left[2]),
                    ],
                    clicks: up(&art.clicks),
                    arrow_left: up(&art.arrow_left),
                    arrow_right: up(&art.arrow_right),
                    net_home: up(&art.net_home),
                    net_direct: up(&art.net_direct),
                };
                self.session.app_mut().set_link_sprites(sprites);
                self.link_art_done = true;
            }
        }
        sync_switcher(
            self.session.app_mut(),
            compositor,
            Faces {
                pool: &mut self.polaroid_texes,
                title: &mut self.title_tex,
                undo: &mut self.undo_tex,
            },
            &mut self.switcher,
        );
        self.draws.clear();
        self.achievements.update(&mut self.session, compositor);
        self.session.app().draw(&mut self.draws);
        self.achievements.draw(&self.session, &mut self.draws);
        compositor.draw_list(&self.draws);
    }

    pub fn drive_emulator(&mut self) {
        self.session.set_driven(true);
    }

    pub fn step_emulator(&self, present: Duration, timeout: Duration) -> bool {
        self.session.step_emulator(present, timeout)
    }

    /// At most one texture upload per frame. The worker already decoded and drew it.
    fn sync_labels(&mut self, compositor: &mut Compositor) {
        let Some(ready) = self.labels.take() else {
            return;
        };
        let Some(path) = ready.cart.label.clone() else {
            return;
        };
        let app = self.session.app_mut();
        if let Some(tex) = app.attach_label(&ready.cart.rom, path) {
            compositor.update_texture(tex, ready.face.w, ready.face.h, &ready.face.rgba);
            if self.core_asked.as_deref() == Some(ready.cart.key().as_str())
                || self.core_built.as_deref() == Some(ready.cart.key().as_str())
            {
                self.core_asked = None;
                self.core_built = None;
            }
        }
    }

    pub fn advance(&mut self, input: &mut dyn InputSource) {
        let now = self.now();
        let events = input.poll(now);
        self.session.feed(events, now);
        let dt = self.last.elapsed().as_secs_f32();
        self.last = Instant::now();
        self.session.update(dt);
    }

    pub fn app(&self) -> &crate::app::App {
        self.session.app()
    }

    pub fn core_settling(&self) -> bool {
        self.session.core_settling()
    }

    pub fn advance_at(&mut self, input: &mut dyn InputSource, now: Millis, dt: f32) {
        let events = input.poll(now);
        self.session.feed(events, now);
        self.session.update(dt);
    }

    fn now(&self) -> Millis {
        self.start.elapsed().as_millis() as Millis
    }

    pub fn powering_off(&self) -> bool {
        self.session.app().ready_to_power_off() && self.session.achievement_flush_ready()
    }

    pub fn poweroff(&mut self) {
        self.session.app_mut().poweroff();
    }

    pub fn set_doze_policy(&mut self, policy: Box<dyn crate::app::DozePolicy>) {
        self.session.app_mut().set_doze_policy(policy);
    }

    pub fn stop_requested(&mut self, stop: &AtomicBool) -> bool {
        if self.exit_saved.is_some() {
            return true;
        }
        if !stop.load(Ordering::Relaxed) {
            return false;
        }
        self.exit_saved = Some(
            self.session
                .flush_before_exit(Instant::now() + EXIT_TIMEOUT),
        );
        true
    }

    pub fn exit_saved(&self) -> Option<bool> {
        self.exit_saved
    }
}

fn menu_faces<'a>(
    compositor: &mut Compositor,
    labels: impl Iterator<Item = &'a str>,
) -> Vec<(TexId, u32, u32)> {
    labels
        .map(|label| {
            let f = menu_face(label);
            (compositor.create_texture(f.w, f.h, &f.rgba), f.w, f.h)
        })
        .collect()
}

fn legend_faces(compositor: &mut Compositor, legend: &[(&str, &str)]) -> Vec<TexId> {
    legend
        .iter()
        .map(|(key, label)| {
            let f = hint_face(key, label);
            compositor.create_texture(f.w, f.h, &f.rgba)
        })
        .collect()
}

struct Faces<'a> {
    pool: &'a mut Vec<TexId>,
    title: &'a mut Option<TexId>,
    undo: &'a mut Option<TexId>,
}

fn sync_switcher(app: &mut App, compositor: &mut Compositor, texes: Faces, state: &mut Switcher) {
    if !matches!(app.phase(), Phase::Polaroids { .. }) {
        state.open = false;
        return;
    }
    if !state.open {
        state.open = true;
        state.titled = None;
        let faces: Vec<_> = app.polaroid_entries().iter().map(photo_face).collect();
        let ids = faces
            .iter()
            .enumerate()
            .map(|(i, f)| match texes.pool.get(i) {
                Some(id) => {
                    compositor.update_texture(*id, f.w, f.h, &f.rgba);
                    *id
                }
                None => {
                    let id = compositor.create_texture_nearest(f.w, f.h, &f.rgba);
                    texes.pool.push(id);
                    id
                }
            })
            .collect();
        app.set_polaroid_faces(ids);

        let label = app
            .undo_label()
            .map(|l| upload(compositor, texes.undo, hint_face("X", l)));
        app.set_undo_face(label);
    }
    if state.titled.as_deref() != app.polaroid_stamp() {
        state.titled = app.polaroid_stamp().map(str::to_string);
        let face = title_face(&app.polaroid_title(&format_stamp(app.wall_secs())));
        let id = upload(compositor, texes.title, face);
        app.set_polaroid_title_face(id);
    }
}

fn sync_clock(app: &mut App, compositor: &mut Compositor, clocks: &mut Clocks) {
    let picked = app.picker().map(|p| p.text());
    if picked != clocks.picked {
        clocks.picked = picked;
        if let (Some(face), Some(hint)) = (app.picker().map(|p| p.face()), clocks.hint) {
            let line = upload(compositor, &mut clocks.line, face);
            app.set_clock_faces(line, hint);
        }
    }
    let shown = hhmm_as(app.wall_secs(), app.twelve_hour());
    if shown != clocks.shown {
        let face = word_face(&shown);
        clocks.shown = shown;
        let w = face.w;
        let id = upload(compositor, &mut clocks.shelf, face);
        app.set_shelf_clock_face(id, w);
    }
    let platform_shown = app.slot_text().unwrap_or_default();
    if platform_shown != clocks.platform {
        clocks.platform = platform_shown.clone();
        if platform_shown.is_empty() {
            app.clear_shelf_platform();
        } else {
            let face = word_face(&platform_shown);
            let w = face.w;
            let id = upload(compositor, &mut clocks.platform_tex, face);
            app.set_shelf_platform_face(id, w);
        }
    }
    let battery_shown = app
        .battery()
        .map(|b| format!("{}%", b.percent))
        .unwrap_or_default();
    if battery_shown != clocks.battery {
        clocks.battery = battery_shown.clone();
        if !battery_shown.is_empty() {
            let face = word_face(&battery_shown);
            let w = face.w;
            let id = upload(compositor, &mut clocks.battery_tex, face);
            app.set_battery_percent_face(id, w);
        }
    }
}

fn sync_quick_clock(app: &mut App, compositor: &mut Compositor, state: &mut QuickClock) {
    if app.quick_menu().is_none() {
        return;
    }
    let text = date_time_text_as(app.wall_secs(), app.twelve_hour());
    if text == state.shown {
        return;
    }
    let (dim, lit) = (
        quick_value_face(&text, false),
        quick_value_face(&text, true),
    );
    let (dim_size, lit_size) = ((dim.w, dim.h), (lit.w, lit.h));
    let dim = upload(compositor, &mut state.dim, dim);
    let lit = upload(compositor, &mut state.lit, lit);
    app.set_quick_clock_faces((dim, dim_size.0, dim_size.1), (lit, lit_size.0, lit_size.1));
    state.shown = text;
}

/// The cheat list's rows, rastered only for the cheats in the window and only when the cheat in
/// a row changes, which is every row when the list scrolls and none while the bar moves inside
/// the window. The count over them is rebuilt when the bar moves.
fn sync_cheats(app: &mut App, compositor: &mut Compositor, state: &mut CheatFaces) {
    let Some(view) = app.cheat_menu_view() else {
        return;
    };
    for slot in 0..CHEAT_ROWS {
        let index = view.top + slot;
        if index >= view.len || state.built[slot] == Some((view.generation, index)) {
            continue;
        }
        let Some(title) = app.cheat_title(index).map(str::to_string) else {
            continue;
        };
        let face = cheat_label_face(&title);
        let (w, h) = (face.w, face.h);
        if w == 0 {
            continue;
        }
        let id = upload(compositor, &mut state.rows[slot], face);
        app.set_cheat_row_face(slot, (id, w, h));
        state.built[slot] = Some((view.generation, index));
    }
    let count = format!("{} of {}", view.row + 1, view.len);
    if count != state.counted {
        let face = word_face(&count);
        let w = face.w;
        let id = upload(compositor, &mut state.count, face);
        app.set_cheat_count_face((id, w));
        state.counted = count;
    }
}

/// The longest shader name the row shows whole. Past this the value runs into the label, so
/// the rest is cut and marked.
const SHADER_NAME_MAX: usize = 22;

fn shader_display(name: &str) -> String {
    if name.chars().count() <= SHADER_NAME_MAX {
        return name.to_string();
    }
    if let Some((stem, folder)) = name.rsplit_once(" (") {
        let folder: String = folder.trim_end_matches(')').chars().take(9).collect();
        let room = SHADER_NAME_MAX.saturating_sub(folder.chars().count() + 5);
        let kept: String = stem.chars().take(room).collect();
        return format!("{kept}... ({folder})");
    }
    let kept: String = name.chars().take(SHADER_NAME_MAX - 3).collect();
    format!("{kept}...")
}

/// Shader's value, built like Date & Time's: only while the menu is up, and only when the name
/// in hand is not the one last built.
fn sync_quick_shader(app: &mut App, compositor: &mut Compositor, state: &mut QuickClock) {
    if app.quick_menu().is_none() {
        return;
    }
    let text = shader_display(&slot_store::shader_label(app.shader(), app.shaders()));
    if text == state.shown {
        return;
    }
    let (dim, lit) = (
        quick_value_face(&text, false),
        quick_value_face(&text, true),
    );
    let (dim_size, lit_size) = ((dim.w, dim.h), (lit.w, lit.h));
    let dim = upload(compositor, &mut state.dim, dim);
    let lit = upload(compositor, &mut state.lit, lit);
    app.set_quick_shader_faces((dim, dim_size.0, dim_size.1), (lit, lit_size.0, lit_size.1));
    state.shown = text;
}

/// Puts the look `App` asked for on the game layer. Here rather than in `App` because only
/// this side holds a GL context; the file is read here too, at the moment it is compiled, so a
/// shader edited over USB is picked up the next time the row lands on it.
///
/// Anything that goes wrong — a file gone since boot, one that will not compile — leaves the
/// LCD look on the panel and says so. The driver's log goes to stderr, which on the device is
/// the log on the card, and names the line.
fn sync_shader(session: &mut Session, compositor: &mut Compositor) {
    if let Some(error) = compositor.take_shader_error() {
        eprintln!("slot: shader: {error}");
        session.set_custom_shader(false);
        session.app_mut().set_shader_parameters(Vec::new());
        session.app_mut().shader_failed();
    }
    let Some(name) = session.app_mut().take_shader() else {
        return;
    };
    let result = match name.as_str() {
        SHADER_LCD => compositor.set_shader(ShaderChoice::Lcd),
        SHADER_OFF => compositor.set_shader(ShaderChoice::Plain),
        file => {
            let path = slot_store::shader_path(session.root(), file);
            let saved = slot_store::read_shader_params(session.root(), file);
            match path {
                Some(path) => compositor.set_shader(ShaderChoice::Preset(&path, &saved)),
                None => {
                    let _ = compositor.set_shader(ShaderChoice::Lcd);
                    eprintln!("slot: shader: {file}.glsl could not be read");
                    session.set_custom_shader(false);
                    session.app_mut().set_shader_parameters(Vec::new());
                    session.app_mut().shader_failed();
                    return;
                }
            }
        }
    };
    session
        .app_mut()
        .set_shader_parameters(compositor.shader_parameters().to_vec());
    session.set_custom_shader(result.is_ok() && !matches!(name.as_str(), SHADER_LCD | SHADER_OFF));
    if let Err(e) = result {
        eprintln!("slot: shader: {name}: {e}");
        session.app_mut().shader_failed();
    }
}

fn sync_shader_params(
    session: &mut Session,
    compositor: &mut Compositor,
    faces: &mut ParameterFaces,
) {
    if let Some(parameters) = session.app_mut().take_parameter_changes() {
        for p in &parameters {
            compositor.set_shader_parameter(&p.name, p.value);
        }
        let values = parameters
            .iter()
            .filter(|p| p.value != p.default)
            .map(|p| (p.name.clone(), p.value))
            .collect();
        if let Err(e) =
            slot_store::write_shader_params(session.root(), session.app().shader(), &values)
        {
            eprintln!("slot: shader parameters: {e}");
        }
    }
    let app = session.app_mut();
    if !faces.prepare(app) {
        return;
    }
    let (_, top) = app.shader_params_window();
    for slot in 0..CHEAT_ROWS {
        let Some(p) = app.shader_parameters().get(top + slot).cloned() else {
            continue;
        };
        let value = slot_ui::parameter_value_text(p.value, p.step, p.default);
        let text = format!("{} {value}", p.label);
        if faces.shown[slot] == text {
            continue;
        }
        let label = slot_ui::parameter_label_face(&p.label);
        let value = quick_value_face(&value, true);
        let sizes = [(label.w, label.h), (value.w, value.h)];
        if sizes.iter().any(|s| s.0 == 0) {
            continue;
        }
        let label_id = upload(compositor, &mut faces.rows[slot][0], label);
        let value_id = upload(compositor, &mut faces.rows[slot][1], value);
        app.set_parameter_face(
            slot,
            [
                (label_id, sizes[0].0, sizes[0].1),
                (value_id, sizes[1].0, sizes[1].1),
            ],
        );
        faces.shown[slot] = text;
    }
}

/// Built only once the screen is up: it is a 660 by 228 rasterisation and most sessions never
/// open it.
fn sync_about(app: &mut App, compositor: &mut Compositor, state: &mut AboutFace) {
    if !matches!(app.phase(), Phase::About) {
        return;
    }
    let battery = app.battery().map(|b| b.percent);
    if state.tex.is_some() && state.battery == battery {
        return;
    }
    state.battery = battery;
    let build = Build::current();
    let face = sticker_face(&StickerFields {
        battery,
        serial: &build.serial(),
        dirty_digit: build.dirty_digit(),
    });
    let id = upload(compositor, &mut state.tex, face);
    app.set_sticker_face(id);
}

fn sync_core_picker(
    app: &mut App,
    compositor: &mut Compositor,
    builder: &FaceBuilder,
    asked: &mut Option<String>,
    board: &mut Option<TexId>,
    lid: &mut Option<TexId>,
    built: &mut Option<String>,
) {
    let highlighted = app.selected_key_ref();
    if highlighted.is_some() && asked.as_deref() != highlighted {
        if let Some(cart) = app.selected_cart() {
            builder.request(cart.clone());
        }
        *asked = highlighted.map(str::to_owned);
    }
    let Some(faces) = builder.take() else {
        return;
    };
    if highlighted != Some(faces.key.as_str())
        || built.as_deref() == highlighted
        || !app.carts().any(|cart| faces.is_for(cart))
    {
        return;
    }
    let board_id = upload_rgba(
        compositor,
        board,
        faces.board.w,
        faces.board.h,
        &faces.board.rgba,
    );
    let lid_id = upload_rgba(compositor, lid, faces.lid.w, faces.lid.h, &faces.lid.rgba);
    app.set_core_board_faces(board_id, lid_id);
    *built = Some(faces.key);
}

fn upload(compositor: &mut Compositor, slot: &mut Option<TexId>, face: slot_ui::UndoFace) -> TexId {
    upload_rgba(compositor, slot, face.w, face.h, &face.rgba)
}

fn upload_rgba(
    compositor: &mut Compositor,
    slot: &mut Option<TexId>,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> TexId {
    match *slot {
        Some(id) => {
            compositor.update_texture(id, w, h, rgba);
            id
        }
        None => {
            let id = compositor.create_texture(w, h, rgba);
            *slot = Some(id);
            id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slot_input::{Action, Btn, RawEvent};
    use slot_ui::QuickRow;

    struct ExitSnapshot(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl crate::persist::Snapshot for ExitSnapshot {
        fn state(&self) -> Option<Vec<u8>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Some(b"current state".to_vec())
        }
        fn save_ram(&self) -> Option<Vec<u8>> {
            Some(b"current SRAM".to_vec())
        }
        fn thumb(&self) -> Option<Vec<u8>> {
            None
        }
        fn load(&self, _state: Vec<u8>) {}
    }

    #[test]
    fn stop_flag_flushes_once_without_shutdown_and_is_a_noop_at_the_shelf() {
        for playing in [false, true] {
            let root = tempfile::tempdir().unwrap();
            crate::root::ensure(root.path());
            if playing {
                std::fs::write(root.path().join("Games/GBA/Example.gba"), vec![0; 256]).unwrap();
            }
            slot_store::write_slot_state(
                root.path(),
                &slot_store::SlotState {
                    cart: playing.then(|| "Example".into()),
                    clock_set: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut frontend =
                Frontend::boot(Box::new(slot_power::SimPlatform::at(root.path().into())));
            let captures = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let app = frontend.session.app_mut();
            app.set_snapshot(Box::new(ExitSnapshot(captures.clone())));
            app.on_core_ready();
            for _ in 0..120 {
                app.update(1.0 / 60.0);
            }
            assert_eq!(matches!(app.phase(), Phase::Playing { .. }), playing);
            let stop = AtomicBool::new(false);
            assert!(!frontend.stop_requested(&stop));
            assert_eq!(frontend.exit_saved(), None);
            assert_eq!(captures.load(Ordering::Relaxed), 0);
            stop.store(true, Ordering::Relaxed);
            assert!(frontend.stop_requested(&stop));
            assert!(frontend.stop_requested(&stop));
            assert_eq!(frontend.exit_saved(), Some(true));
            assert_eq!(captures.load(Ordering::Relaxed), usize::from(playing));
            assert!(!frontend.app().powering_off());
            let resume = crate::persist::read_resume(
                root.path(),
                slot_store::Platform::Gba,
                slot_store::Core::Mgba,
                "Example",
            );
            let sav = crate::persist::read_sav(root.path(), slot_store::Platform::Gba, "Example");
            assert_eq!(resume, playing.then(|| b"current state".to_vec()));
            assert_eq!(sav, playing.then(|| b"current SRAM".to_vec()));
        }
    }

    #[test]
    fn parameter_faces_refresh_after_close_and_reopen_in_one_batch() {
        let root = tempfile::tempdir().unwrap();
        crate::root::ensure(root.path());
        slot_store::write_slot_state(
            root.path(),
            &slot_store::SlotState {
                clock_set: true,
                ..slot_store::SlotState::default()
            },
        )
        .unwrap();
        let mut session = Session::boot(root.path().to_path_buf());
        let app = session.app_mut();
        app.set_shader_parameters(vec![slot_gfx::preset::Parameter {
            name: "P".into(),
            label: "Amount".into(),
            default: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.1,
            value: 0.5,
        }]);
        app.apply(Action::QuickMenu);
        for _ in 0..QuickRow::Shader.index() {
            app.apply(Action::GbaDown(Btn::Down));
        }
        app.apply(Action::GbaDown(Btn::A));
        assert!(app.shader_params_open());
        let mut faces = ParameterFaces {
            shown: vec![String::new(); CHEAT_ROWS],
            ..ParameterFaces::default()
        };
        assert!(faces.prepare(session.app()));
        faces.shown.fill("Amount 0.5".into());
        let generation = faces.generation;

        session.feed(
            [
                RawEvent::Down(Btn::B),
                RawEvent::Up(Btn::B),
                RawEvent::Down(Btn::A),
                RawEvent::Up(Btn::A),
            ],
            100,
        );
        assert!(session.app().shader_params_open());
        assert_ne!(Some(session.app().shader_params_generation()), generation);
        assert!(faces.prepare(session.app()));
        assert!(faces.shown.iter().all(String::is_empty));

        faces.shown.fill("Amount 0.5".into());
        assert!(faces.prepare(session.app()));
        assert!(faces.shown.iter().all(|text| text == "Amount 0.5"));
    }
}
