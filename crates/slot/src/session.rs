use std::path::PathBuf;
use std::time::{Duration, Instant};

use slot_input::{Action, Btn, Gestures, Millis, RawEvent};
use slot_retro::Rumble;
use slot_store::Platform;
use slot_ui::{FfState, Toast};

use crate::app::{App, Phase};
use crate::audio::{open_sink, AudioSink, Ring, Sfx, GBA_HZ};
use crate::core::open_core;
use crate::emu::{CoreState, EmuHandle, Speed};
use crate::frames::FrameRef;
use crate::input::Pad;
use crate::persist;

pub struct Session {
    achievements: slot_achievements::Service,
    home_connected: bool,
    root: PathBuf,
    app: App,
    emu: Option<EmuHandle>,
    framerate_generation: u64,
    sink: Box<dyn AudioSink>,
    gestures: Gestures,
    pad: Pad,
    rewinding: bool,
    fast: bool,
    motor: u16,
    reloading: bool,
    driven: bool,
    custom_shader: bool,
    /// The cheats the open list was made from, and whose cart they are. See `open_cheats`.
    cheat_list: Option<(Platform, String, Vec<slot_store::Cheat>)>,
}

impl Session {
    pub fn boot(root: PathBuf) -> Self {
        let mut sink: Box<dyn AudioSink> = open_sink();
        if let Err(e) = sink.open(GBA_HZ) {
            eprintln!("slot: audio: {e}");
        }
        Session {
            achievements: slot_achievements::Service::start(root.clone()),
            home_connected: false,
            app: App::boot(&root),
            root,
            emu: None,
            framerate_generation: 0,
            sink,
            gestures: Gestures::new(),
            pad: Pad::default(),
            rewinding: false,
            fast: false,
            motor: 0,
            reloading: false,
            driven: false,
            custom_shader: false,
            cheat_list: None,
        }
    }

    pub fn play_sfx(&mut self, sfx: Sfx) {
        let ring = self.sink.ring();
        let rate = ring.sample_rate();
        if rate == 0 {
            return;
        }
        self.mix_sfx(sfx.render(rate));
    }

    /// A clip prepared off-thread, mixed at the current volume when it reaches the screen.
    pub(crate) fn mix_sfx(&mut self, mut samples: Vec<i16>) {
        if samples.is_empty() {
            return;
        }
        crate::audio::volume::apply(&mut samples, self.app.output_volume());
        self.sink.ring().mix(&samples);
    }

    pub fn audio_queued(&self) -> usize {
        self.sink.ring().queued_frames()
    }

    pub fn audio_ring(&self) -> std::sync::Arc<Ring> {
        self.sink.ring()
    }

    pub fn rumble(&mut self, strength: u16) {
        if strength == self.motor {
            return;
        }
        self.motor = strength;
        self.app.set_rumble(strength);
    }

