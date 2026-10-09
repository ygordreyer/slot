use std::path::PathBuf;
use std::time::{Duration, Instant};

use slot_input::{Action, Btn, Gestures, Millis, RawEvent};
use slot_retro::Rumble;
use slot_store::Platform;
use slot_ui::{FfState, Toast};

use crate::app::{App, Phase};
use crate::audio::look::BASELINE_LATENCY_MS;
use crate::audio::{open_sink, AudioLook, AudioLookError, AudioSink, Ring, Sfx, GBA_HZ};
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
    audio_look: Option<AudioLook>,
    gestures: Gestures,
    pad: Pad,
    rewinding: bool,
    fast: bool,
    motor: u16,
    reloading: bool,
    driven: bool,
    custom_shader: bool,
    shader_profile: Option<slot_gfx::preset::Profile>,
    profile_baseline: std::collections::BTreeMap<String, String>,
    profile_applied: std::collections::BTreeSet<String>,
    /// The cheats the open list was made from, and whose cart they are. See `open_cheats`.
    cheat_list: Option<(Platform, String, Vec<slot_store::Cheat>)>,
}

impl Session {
    pub fn boot(root: PathBuf) -> Self {
        Self::boot_with_sink(root, open_sink())
    }

    fn boot_with_sink(root: PathBuf, mut sink: Box<dyn AudioSink>) -> Self {
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
            audio_look: None,
            gestures: Gestures::new(),
            pad: Pad::default(),
            rewinding: false,
            fast: false,
            motor: 0,
            reloading: false,
            driven: false,
            custom_shader: false,
            shader_profile: None,
            profile_baseline: Default::default(),
            profile_applied: Default::default(),
            cheat_list: None,
        }
    }

    pub fn audio_look(&self) -> Option<&AudioLook> {
        self.audio_look.as_ref()
    }

    pub fn set_audio_look(&mut self, look: Option<AudioLook>) -> Result<(), AudioLookError> {
        if self.audio_look == look {
            return Ok(());
        }
        if let Some(emu) = &self.emu {
            if let Err(error) = emu.suspend_audio() {
                let _ = emu.set_audio_look(self.audio_look.clone());
                return Err(error);
            }
        }
        let config = |look: Option<&AudioLook>| {
            (
                look.map_or(GBA_HZ, AudioLook::output_rate),
                look.map_or(BASELINE_LATENCY_MS, AudioLook::output_latency_ms),
            )
        };
        let requested = config(look.as_ref());
        let previous = config(self.audio_look.as_ref());
        let result = (|| {
            if requested != previous {
                self.sink
                    .open_with_latency(requested.0, requested.1)
                    .map_err(|e| AudioLookError(e.to_string()))?;
            }
            if let Some(emu) = &self.emu {
                emu.set_audio_look(look.clone())?;
            } else if let Some(dsp) = look.as_ref().and_then(|l| l.dsp.as_ref()) {
                dsp.build(GBA_HZ as f32)?;
            }
            if self.emu.is_none() {
                self.sink.ring().clear();
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.audio_look = None;
            let sink_restore = self.sink.open_with_latency(GBA_HZ, BASELINE_LATENCY_MS);
            let worker_restore = self
                .emu
                .as_ref()
                .map_or(Ok(()), |emu| emu.set_audio_look(None));
            if self.emu.is_none() {
                self.sink.ring().clear();
            }
            return match (sink_restore, worker_restore) {
                (Ok(()), Ok(())) => Err(error),
                (sink, worker) => Err(AudioLookError(format!(
                    "{error}; baseline restore: sink {sink:?}, worker {worker:?}"
                ))),
            };
        }
        self.audio_look = look;
        Ok(())
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
        self.set_shader_profile(active, None);
    }

    pub fn shader_profile(&self) -> Option<&slot_gfx::preset::Profile> {
        self.shader_profile.as_ref()
    }

    pub fn set_shader_profile(&mut self, active: bool, profile: Option<slot_gfx::preset::Profile>) {
        self.custom_shader = active;
        self.shader_profile = profile.filter(|_| active);
        self.apply_shader_profile();
        self.apply_shader_audio_look();
    }

    fn apply_shader_audio_look(&mut self) {
        let wanted = self
            .shader_profile
            .as_ref()
            .filter(|profile| !profile.audio.is_empty())
            .map(|profile| AudioLook::from_entries(&profile.audio, &profile.directory, &self.root))
            .transpose();
        let result = match wanted {
            Ok(look) if self.audio_look != look => self.set_audio_look(look),
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        };
        if let Err(error) = result {
            eprintln!("slot: preset audio: {error}");
            if self.audio_look.is_some() {
                if let Err(error) = self.set_audio_look(None) {
                    eprintln!("slot: preset audio baseline: {error}");
                }
            }
            self.app.show_toast(Toast::ProfileAudioFailed);
        }
    }

    fn apply_shader_profile(&mut self) {
        let plan = crate::shader_profile::options(
            self.app.core(),
            self.app.colour_correction(),
            self.custom_shader,
            self.shader_profile.as_ref(),
            &self.profile_baseline,
            &self.profile_applied,
        );
        if let Some(emu) = &self.emu {
            for (key, value) in &plan.values {
                emu.set_option(key, value);
            }
            self.profile_applied = plan.applied;
        }
        self.app.set_profile_colour(plan.colour);
        if plan.mismatch {
            self.app.show_toast(Toast::ProfileCoreMismatch);
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
        if self.app.take_colour_correction().is_some() {
            self.apply_shader_profile();
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

    pub fn actually_rewinding(&self) -> bool {
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
        self.profile_baseline = crate::shader_profile::capture_baseline(opened.core.as_ref());
        self.profile_applied.clear();
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
        if let Some(look) = &self.audio_look {
            if let Err(error) = emu.set_audio_look(Some(look.clone())) {
                eprintln!("slot: audio look: {error}");
                self.app.show_toast(Toast::ProfileAudioFailed);
                self.audio_look = None;
                let _ = self.sink.open_with_latency(GBA_HZ, BASELINE_LATENCY_MS);
                let _ = emu.set_audio_look(None);
            }
        }
        emu.set_volume(self.app.output_volume());
        emu.set_driven(self.driven);
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
        self.apply_shader_profile();
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

#[cfg(test)]
mod profile_tests {
    use super::*;
    use crate::persist::Snapshot;
    use slot_retro::{AvInfo, ButtonMask, CoreError, MockCore, RetroCore};
    use slot_store::Core;
    use slot_ui::{QuickRow, QuickValue};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    struct Recorder {
        core: MockCore,
        values: Arc<Mutex<BTreeMap<String, String>>>,
    }
    impl RetroCore for Recorder {
        fn load(&mut self, path: &std::path::Path) -> Result<(), CoreError> {
            self.core.load(path)
        }
        fn run_frame(&mut self, input: ButtonMask) {
            self.core.run_frame(input);
        }
        fn video_xrgb8888(&self) -> &[u8] {
            self.core.video_xrgb8888()
        }
        fn take_audio(&mut self) -> Vec<i16> {
            self.core.take_audio()
        }
        fn serialize(&mut self) -> Result<Vec<u8>, CoreError> {
            self.core.serialize()
        }
        fn unserialize(&mut self, state: &[u8]) -> Result<(), CoreError> {
            self.core.unserialize(state)
        }
        fn save_ram(&self) -> Option<Vec<u8>> {
            self.core.save_ram()
        }
        fn load_save_ram(&mut self, state: &[u8]) -> Result<(), CoreError> {
            self.core.load_save_ram(state)
        }
        fn av_info(&self) -> AvInfo {
            self.core.av_info()
        }
        fn set_option(&mut self, key: &str, value: &str) {
            self.values.lock().unwrap().insert(key.into(), value.into());
        }
        fn option(&self, key: &str) -> Option<String> {
            self.values.lock().unwrap().get(key).cloned()
        }
    }

    fn game_open(session: &mut Session, which: Core) -> Arc<Mutex<BTreeMap<String, String>>> {
        session.emu = None;
        session.app.set_core(which);
        let values = Arc::new(Mutex::new(BTreeMap::from([
            ("mgba_audio_low_pass_filter".into(), "disabled".into()),
            ("mgba_audio_low_pass_range".into(), "75".into()),
        ])));
        let core = Recorder {
            core: MockCore::new(),
            values: values.clone(),
        };
        session.profile_baseline = crate::shader_profile::capture_baseline(&core);
        session.profile_applied.clear();
        session.emu = Some(EmuHandle::spawn(
            Box::new(core),
            "mock".into(),
            session.sink.ring(),
            None,
            None,
        ));
        session.apply_shader_profile();
        flush(session);
        values
    }

    fn flush(session: &Session) {
        assert!(session.emu().unwrap().snapshot().state().is_some());
    }

    fn owner_profile() -> slot_gfx::preset::Profile {
        slot_gfx::preset::parse_preset("shaders=1\nshader0=copy.glsl\nslot_core=mgba\nslot_core_options=mgba_color_correction;mgba_interframe_blending;mgba_audio_low_pass_filter;mgba_audio_low_pass_range\nmgba_color_correction=GBA\nmgba_interframe_blending=mix_smart\nmgba_audio_low_pass_filter=enabled\nmgba_audio_low_pass_range=30", std::path::Path::new("owner.glslp")).unwrap().profile
    }

    pub(super) fn owner_audio_profile(root: &std::path::Path) -> slot_gfx::preset::Profile {
        let directory = root.join("Shaders/private");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(root.join("Audio")).unwrap();
        std::fs::write(
            root.join("Audio/ChipTuneEnhance.dsp"),
            include_str!("../../../card/Audio/ChipTuneEnhance.dsp"),
        )
        .unwrap();
        let mut profile = owner_profile();
        profile.directory = directory;
        profile.audio = BTreeMap::from([
            ("slot_audio_rate".into(), "48000".into()),
            ("slot_audio_latency_ms".into(), "256".into()),
            ("slot_audio_resampler".into(), "sinc".into()),
            ("slot_audio_resampler_quality".into(), "higher".into()),
            ("slot_audio_sync".into(), "true".into()),
            ("slot_audio_gain_db".into(), "-2.0".into()),
            (
                "slot_audio_dsp".into(),
                "../../Audio/ChipTuneEnhance.dsp".into(),
            ),
        ]);
        profile
    }

    #[test]
    fn profile_audio_installs_owner_look_and_reselection_does_not_reinstall() {
        let (root, mut session, calls) = super::audio_look_tests::session(false);
        game_open(&mut session, Core::Mgba);
        let profile = owner_audio_profile(root.path());
        let wanted =
            AudioLook::from_entries(&profile.audio, &profile.directory, root.path()).unwrap();
        session.set_shader_profile(true, Some(profile.clone()));
        assert_eq!(session.audio_look(), Some(&wanted));
        assert_eq!(session.audio_ring().sample_rate(), 48000);
        assert_eq!(wanted.latency_ms, Some(256));
        assert_eq!(wanted.sync, Some(true));
        assert_eq!(wanted.gain_db, Some(-2.0));
        assert!(wanted.dsp.is_some());
        assert_eq!(session.emu().unwrap().audio_look_commands(), 1);
        session.audio_ring().push(&[1234; 100]);

        session.set_shader_profile(true, Some(profile));
        assert_eq!(session.audio_look(), Some(&wanted));
        assert_eq!(session.emu().unwrap().audio_look_commands(), 1);
        assert_eq!(session.audio_ring().queued_frames(), 50);
        assert_eq!(*calls.lock().unwrap(), [(32768, 40), (48000, 256)]);
    }

    #[test]
    fn profile_audio_restores_baseline_on_empty_profile_leave_and_shader_failure() {
        let (root, mut session, calls) = super::audio_look_tests::session(false);
        game_open(&mut session, Core::Mgba);
        let profile = owner_audio_profile(root.path());
        for (active, next) in [
            (true, Some(owner_profile())),
            (true, None),
            (false, Some(profile.clone())),
            (false, None),
        ] {
            session.set_shader_profile(true, Some(profile.clone()));
            assert!(session.audio_look().is_some());
            if active || next.is_some() {
                session.set_shader_profile(active, next);
            } else {
                session.set_custom_shader(false);
            }
            assert_eq!(session.audio_look(), None);
            assert_eq!(session.audio_ring().sample_rate(), GBA_HZ);
        }
        assert_eq!(session.emu().unwrap().audio_look_commands(), 8);
        assert_eq!(calls.lock().unwrap().len(), 9);
    }

    #[test]
    fn profile_audio_rejects_dsp_outside_card_and_clears_previous_look() {
        let (root, mut session, calls) = super::audio_look_tests::session(false);
        let outside = tempfile::tempdir_in(root.path().parent().unwrap()).unwrap();
        let dsp = outside.path().join("outside.dsp");
        std::fs::write(&dsp, "filters=0").unwrap();
        let profile = owner_audio_profile(root.path());
        session.set_shader_profile(true, Some(profile.clone()));
        let mut invalid = profile;
        invalid.audio.insert(
            "slot_audio_dsp".into(),
            format!(
                "../../../{}/outside.dsp",
                outside.path().file_name().unwrap().to_str().unwrap()
            ),
        );
        let error =
            AudioLook::from_entries(&invalid.audio, &invalid.directory, root.path()).unwrap_err();
        assert!(error.to_string().contains("path escapes card root"));

        session.set_shader_profile(true, Some(invalid.clone()));
        assert_eq!(session.shader_profile(), Some(&invalid));
        assert_eq!(session.audio_look(), None);
        assert_eq!(session.audio_ring().sample_rate(), GBA_HZ);
        assert_eq!(session.app.toast(), Some(Toast::ProfileAudioFailed));
        assert_eq!(
            *calls.lock().unwrap(),
            [(32768, 40), (48000, 256), (32768, 40)]
        );
    }

    #[test]
    fn profile_audio_errors_keep_video_profile_and_core_options() {
        for sink_fails in [false, true] {
            let (root, mut session, calls) = super::audio_look_tests::session(sink_fails);
            let values = game_open(&mut session, Core::Mgba);
            let mut profile = owner_audio_profile(root.path());
            profile.overlay = Some(root.path().join("Config/overlays/owner.png"));
            if !sink_fails {
                session.set_shader_profile(true, Some(profile.clone()));
                profile
                    .audio
                    .insert("slot_audio_rate".into(), "invalid".into());
            }
            session.set_shader_profile(true, Some(profile.clone()));
            flush(&session);
            assert!(session.custom_shader);
            assert_eq!(session.shader_profile(), Some(&profile));
            for (key, value) in &profile.core_options {
                assert_eq!(values.lock().unwrap()[key], *value);
            }
            assert_eq!(
                session.app.quick_value(QuickRow::ColourCorrection),
                Some(QuickValue::Gba)
            );
            assert_eq!(session.app.toast(), Some(Toast::ProfileAudioFailed));
            assert_eq!(session.audio_look(), None);
            assert_eq!(session.audio_ring().sample_rate(), GBA_HZ);
            assert_eq!(
                *calls.lock().unwrap(),
                [(32768, 40), (48000, 256), (32768, 40)]
            );
        }
    }

    #[test]
    fn profile_options_apply_on_select_and_game_open_and_restore_on_leave_or_failure() {
        let root = tempfile::tempdir().unwrap();
        let mut session = Session::boot(root.path().into());
        let first = game_open(&mut session, Core::Mgba);
        session.set_shader_profile(true, Some(owner_profile()));
        flush(&session);
        for (key, value) in &owner_profile().core_options {
            assert_eq!(first.lock().unwrap()[key], *value);
        }
        assert_eq!(
            session.app.quick_value(QuickRow::ColourCorrection),
            Some(QuickValue::Gba)
        );
        let next = game_open(&mut session, Core::Mgba);
        for (key, value) in &owner_profile().core_options {
            assert_eq!(next.lock().unwrap()[key], *value);
        }
        session.set_shader_profile(true, None);
        flush(&session);
        {
            let values = next.lock().unwrap();
            assert_eq!(
                values["mgba_color_correction"],
                if session.app.colour_correction() {
                    "Auto"
                } else {
                    "OFF"
                }
            );
            assert_eq!(values["mgba_interframe_blending"], "OFF");
            assert_eq!(values["mgba_audio_low_pass_filter"], "disabled");
            assert_eq!(
                values["mgba_audio_low_pass_range"], "75",
                "restore actual core baseline"
            );
        }
        assert_eq!(
            session.app.quick_value(QuickRow::ColourCorrection),
            Some(QuickValue::flag(session.app.colour_correction()))
        );
        session.set_shader_profile(true, Some(owner_profile()));
        session.set_custom_shader(false);
        flush(&session);
        assert!(session.shader_profile().is_none());
        let values = next.lock().unwrap();
        assert_eq!(values["mgba_interframe_blending"], "mix");
        assert_eq!(values["mgba_audio_low_pass_filter"], "disabled");
        assert_eq!(values["mgba_audio_low_pass_range"], "75");
    }

    #[test]
    fn profile_options_skip_mismatched_core_and_report_a_nonfatal_notice() {
        let root = tempfile::tempdir().unwrap();
        let mut session = Session::boot(root.path().into());
        let values = game_open(&mut session, Core::Gpsp);
        session.set_shader_profile(true, Some(owner_profile()));
        flush(&session);
        assert!(session.shader_profile().is_some() && session.custom_shader);
        assert_eq!(session.app.core(), Core::Gpsp);
        assert_eq!(session.app.toast(), Some(Toast::ProfileCoreMismatch));
        let values = values.lock().unwrap();
        assert!(!values.contains_key("mgba_color_correction"));
        assert!(!values.contains_key("mgba_interframe_blending"));
        assert_eq!(values["mgba_audio_low_pass_filter"], "disabled");
        assert_eq!(
            session.app.quick_value(QuickRow::ColourCorrection),
            Some(QuickValue::flag(session.app.colour_correction()))
        );
    }
}

#[cfg(test)]
mod audio_look_tests {
    use super::*;
    use crate::audio::look::ResamplerKind;
    use crate::audio::sinc::SincQuality;
    use crate::audio::{AudioError, StubSink};
    use std::sync::{Arc, Mutex};

    struct RecordingSink {
        stub: StubSink,
        calls: Arc<Mutex<Vec<(u32, u32)>>>,
        fail: bool,
    }
    impl AudioSink for RecordingSink {
        fn open(&mut self, rate: u32) -> Result<(), AudioError> {
            self.open_with_latency(rate, 40)
        }
        fn open_with_latency(&mut self, rate: u32, latency: u32) -> Result<(), AudioError> {
            self.calls.lock().unwrap().push((rate, latency));
            if self.fail && rate == 48000 {
                return Err(AudioError::Device("requested rate refused".into()));
            }
            self.stub.open(rate)
        }
        fn ring(&self) -> Arc<Ring> {
            self.stub.ring()
        }
    }
    fn look() -> AudioLook {
        AudioLook {
            rate: Some(48000),
            latency_ms: Some(256),
            resampler: Some(ResamplerKind::Sinc),
            quality: Some(SincQuality::Higher),
            gain_db: Some(-2.0),
            ..AudioLook::default()
        }
    }
    pub(super) fn session(fail: bool) -> (tempfile::TempDir, Session, Arc<Mutex<Vec<(u32, u32)>>>) {
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sink = RecordingSink {
            stub: StubSink::new(),
            calls: calls.clone(),
            fail,
        };
        let session = Session::boot_with_sink(root.path().into(), Box::new(sink));
        (root, session, calls)
    }
    #[test]
    fn apply_and_restore_baseline() {
        let (_root, mut session, calls) = session(false);
        session.audio_ring().push(&[1234; 100]);
        session.set_audio_look(Some(look())).unwrap();
        assert_eq!(session.audio_ring().sample_rate(), 48000);
        assert_eq!(session.audio_ring().queued_frames(), 0);
        assert_eq!(session.audio_look().unwrap().gain_db, Some(-2.0));
        session.set_audio_look(None).unwrap();
        assert_eq!(session.audio_ring().sample_rate(), 32768);
        assert_eq!(session.audio_ring().queued_frames(), 0);
        assert_eq!(session.audio_look(), None);
        let baseline = AudioLook::default();
        assert_eq!(baseline.output_latency_ms(), 40);
        assert_eq!(
            baseline.resampler.unwrap_or_default(),
            ResamplerKind::Linear
        );
        assert_eq!(baseline.gain(), 1.0);
        assert_eq!(
            *calls.lock().unwrap(),
            [(32768, 40), (48000, 256), (32768, 40)]
        );
    }
    #[test]
    fn failed_reopen_restores_baseline() {
        let (_root, mut session, calls) = session(true);
        session.emu = Some(EmuHandle::spawn(
            Box::new(slot_retro::MockCore::new()),
            PathBuf::from("mock"),
            session.audio_ring(),
            None,
            None,
        ));
        session
            .set_audio_look(Some(AudioLook {
                gain_db: Some(-2.0),
                ..AudioLook::default()
            }))
            .unwrap();
        let error = session.set_audio_look(Some(look())).unwrap_err();
        assert!(error.to_string().contains("requested rate refused"));
        assert_eq!(session.audio_ring().sample_rate(), 32768);
        assert_eq!(session.audio_look(), None);
        assert_eq!(
            *calls.lock().unwrap(),
            [(32768, 40), (48000, 256), (32768, 40)]
        );
    }
    #[test]
    fn gain_only_does_not_reopen_sink() {
        let (_root, mut session, calls) = session(false);
        session
            .set_audio_look(Some(AudioLook {
                gain_db: Some(-2.0),
                ..AudioLook::default()
            }))
            .unwrap();
        session.set_audio_look(None).unwrap();
        assert_eq!(*calls.lock().unwrap(), [(32768, 40)]);
    }
    #[test]
    fn worker_reconfigures_while_paused_then_restores() {
        let (_root, mut session, calls) = session(false);
        let emu = EmuHandle::spawn(
            Box::new(slot_retro::MockCore::new()),
            PathBuf::from("mock"),
            session.audio_ring(),
            None,
            None,
        );
        session.emu = Some(emu);
        session.set_audio_look(Some(look())).unwrap();
        assert_eq!(session.audio_ring().sample_rate(), 48000);
        assert_eq!(session.emu().unwrap().observed_speed(), Speed::Paused);
        session.set_audio_look(None).unwrap();
        assert_eq!(session.audio_ring().sample_rate(), 32768);
        assert_eq!(
            *calls.lock().unwrap(),
            [(32768, 40), (48000, 256), (32768, 40)]
        );
    }
    fn mock_cart() -> slot_store::Cart {
        slot_store::Cart {
            stem: "mock".into(),
            title: "Mock".into(),
            rom: PathBuf::from("mock"),
            platform: Platform::Gba,
            label: None,
            code: String::new(),
            shell: None,
        }
    }

    #[test]
    fn updates_and_volume_preserve_active_look_and_queued_audio() {
        let (_root, mut session, calls) = session(false);
        session.app = App::new(vec![mock_cart()]);
        session.app.apply(Action::Insert);
        session.app.take_sfx();
        session.emu = Some(EmuHandle::spawn(
            Box::new(slot_retro::MockCore::new()),
            PathBuf::from("mock"),
            session.audio_ring(),
            None,
            None,
        ));
        session.set_audio_look(Some(look())).unwrap();
        session.audio_ring().push(&[1234; 100]);
        // Keep the worker paused while exercising per-frame and volume handling.
        for _ in 0..8 {
            session.update(0.0);
        }
        for action in [
            Action::VolumeUp,
            Action::VolumeDown,
            Action::MuteToggle,
            Action::MuteToggle,
        ] {
            session.act(action);
        }
        session.set_audio_look(Some(look())).unwrap();
        assert_eq!(session.emu().unwrap().audio_look_commands(), 1);
        assert_eq!(session.audio_ring().queued_frames(), 50);
        assert_eq!(*calls.lock().unwrap(), [(32768, 40), (48000, 256)]);
        session
            .set_audio_look(Some(AudioLook {
                gain_db: Some(-3.0),
                ..look()
            }))
            .unwrap();
        assert_eq!(session.emu().unwrap().audio_look_commands(), 2);
        session.set_audio_look(None).unwrap();
        assert_eq!(session.emu().unwrap().audio_look_commands(), 3);
    }

    #[test]
    fn new_game_workers_each_receive_the_look_once() {
        let (root, mut session, calls) = session(false);
        let profile = super::profile_tests::owner_audio_profile(root.path());
        session.set_shader_profile(true, Some(profile.clone()));
        let wanted = session.audio_look().cloned();
        assert!(wanted.is_some());
        for stem in ["first", "second"] {
            let mut cart = mock_cart();
            cart.stem = stem.into();
            session.app = App::new(vec![cart]);
            session.app.apply(Action::Insert);
            session.spawn_core(stem, "off");
            assert_eq!(session.shader_profile(), Some(&profile));
            assert_eq!(session.audio_look(), wanted.as_ref());
            assert_eq!(session.emu().unwrap().audio_look_commands(), 1);
            session.update(0.0);
            assert_eq!(session.emu().unwrap().audio_look_commands(), 1);
            session.emu = None;
        }
        assert_eq!(*calls.lock().unwrap(), [(32768, 40), (48000, 256)]);
    }

    #[test]
    fn ui_sound_bypasses_look_gain_and_dsp() {
        let (_root, mut session, _calls) = session(false);
        session.set_audio_look(Some(AudioLook {
            gain_db:Some(-120.0),
            dsp:Some(crate::audio::dsp::DspConfig::parse("filters=1\nfilter0=panning\npanning_left_mix=\"0 0\"\npanning_right_mix=\"0 0\"").unwrap()),
            ..AudioLook::default()
        })).unwrap();
        let mut expected = vec![12000i16, -12000, 5000, -5000];
        crate::audio::volume::apply(&mut expected, session.app.output_volume());
        session.mix_sfx(vec![12000, -12000, 5000, -5000]);
        let mut got = vec![0i16; 4];
        session.audio_ring().fill(&mut got);
        assert_eq!(got, expected);
        assert!(got.iter().any(|&x| x != 0));
    }
}
