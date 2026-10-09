mod common;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use slot::app::Phase;
use slot::audio::Ring;
use slot::emu::{EmuHandle, Speed};
use slot::rewind::{Rewind, RewindThread};
use slot::session::Session;
use slot_input::{Action, Btn, Millis, RawEvent};
use slot_retro::{AvInfo, ButtonMask, CoreError, RetroCore};
use slot_store::{write_slot_state, SlotState};
use slot_ui::{Draw, QuickRow};

const STATE_LEN: usize = 400_000;
const CHURN: usize = 12_000;

struct FrameOriginCore {
    frame: u64,
    video: [u8; 8],
    snapshots: Arc<AtomicBool>,
    saved: Arc<AtomicU64>,
    presents: Arc<AtomicU64>,
    refuse_restore: bool,
}

impl RetroCore for FrameOriginCore {
    fn load(&mut self, _rom: &std::path::Path) -> Result<(), CoreError> {
        Ok(())
    }
    fn run_frame(&mut self, _input: ButtonMask) {
        self.frame += 1;
        self.video = self.frame.to_le_bytes();
    }
    fn video_xrgb8888(&self) -> &[u8] {
        &self.video
    }
    fn take_audio(&mut self) -> Vec<i16> {
        Vec::new()
    }
    fn serialize(&mut self) -> Result<Vec<u8>, CoreError> {
        if !self.snapshots.load(Ordering::Relaxed) {
            return Err(CoreError::State("no snapshot yet".into()));
        }
        self.saved.fetch_add(1, Ordering::Relaxed);
        Ok(self.frame.to_le_bytes().to_vec())
    }
    fn unserialize(&mut self, data: &[u8]) -> Result<(), CoreError> {
        if self.refuse_restore {
            return Err(CoreError::State("restore refused".into()));
        }
        self.frame = u64::from_le_bytes(data.try_into().unwrap());
        Ok(())
    }
    fn save_ram(&self) -> Option<Vec<u8>> {
        None
    }
    fn load_save_ram(&mut self, _data: &[u8]) -> Result<(), CoreError> {
        Ok(())
    }
    fn av_info(&self) -> AvInfo {
        AvInfo {
            fps: 60.0,
            sample_rate: 32768.0,
        }
    }
    fn pump_link(&mut self) {
        self.presents.fetch_add(1, Ordering::Relaxed);
    }
}

fn await_origin(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "frame origin transition timed out"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn origin_worker(
    refuse_restore: bool,
) -> (EmuHandle, Arc<AtomicBool>, Arc<AtomicU64>, Arc<AtomicU64>) {
    let snapshots = Arc::new(AtomicBool::new(false));
    let saved = Arc::new(AtomicU64::new(0));
    let presents = Arc::new(AtomicU64::new(0));
    let emu = EmuHandle::spawn(
        Box::new(FrameOriginCore {
            frame: 0,
            video: [0; 8],
            snapshots: snapshots.clone(),
            saved: saved.clone(),
            presents: presents.clone(),
            refuse_restore,
        }),
        "mock".into(),
        Arc::new(Ring::new(0)),
        None,
        None,
    );
    emu.set_speed(Speed::Normal);
    (emu, snapshots, saved, presents)
}

fn await_presents(presents: &AtomicU64, count: u64) {
    let target = presents.load(Ordering::Relaxed) + count;
    await_origin(|| presents.load(Ordering::Relaxed) >= target);
}

#[test]
fn published_frame_origin_tracks_empty_history_rewind_exhaustion_pause_and_release() {
    let (emu, snapshots, saved, presents) = origin_worker(false);
    await_origin(|| emu.has_published());
    emu.set_rewinding(true);
    await_presents(&presents, 3);
    let forward = emu.latest_frame().unwrap();
    assert!(
        !forward.rewound(),
        "an empty rewind marked forward pixels as reverse"
    );
    let published = emu.published_count();
    await_presents(&presents, 3);
    assert_eq!(emu.published_count(), published);
    assert!(emu.latest_frame().is_none());

    snapshots.store(true, Ordering::Relaxed);
    emu.set_rewinding(false);
    await_origin(|| saved.load(Ordering::Relaxed) >= 3);
    snapshots.store(false, Ordering::Relaxed);
    emu.set_rewinding(true);
    await_presents(&presents, 3);
    let reverse = emu.latest_frame().unwrap();
    assert!(reverse.rewound());
    await_presents(&presents, saved.load(Ordering::Relaxed) + 3);
    let last_reverse = emu.latest_frame().unwrap_or(reverse);
    assert!(last_reverse.rewound());
    let at_reverse = u64::from_le_bytes(last_reverse[..].try_into().unwrap());
    let published = emu.published_count();
    await_presents(&presents, 3);
    assert_eq!(emu.published_count(), published);
    assert!(emu.latest_frame().is_none());
    assert!(last_reverse.rewound());

    emu.set_speed(Speed::Paused);
    await_presents(&presents, 3);
    assert_eq!(emu.published_count(), published);
    emu.set_rewinding(false);
    emu.set_speed(Speed::Normal);
    await_origin(|| emu.published_count() > published);
    let resumed = emu.latest_frame().unwrap();
    assert!(!resumed.rewound());
    assert!(u64::from_le_bytes(resumed[..].try_into().unwrap()) > at_reverse);
}

#[test]
fn a_failed_state_restore_publishes_forward_frame_origin() {
    let (emu, snapshots, saved, presents) = origin_worker(true);
    snapshots.store(true, Ordering::Relaxed);
    await_origin(|| saved.load(Ordering::Relaxed) >= 2);
    emu.set_rewinding(true);
    await_presents(&presents, 3);
    assert!(!emu.latest_frame().unwrap().rewound());
}

fn noise(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        })
        .collect()
}