    pub fn core_rumble(&self) -> Option<&Rumble> {
        self.emu.as_ref().map(EmuHandle::rumble)
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn achievement_account_state(&self) -> slot_achievements::AccountState {
        self.achievements.account_state()
    }

    pub fn achievement_snapshot_key(&self) -> (u64, u64) {
        (
            self.achievements.game_generation(),
            self.achievements.snapshot_epoch(),
        )
    }

    pub fn achievement_snapshot(
        &self,
    ) -> Option<std::sync::Arc<slot_achievements::GameAchievementSnapshot>> {
        self.achievements.game_snapshot()
    }

    pub fn take_achievement_notice(&self) -> Option<slot_achievements::Notice> {
        self.achievements.take_notice()
    }

    pub fn achievement_flush_ready(&self) -> bool {
        self.achievements.flush_ready()
    }

    pub fn flush_before_exit(&mut self, deadline: Instant) -> bool {
        if let Some(emu) = &self.emu {
            emu.set_speed(Speed::Paused);
        }
        let mut saved = self.app.flush_resume_before(deadline);
        if let Some(emu) = &mut self.emu {
            emu.stop_before(deadline);
            // stop_before detaches at the deadline; do not acknowledge an unconfirmed stop.
            if Instant::now() >= deadline {
                saved = false;
            }
        }
        while !self.achievement_flush_ready() {
            if Instant::now() >= deadline {
                eprintln!("slot: sigterm: achievement flush timed out");
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        saved
    }

    pub fn achievement_sync_progress(&self) -> Option<u8> {
        self.achievements.sync_progress()
    }

    pub fn achievement_sync_pending(&self) -> bool {
        self.achievements.sync_pending()
    }

    pub fn achievement_sync_status(&self) -> slot_achievements::SyncStatus {
        self.achievements.sync_status()
    }

    pub fn set_custom_shader(&mut self, active: bool) {
        self.custom_shader = active;
        if self.app.core() == slot_store::Core::Mgba {
            if let Some(emu) = &self.emu {
                emu.set_option(
                    "mgba_interframe_blending",
                    if active { "OFF" } else { "mix" },
                );
            }
        }
    }

    /// The content root: the card, or wherever the host build was pointed.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    pub fn emu(&self) -> Option<&EmuHandle> {
        self.emu.as_ref()
    }

    pub fn set_driven(&mut self, driven: bool) {
        self.driven = driven;
        if let Some(emu) = &self.emu {
            emu.set_driven(driven);
        }
    }

    pub fn step_emulator(&self, present: Duration, timeout: Duration) -> bool {
        self.emu.as_ref().is_some_and(|emu| {
            emu.tick(present);
            emu.wait_frame(timeout)
        })
    }

    pub fn frame(&self) -> Option<FrameRef> {
        self.emu.as_ref().and_then(|e| e.latest_frame())
    }

    pub fn core_settling(&self) -> bool {
        self.emu
            .as_ref()
            .is_some_and(|e| e.state() == CoreState::Loading)
    }

    pub fn has_core(&self) -> bool {
        self.emu.is_some()
    }

    pub fn frame_ready(&self) -> bool {
        self.emu.as_ref().is_some_and(EmuHandle::frame_ready)
    }

    pub fn frames_published(&self) -> u64 {
        self.emu.as_ref().map_or(0, EmuHandle::published_count)
    }

    pub fn frames_emulated(&self) -> u64 {
        self.emu.as_ref().map_or(0, EmuHandle::emulated_count)
    }

    pub fn framerate_generation(&self) -> u64 {
        self.framerate_generation
    }

    pub fn framerate_active(&self) -> bool {
        self.app.framerate_visible()
            && self.emu.as_ref().is_some_and(|emu| {
                emu.state() == CoreState::Ready && emu.observed_speed() != Speed::Paused
            })
    }

    pub fn observed_speed(&self) -> Option<Speed> {
        self.emu.as_ref().map(EmuHandle::observed_speed)
    }

    pub fn frames_taken(&self) -> u64 {
        self.emu.as_ref().map_or(0, EmuHandle::frames_taken)
    }

    pub fn game_visible(&self) -> bool {
        self.app.game_visible()
    }

    pub fn feed(&mut self, events: impl IntoIterator<Item = RawEvent>, now: Millis) {
        let mut actions = Vec::new();
        for ev in events {
            if self.app.wifi_screen().is_some() || self.app.account_screen().is_some() {
                let button = match ev {
                    RawEvent::Down(button) | RawEvent::Up(button) => button,
                };
                // L2 presses cancel the keyboard; releases must stop any earlier rewind hold.
                if matches!(ev, RawEvent::Down(Btn::L2))
                    || matches!(
                        button,
                        Btn::Up
                            | Btn::Down
                            | Btn::Left
                            | Btn::Right
                            | Btn::A
                            | Btn::B
                            | Btn::X
                            | Btn::Y
                            | Btn::L1
                            | Btn::R1
                            | Btn::Start
                            | Btn::Select
                    )
                {
                    actions.push(match ev {
                        RawEvent::Down(button) => Action::GbaDown(button),
                        RawEvent::Up(button) => Action::GbaUp(button),
                    });
                    continue;
                }
            }
            actions.extend(self.gestures.feed(ev, now));
        }
        actions.extend(self.gestures.tick(now));
        for action in actions {
            self.act(action);
        }
        self.sync_pad();
    }

    fn sync_pad(&mut self) {
        for btn in self.app.taken_buttons() {
            self.pad.apply(Action::GbaUp(*btn));
        }
        if let Some(emu) = &self.emu {
            emu.set_input(self.pad.mask());
        }
    }

    fn bridge_link(&mut self, f: impl FnOnce(&mut App)) {
        let had_link = self.app.link_active();
        let was_dozing = self.dozing();
        f(&mut self.app);
        if was_dozing && !self.dozing() {
            self.framerate_generation = self.framerate_generation.wrapping_add(1);
        }
        if had_link && !self.app.link_active() {
            if let Some(emu) = &self.emu {
                emu.end_link();
            }
        }
    }

    fn act(&mut self, action: Action) {
        if trace() {
            eprintln!("slot: {action:?} in {:?}", self.app.phase());
        }
        match action {
            Action::RewindStart => self.rewinding = true,
            Action::RewindStop => self.rewinding = false,
            Action::FfStart => self.fast = true,
            Action::FfStop => self.fast = false,
            _ => {}
        }
        let menu = self.overlaid();
        self.app.observe_account(self.achievements.account_state());
        self.bridge_link(|app| app.apply(action));
        self.sync_account();
        if matches!(
            action,
            Action::VolumeUp | Action::VolumeDown | Action::MuteToggle
        ) {
            if let Some(emu) = &self.emu {
                emu.set_volume(self.app.output_volume());
            }
        }
        if menu || self.overlaid() {
            self.pad.clear();
        } else if self.app.takes_from_the_game(action) {
            if let Action::GbaDown(btn) | Action::GbaUp(btn) = action {
                self.pad.apply(Action::GbaUp(btn));
            }
        } else {
            self.pad.apply(action);
        }
        self.sync_rumble();
    }

    fn sync_account(&mut self) {
        if let Some(control) = self.app.take_account_control() {
            self.achievements.account_control(control);
        }
        self.app.observe_account(self.achievements.account_state());
    }

    pub fn update(&mut self, dt: f32) {
        self.sync_account();
        self.bridge_link(|app| app.update(dt));
        let connected = self.app.home_connected();
        if connected && !self.home_connected {
            self.achievements.network_available();
        }
        self.home_connected = connected;
        if let Some(emu) = &self.emu {
            emu.set_volume(self.app.output_volume());
        }
        if let Some((client_id, transport)) = self.app.take_link_transport() {
            match &self.emu {
                Some(emu) => match self.app.link_player() {
                    Some(player) => emu.begin_cable(player, transport),
                    None => emu.begin_link(client_id, transport),
                },
                None => eprintln!("slot: link: a transport arrived with no core to run it"),
            }
        }
        if let Some(on) = self.app.take_colour_correction() {
            if let Some((key, value)) = crate::core::colour_option(self.app.core(), on) {
                if let Some(emu) = &self.emu {
                    emu.set_option(key, value);
                }
            }
        }
        // SELECT+X. Carried here for the same reason colour correction is: `App` never touches
        // the card's cheat files or the core.
        if self.app.take_cheats_toggle() {
            self.open_cheats();
        }
        if let Some(flags) = self.app.take_cheat_commit() {
            self.commit_cheats(flags);
        }
        // A list that closed with nothing changed leaves nothing to collect.
        if !self.app.cheat_menu_open() {
            self.cheat_list = None;
        }
        // A link picked in a mode the running core was not loaded with. Carried out here for the
        // same reason the wire is: `App` never touches the core.
        if let Some((stem, serial)) = self.app.take_link_reload() {
            self.reload_for_link(&stem, serial);
        }
        if self.app.link_active() {
            if self.emu.as_ref().is_some_and(EmuHandle::bios_mismatch) {
                self.bridge_link(|app| app.bios_mismatch());
            } else if self.emu.as_ref().is_some_and(EmuHandle::peer_ended) {
                self.bridge_link(|app| app.peer_ended());
            } else if self.emu.as_ref().is_some_and(EmuHandle::link_lost) {
                self.app.peer_lost();
            }
        }
        if let Some(sfx) = self.app.take_sfx() {
            self.play_sfx(sfx);
        }
        self.sync_core();
        self.sync_reload();
        if !self.has_core() {
            for action in self.gestures.drop_ff_latch() {
                self.act(action);
            }
        }
        self.app
            .set_game_ready(self.emu.as_ref().is_some_and(EmuHandle::has_published));
        self.sync_speed();
        self.sync_rewind_hud();
        self.sync_ff_hud();
        self.sync_rumble();
        self.sync_pad();
    }

    fn sync_rumble(&mut self) {
        let want = match &self.emu {
            Some(emu) if self.playing() && self.app.rumble_enabled() => emu.rumble().strength(),
            _ => 0,
        };
        self.rumble(want);
    }

    fn sync_ff_hud(&mut self) {
        let ff = match (self.actually_fast_forwarding(), self.gestures.ff_latched()) {
            (false, _) => FfState::Off,
            (true, false) => FfState::Held,
            (true, true) => FfState::Latched,
        };
        self.app.set_ff(ff);
    }

    fn sync_rewind_hud(&mut self) {
        let fill = self
            .actually_rewinding()
            .then(|| self.emu.as_ref().map(EmuHandle::rewind_fill))
            .flatten();
        match fill {
            Some(fill) => self.app.show_rewind(fill),
            None => self.app.hide_rewind(),
        }
    }

    fn actually_rewinding(&self) -> bool {
        self.rewinding && self.playing() && self.app.may_rewind()
    }

    fn actually_fast_forwarding(&self) -> bool {
        self.fast && self.playing() && self.app.may_fast_forward()
    }

    fn inserting(&self) -> bool {
        matches!(self.app.phase(), Phase::Inserting { .. })
    }

    fn showing_polaroids(&self) -> bool {
        matches!(self.app.phase(), Phase::Polaroids { .. })
    }

    fn overlaid(&self) -> bool {
        self.showing_polaroids() || self.held()
    }

    /// Menus and shutdown can pause the game while its phase remains Playing.
    fn playing(&self) -> bool {
        matches!(self.app.phase(), Phase::Playing { .. }) && !self.held()
    }

    fn held(&self) -> bool {
        self.app.game_menu_open() || self.app.cheat_menu_open() || self.app.shutting_down()
    }

    fn dozing(&self) -> bool {
        matches!(self.app.phase(), Phase::Doze { .. })
    }

    fn ejecting(&self) -> bool {
        matches!(self.app.phase(), Phase::Ejecting { .. })
    }

    fn sync_speed(&self) {
        if let Some(emu) = &self.emu {
            emu.set_fast_steps(u32::from(self.app.ff_speed()));
            emu.set_ff_sound(self.app.ff_sound());
            emu.set_speed(
                if self.inserting()
                    || self.ejecting()
                    || self.showing_polaroids()
                    || self.dozing()
                    || (self.held() && !self.app.link_active())
                {
                    Speed::Paused
                } else if self.actually_fast_forwarding() {
                    Speed::Fast
                } else {
                    Speed::Normal
                },
            );
            emu.set_rewinding(self.actually_rewinding());
        }
    }

    fn no_core() -> bool {
        std::env::var_os("SLOT_NO_CORE").is_some_and(|v| v != "0")
    }

    fn sync_core(&mut self) {
        if Self::no_core() {
            return;
        }
        let stem = match self.app.phase() {
            Phase::Shelf => {
                self.emu = None;
                self.app.set_cheats_applied(false);
                return;
            }
            Phase::Inserting { cart, .. } => cart.clone(),
            _ => return,
        };
        if self.emu.is_none() {
            let (_, serial) = self.app.link_mode(&stem);
            self.spawn_core(&stem, serial);
        }
        match self.emu.as_ref().map(EmuHandle::state) {
            Some(CoreState::Loading) => {}
            Some(CoreState::Ready) => self.app.on_core_ready(),
            Some(CoreState::Failed) | None => {
                self.emu = None;
                self.app.on_core_failed();
            }
        }
    }

    fn spawn_core(&mut self, stem: &str, serial: &'static str) {
        let Some((rom, platform)) = self
            .app
            .seated_cart()
            .filter(|c| c.stem == stem)
            .map(|c| (c.rom.clone(), c.platform))
        else {
            return;
        };
        let core = slot_store::core_for_platform(&self.root, stem, platform);
        self.app.set_core(core);
        self.app.set_platform(platform);
        self.app
            .set_video_mode(crate::video_mode::video_mode_for(&self.root, stem));
        self.app.set_link_loaded(serial);
        let resume = (!self.app.starting_clean())
            .then(|| persist::read_resume(&self.root, platform, core, stem))
            .flatten();
        let player = self.app.link_player();
        let opened = open_core(
            &self.root,
            core,
            serial,
            self.app.colour_correction(),
            player,
        );
        self.app.set_named_core(opened.named);
        let sav = persist::read_sav(&self.root, platform, stem);
        let ring = self.sink.ring();
        let emu = match player.filter(|_| platform == Platform::Gba) {
            Some(p) => EmuHandle::spawn_linked(opened.core, rom, ring, sav, resume, p),
            None => {
                let tracked = if opened.named && player.is_none() {
                    self.achievements.wrap(opened.core)
                } else {
                    opened.core
                };
                EmuHandle::spawn(tracked, rom, ring, sav, resume)
            }
        };
        emu.set_volume(self.app.output_volume());
        emu.set_driven(self.driven);
        if core == slot_store::Core::Mgba {
            emu.set_option(
                "mgba_interframe_blending",
                if self.custom_shader { "OFF" } else { "mix" },
            );
        }
        // Queued behind the load, which is the first thing the worker does, so they land on a
        // loaded game. Never for a cable session: both devices run both consoles from the
        // host's state, and a cheat on one is a machine the other is not simulating.
        self.app.set_cheats_applied(false);
        if self.app.link_player().is_none() {
            let codes =
                slot_store::enabled_codes(&slot_store::read_cheats(&self.root, platform, stem));
            if !codes.is_empty() {
                self.app.set_cheats_applied(true);
                slot_store::backup_save_once(&self.root, platform, stem);
                emu.set_cheats(codes);
            }
        }
        self.app.set_snapshot(Box::new(emu.snapshot()));
        self.emu = Some(emu);
        self.framerate_generation = self.framerate_generation.wrapping_add(1);
    }

    fn reload_for_link(&mut self, stem: &str, serial: &'static str) {
        eprintln!("slot: link: loading {stem} again with gpsp_serial={serial}");
        if self.emu.is_some() {
            self.app.flush_resume();
        }
        self.emu = None;
        self.spawn_core(stem, serial);
        self.reloading = true;
    }

    /// SELECT+X: the seated cart's cheats read off the card and put up as a list. Read fresh
    /// every time rather than remembered, so a `.cht` edited over USB since the cart went in is
    /// what the list shows. Kept here as well, so the flags the list closes on can be matched
    /// back to the lines of the file they came from.
    fn open_cheats(&mut self) {
        let Some((platform, stem)) = self.app.seated_cart().map(|c| (c.platform, c.stem.clone()))
        else {
            return;
        };
        let cheats = slot_store::read_cheats(&self.root, platform, &stem);
        if cheats.is_empty() {
            self.app.show_toast(Toast::NoCheats);
            return;
        }
        self.app
            .open_cheat_menu(cheats.iter().map(|c| (c.title(), c.enabled)).collect());
        if self.app.cheat_menu_open() {
            self.cheat_list = Some((platform, stem, cheats));
        }
    }

    /// The list closed on a change. The file is written first, so the card and the running game
    /// never disagree for longer than it takes to write one small file; then the core is handed
    /// every cheat that is now on, which replaces whatever it was running.
    fn commit_cheats(&mut self, flags: Vec<bool>) {
        let Some((platform, stem, mut cheats)) = self.cheat_list.take() else {
            return;
        };
        for (c, on) in cheats.iter_mut().zip(flags) {
            c.enabled = on;
        }
        if let Err(e) = slot_store::write_cheat_enables(&self.root, platform, &stem, &cheats) {
            // Still carried out: the game in hand is what the player is looking at. The card
            // only decides what the next insert starts with.
            eprintln!("slot: cheats: could not write {stem}.cht: {e}");
        }
        let codes = slot_store::enabled_codes(&cheats);
        let any = !codes.is_empty();
        if any {
            slot_store::backup_save_once(&self.root, platform, &stem);
        }
        if let Some(emu) = &self.emu {
            self.app.set_cheats_applied(any);
            emu.set_cheats(codes);
        }
        self.app.show_toast(if any {
            Toast::CheatsOn
        } else {
            Toast::CheatsOff
        });
    }

    /// Follows a reload for a link to its end, which `App` is waiting on. A core that will not
    /// load is dropped, as `sync_core` drops a refused cart's, and `App` decides what follows.
    /// The first time that is the mode the game came from, carried out here straight away so no
    /// frame passes with a seated cart and no core behind it; the second time, the cart comes
    /// back out of the slot.
    fn sync_reload(&mut self) {
        if !self.reloading {
            return;
        }
        match self.emu.as_ref().map(EmuHandle::state) {
            Some(CoreState::Loading) => {}
            Some(CoreState::Ready) => {
                self.reloading = false;
                self.app.link_reload_done();
            }
            Some(CoreState::Failed) | None => {
                self.reloading = false;
                self.emu = None;
                self.app.link_reload_failed();
                if let Some((stem, serial)) = self.app.take_link_reload() {
                    self.reload_for_link(&stem, serial);
                }
            }
        }
    }
}

pub(crate) fn trace() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SLOT_TRACE").is_some())
}