fn stale() -> &'static [u8] {
    static BASE: OnceLock<Vec<u8>> = OnceLock::new();
    BASE.get_or_init(|| noise(0x5eed, STATE_LEN))
}

fn synthetic_state(i: u32) -> Vec<u8> {
    let mut s = stale().to_vec();
    let at = (i as usize * 997) % (STATE_LEN - CHURN);
    s[at..at + CHURN].copy_from_slice(&noise(i.wrapping_add(1), CHURN));
    s[..8].copy_from_slice(&(i as u64).to_le_bytes());
    s
}

#[test]
fn rewind_reconstructs_states_exactly_in_reverse() {
    let mut r = Rewind::new(4 * 1024 * 1024);
    let states: Vec<Vec<u8>> = (0..200u32).map(synthetic_state).collect();
    for s in &states {
        r.push(s);
    }
    for i in (0..200).rev() {
        assert_eq!(r.pop().unwrap(), states[i], "mismatch rewinding to {i}");
    }
}

#[test]
fn rewind_respects_its_byte_budget() {
    let mut r = Rewind::new(256 * 1024);
    for i in 0..5000u32 {
        r.push(&synthetic_state(i));
    }
    assert!(r.bytes_used() <= 256 * 1024, "used {}", r.bytes_used());
    assert!(r.depth() > 0);
}

#[test]
fn delta_compression_beats_storing_raw_states() {
    let mut r = Rewind::new(64 * 1024 * 1024);
    for i in 0..120u32 {
        r.push(&synthetic_state(i));
    }
    assert!(
        r.bytes_used() < 120 * 400_000 / 4,
        "delta gained less than 4x: {} bytes",
        r.bytes_used()
    );
}

#[test]
fn eviction_leaves_what_it_kept_intact_and_then_bottoms_out() {
    let mut r = Rewind::new(64 * 1024);
    let states: Vec<Vec<u8>> = (0..40u32).map(synthetic_state).collect();
    for s in &states {
        r.push(s);
    }
    let kept = r.depth();
    assert!(kept > 1 && kept < 40, "the budget kept {kept} of 40");
    for i in (40 - kept..40).rev() {
        assert_eq!(r.pop().unwrap(), states[i], "mismatch rewinding to {i}");
    }
    assert!(r.pop().is_none(), "a state that was evicted came back");
    assert_eq!(r.depth(), 0);
}

#[test]
fn play_resuming_after_a_rewind_chains_onto_the_state_it_landed_on() {
    let mut r = Rewind::new(4 * 1024 * 1024);
    let states: Vec<Vec<u8>> = (0..20u32).map(synthetic_state).collect();
    for s in &states {
        r.push(s);
    }
    for _ in 0..5 {
        r.pop().expect("history was shorter than the rewind");
    }
    let resumed = synthetic_state(100);
    r.push(&resumed);
    assert_eq!(r.pop().unwrap(), resumed);
    assert_eq!(r.pop().unwrap(), states[14]);
    assert_eq!(r.pop().unwrap(), states[13]);
}

#[test]
fn fill_reads_full_on_a_full_ring_and_empty_once_it_is_spent() {
    let mut r = Rewind::new(1024 * 1024);
    assert_eq!(r.fill(), 0);
    for i in 0..200u32 {
        r.push(&synthetic_state(i));
    }
    assert!(r.fill() > 90, "a ring at its budget read {}", r.fill());
    while r.pop().is_some() {}
    assert_eq!(r.fill(), 0, "a spent ring still reads as holding history");
}

#[test]
fn the_rewind_bar_is_up_while_l2_is_held_and_gone_once_it_is_let_go() {
    let d = common::tmp_root_with_carts(&["Emerald"]);
    write_slot_state(
        d.path(),
        &SlotState {
            cart: Some("Emerald".into()),
            clock_set: true,
            utc_offset_min: 0,
            ..Default::default()
        },
    )
    .expect("write slot.state");
    let mut s = Session::boot(d.path().to_path_buf());
    let mut now: Millis = 0;

    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(s.app().phase(), Phase::Playing { .. }) {
        assert!(Instant::now() < deadline, "the cart never seated");
        step(&mut s, &mut now, None);
        std::thread::sleep(Duration::from_millis(1));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.game_visible() {
        assert!(Instant::now() < deadline, "the game layer never came up");
        step(&mut s, &mut now, None);
        std::thread::sleep(Duration::from_millis(1));
    }

    step(&mut s, &mut now, Some(RawEvent::Down(Btn::L2)));
    assert!(s.actually_rewinding());
    for _ in 0..200 {
        step(&mut s, &mut now, None);
    }
    assert!(
        !drawn(&s).is_empty(),
        "the rewind bar timed out while L2 was held"
    );
    step(&mut s, &mut now, Some(RawEvent::Up(Btn::L2)));
    assert!(!s.actually_rewinding());
    assert!(drawn(&s).is_empty(), "the bar outlived the hold");
}

#[test]
fn raw_l2_release_inside_a_screen_stops_a_hold_started_before_it_opened() {
    for row in [QuickRow::WifiNetworks, QuickRow::RetroAchievements] {
        for hold_in_quick_menu in [false, true] {
            let d = common::tmp_root_with_carts(&["Emerald", "Fusion"]);
            common::clocked(d.path());
            let mut s = Session::boot(d.path().into());
            let mut now = 0;
            if hold_in_quick_menu {
                s.app_mut().apply(Action::QuickMenu);
            }
            now += 16;
            s.feed([RawEvent::Down(Btn::L2)], now);
            if !hold_in_quick_menu {
                s.app_mut().apply(Action::QuickMenu);
            }
            for _ in 0..row.index() {
                s.app_mut().apply(Action::GbaDown(Btn::Down));
            }
            s.app_mut().apply(Action::GbaDown(Btn::A));
            assert_eq!(
                s.app().wifi_screen().is_some(),
                row == QuickRow::WifiNetworks
            );
            assert_eq!(
                s.app().account_screen().is_some(),
                row == QuickRow::RetroAchievements
            );
            now += 16;
            s.feed([RawEvent::Up(Btn::L2)], now);
            for _ in 0..2 {
                now += 16;
                s.feed([RawEvent::Down(Btn::B), RawEvent::Up(Btn::B)], now);
            }
            assert!(s.app().wifi_screen().is_none());
            assert!(s.app().account_screen().is_none());
            assert!(matches!(s.app().phase(), Phase::Shelf));
            s.app_mut().apply(Action::Insert);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !matches!(s.app().phase(), Phase::Playing { .. }) || !s.game_visible() {
                assert!(Instant::now() < deadline, "the game never came up");
                step(&mut s, &mut now, None);
                std::thread::sleep(Duration::from_millis(1));
            }
            for _ in 0..200 {
                step(&mut s, &mut now, None);
            }
            assert!(
                drawn(&s).is_empty(),
                "{row:?} leaked a rewind hold (quick menu: {hold_in_quick_menu})"
            );
            step(&mut s, &mut now, Some(RawEvent::Down(Btn::R2)));
            assert!(
                s.app().ff_badge().is_some(),
                "{row:?} refused fast forward after L2 was released"
            );
            step(&mut s, &mut now, Some(RawEvent::Up(Btn::R2)));
            assert!(
                s.app().ff_badge().is_none(),
                "fast forward outlived its hold"
            );
            assert!(drawn(&s).is_empty());
        }
    }
}

#[test]
fn raw_l2_without_a_keyboard_leaves_screens_unchanged_and_does_not_leak_rewind() {
    for row in [QuickRow::WifiNetworks, QuickRow::RetroAchievements] {
        let d = common::tmp_root_with_carts(&["Emerald", "Fusion"]);
        common::clocked(d.path());
        let mut s = Session::boot(d.path().into());
        let mut now = 0;
        s.app_mut().apply(Action::QuickMenu);
        for _ in 0..row.index() {
            s.app_mut().apply(Action::GbaDown(Btn::Down));
        }
        s.app_mut().apply(Action::GbaDown(Btn::A));
        if row == QuickRow::RetroAchievements {
            let deadline = Instant::now() + Duration::from_secs(2);
            while s.app().account_screen().unwrap().account.message != "Signed out" {
                assert!(
                    Instant::now() < deadline,
                    "the account worker never initialized"
                );
                s.update(0.0);
                std::thread::yield_now();
            }
        }
        let wifi = s.app().wifi_screen().cloned();
        let account_face = s.app().account_screen().map(|screen| screen.face().rgba);
        let account = s.app().account_screen().map(|screen| {
            assert!(screen.keyboard.is_none());
            (
                screen.account.clone(),
                screen.selected,
                screen.username.clone(),
                screen.status.clone(),
                screen.revision(),
            )
        });
        assert!(wifi.is_some() || account.is_some());
        assert!(wifi.as_ref().is_none_or(|screen| screen.keyboard.is_none()));
        for event in [RawEvent::Down(Btn::L2), RawEvent::Up(Btn::L2)] {
            now += 16;
            s.feed([event], now);
            assert_eq!(s.app().wifi_screen(), wifi.as_ref());
            assert_eq!(
                s.app().account_screen().map(|screen| (
                    screen.account.clone(),
                    screen.selected,
                    screen.username.clone(),
                    screen.status.clone(),
                    screen.revision(),
                )),
                account
            );
            assert!(s.app().account_screen().is_none_or(|screen| {
                screen.keyboard.is_none() && Some(screen.face().rgba) == account_face
            }));
        }
        now += 16;
        s.feed([RawEvent::Down(Btn::L2)], now);
        for _ in 0..2 {
            now += 16;
            s.feed([RawEvent::Down(Btn::B), RawEvent::Up(Btn::B)], now);
        }
        assert!(s.app().wifi_screen().is_none());
        assert!(s.app().account_screen().is_none());
        assert!(matches!(s.app().phase(), Phase::Shelf));
        s.app_mut().apply(Action::Insert);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(s.app().phase(), Phase::Playing { .. }) || !s.game_visible() {
            assert!(Instant::now() < deadline, "the game never came up");
            step(&mut s, &mut now, None);
            std::thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..200 {
            step(&mut s, &mut now, None);
        }
        assert!(drawn(&s).is_empty(), "{row:?} leaked a rewind hold");
        step(&mut s, &mut now, Some(RawEvent::Up(Btn::L2)));
        assert!(drawn(&s).is_empty());
        step(&mut s, &mut now, Some(RawEvent::Down(Btn::R2)));
        assert!(s.app().ff_badge().is_some(), "{row:?} refused fast forward");
        step(&mut s, &mut now, Some(RawEvent::Up(Btn::R2)));
        assert!(s.app().ff_badge().is_none());
    }
}

#[test]
fn a_live_link_session_refuses_to_actually_rewind() {
    let d = common::tmp_root_with_carts(&["Emerald"]);
    write_slot_state(
        d.path(),
        &SlotState {
            cart: Some("Emerald".into()),
            clock_set: true,
            utc_offset_min: 0,
            ..Default::default()
        },
    )
    .expect("write slot.state");
    let mut s = Session::boot(d.path().to_path_buf());
    let mut now: Millis = 0;

    let deadline = Instant::now() + Duration::from_secs(10);
    while !matches!(s.app().phase(), Phase::Playing { .. }) {
        assert!(Instant::now() < deadline, "the cart never seated");
        step(&mut s, &mut now, None);
        std::thread::sleep(Duration::from_millis(1));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while !s.game_visible() {
        assert!(Instant::now() < deadline, "the game layer never came up");
        step(&mut s, &mut now, None);
        std::thread::sleep(Duration::from_millis(1));
    }
    s.app_mut().begin_link(0);

    step(&mut s, &mut now, Some(RawEvent::Down(Btn::L2)));
    for _ in 0..200 {
        step(&mut s, &mut now, None);
    }
    assert!(
        drawn(&s).is_empty(),
        "the bar showed for a rewind a live session must refuse"
    );
    step(&mut s, &mut now, Some(RawEvent::Up(Btn::L2)));
}

fn step(s: &mut Session, now: &mut Millis, ev: Option<RawEvent>) {
    *now += 16;
    s.feed(ev, *now);
    s.update(1.0 / 60.0);
}

fn drawn(s: &Session) -> Vec<Draw> {
    let mut out = Vec::new();
    s.app().draw(&mut out);
    out.retain(|d| !matches!(d, Draw::Game));
    out
}

#[test]
fn a_state_that_changed_size_drops_the_history_rather_than_corrupting_it() {
    let mut r = Rewind::new(4 * 1024 * 1024);
    for i in 0..5u32 {
        r.push(&synthetic_state(i));
    }
    let bigger = vec![7u8; STATE_LEN + 64];
    r.push(&bigger);
    assert_eq!(r.pop().unwrap(), bigger);
    assert!(
        r.pop().is_none(),
        "a state was rebuilt across a size change"
    );
}

#[test]
fn the_thread_reconstructs_states_exactly_in_reverse() {
    let r = RewindThread::spawn(4 * 1024 * 1024);
    let states: Vec<Vec<u8>> = (0..120u32).map(synthetic_state).collect();
    for s in &states {
        r.push(s.clone());
    }
    for i in (0..120).rev() {
        assert_eq!(r.pop().unwrap(), states[i], "mismatch rewinding to {i}");
    }
}

#[test]
fn a_pop_sees_every_push_queued_before_it() {
    let r = RewindThread::spawn(4 * 1024 * 1024);
    for i in 0..40u32 {
        r.push(synthetic_state(i));
    }
    assert_eq!(
        r.pop().unwrap(),
        synthetic_state(39),
        "a pop overtook the pushes in front of it"
    );
    assert_eq!(r.pop().unwrap(), synthetic_state(38));
}

#[test]
fn the_thread_runs_dry_without_lying_about_it() {
    let r = RewindThread::spawn(1024 * 1024);
    assert!(r.pop().is_none(), "an untouched ring returned a state");
    r.push(synthetic_state(1));
    assert_eq!(r.pop().unwrap(), synthetic_state(1));
    assert!(r.pop().is_none(), "a spent ring kept handing states back");
}

#[test]
fn the_thread_publishes_its_fill() {
    let r = RewindThread::spawn(256 * 1024);
    for i in 0..400u32 {
        r.push(synthetic_state(i));
    }
    let _ = r.pop();
    assert!(r.fill() > 50, "a loaded ring published fill {}", r.fill());
}
