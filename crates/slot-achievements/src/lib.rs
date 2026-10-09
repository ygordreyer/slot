//! Optional, softcore RetroAchievements. Emulator threads only copy RAM into a bounded,
//! nonblocking queue; evaluation/storage and HTTPS each have their own worker.
mod badges;
mod core;
mod library;
mod network;
mod runtime;
mod snapshot;
pub use snapshot::{
    AchievementSummary, AchievementView, GameAchievementSnapshot, GameAchievementState,
};
mod storage;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

pub use badges::{load_badge, BadgeImage};
use slot_retro::RetroCore;
use storage::{Config, Store, Unlock};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountState {
    pub enabled: bool,
    pub username: String,
    pub signed_in: bool,
    pub busy: bool,
    pub message: String,
}

pub enum AccountControl {
    SignIn {
        username: String,
        password: Zeroizing<String>,
    },
    SignOut,
    SetEnabled(bool),
}

impl std::fmt::Debug for AccountControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SignIn { username, password } => f
                .debug_struct("SignIn")
                .field("username", username)
                .field("password_length", &password.len())
                .finish(),
            Self::SignOut => f.write_str("SignOut"),
            Self::SetEnabled(value) => f.debug_tuple("SetEnabled").field(value).finish(),
        }
    }
}

const RAM_SIZE: usize = 0x58000;
const QUEUE_SIZE: usize = 8;

/// A passive shelf indicator. Reading it never acquires a worker's lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SyncStatus {
    Disabled,
    Syncing,
    Ready,
    Offline,
    Attention,
}

#[derive(Default)]
struct Status {
    account: Mutex<AccountState>,
    snapshot: Mutex<snapshot::Published>,
    epoch: AtomicU64,
    network: AtomicU8,
    unsaved: AtomicBool,
    reconnect: AtomicBool,
    // Zero means unknown; otherwise percent + 1, keeping Default valid.
    progress: AtomicU8,
    pending: AtomicBool,
    // Shared only by the evaluator and HTTP workers, never gameplay/UI.
    presence: Mutex<Option<(u64, String)>>,
}

impl Status {
    fn set(&self, value: SyncStatus) {
        self.network.store(value as u8, Ordering::Release);
    }

    fn get(&self) -> SyncStatus {
        if self.unsaved.load(Ordering::Acquire) {
            return SyncStatus::Attention;
        }
        match self.network.load(Ordering::Acquire) {
            0 => SyncStatus::Disabled,
            1 => SyncStatus::Syncing,
            2 => SyncStatus::Ready,
            3 => SyncStatus::Offline,
            _ => SyncStatus::Attention,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    Status,
    Earned,
}

#[derive(Clone, Debug)]
pub struct Notice {
    /// Local cached image, read only by the notification raster worker.
    pub badge: Option<PathBuf>,
    pub generation: u64,
    pub title: String,
    pub detail: String,
    pub kind: NoticeKind,
}

impl Notice {
    fn status(generation: u64, title: &str) -> Self {
        // Background diagnostics only; routine status is represented by the shelf icon.
        eprintln!("slot: {title}");
        Self {
            badge: None,
            generation,
            title: title.into(),
            detail: String::new(),
            kind: NoticeKind::Status,
        }
    }
}

enum Control {
    Account(AccountControl),
    Load(network::Load),
    Unload(u64),
    Flush(Arc<AtomicBool>),
}

struct Frame {
    generation: u64,
    sequence: u64,
    timeline: u64,
    earned_at: u64,
    ram: Vec<u8>,
    valid: [usize; 3],
    recycle: mpsc::SyncSender<Vec<u8>>,
}

/// Outlives games, so pending unlocks sync while the player is on the shelf, too.
pub struct Service {
    controls: mpsc::Sender<Control>,
    frames: mpsc::SyncSender<Frame>,
    notices: mpsc::Receiver<Notice>,
    enabled: Arc<AtomicBool>,
    current: Arc<AtomicU64>,
    flushing: AtomicBool,
    flushed: Arc<AtomicBool>,
    status: Arc<Status>,
}

impl Service {
    pub fn start(root: PathBuf) -> Self {
        Self::start_with(root, network::Http::new())
    }

    fn start_with(root: PathBuf, transport: impl network::Transport + Send + 'static) -> Self {
        let (controls, control_rx) = mpsc::channel();
        let (frames, frame_rx) = mpsc::sync_channel(QUEUE_SIZE);
        let (notice_tx, notices) = mpsc::channel();
        let enabled = Arc::new(AtomicBool::new(false));
        let current = Arc::new(AtomicU64::new(0));
        let flushed = Arc::new(AtomicBool::new(false));
        let worker_flushed = flushed.clone();
        let worker_enabled = enabled.clone();
        let status = Arc::new(Status::default());
        let worker_status = status.clone();
        let spawned = std::thread::Builder::new()
            .name("slot-ra".into())
            .spawn(move || {
                run(
                    root,
                    worker_enabled.clone(),
                    control_rx,
                    frame_rx,
                    notice_tx,
                    worker_status,
                    transport,
                );
                worker_enabled.store(false, Ordering::Release);
                worker_flushed.store(true, Ordering::Release);
            });
        if spawned.is_err() {
            status.set(SyncStatus::Attention);
            flushed.store(true, Ordering::Release);
            eprintln!("slot: achievements worker could not start");
        }
        Self {
            controls,
            frames,
            notices,
            enabled,
            current,
            flushing: AtomicBool::new(false),
            flushed,
            status,
        }
    }

    pub fn account_state(&self) -> AccountState {
        self.status.account.lock().unwrap().clone()
    }

    pub fn account_control(&self, control: AccountControl) {
        let mut account = self.status.account.lock().unwrap();
        if account.busy {
            return;
        }
        account.busy = true;
        account.message = match &control {
            AccountControl::SignIn { .. } => "Signing in...",
            AccountControl::SignOut => "Signing out...",
            AccountControl::SetEnabled(_) => "Saving...",
        }
        .into();
        self.status.epoch.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(false, Ordering::Release);
        if self.controls.send(Control::Account(control)).is_err() {
            account.busy = false;
            account.message = "Achievements worker unavailable".into();
        }
    }

    pub fn game_generation(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }

    pub fn snapshot_epoch(&self) -> u64 {
        self.status.epoch.load(Ordering::Acquire)
    }

    /// Never waits for evaluation, filesystem work, or the store mutex.
    pub fn game_snapshot(&self) -> Option<Arc<GameAchievementSnapshot>> {
        let published = self.status.snapshot.try_lock().ok()?;
        (published.snapshot.generation == self.game_generation()
            && published.epoch == self.snapshot_epoch())
        .then(|| published.snapshot.clone())
    }

    pub fn sync_progress(&self) -> Option<u8> {
        self.status.progress.load(Ordering::Acquire).checked_sub(1)
    }

    pub fn sync_pending(&self) -> bool {
        self.status.pending.load(Ordering::Acquire) || self.status.unsaved.load(Ordering::Acquire)
    }

    pub fn sync_status(&self) -> SyncStatus {
        self.status.get()
    }

    /// The Wi-Fi worker observed a new connection. Wake retries without blocking the UI.
    pub fn network_available(&self) {
        self.status.reconnect.store(true, Ordering::Release);
    }

    /// Poll during power-off. The UI keeps rendering while the evaluator drains final
    /// frames and fsyncs their awards; an HTTP request is never part of this barrier.
    pub fn flush_ready(&self) -> bool {
        if !self.flushing.swap(true, Ordering::AcqRel)
            && self
                .controls
                .send(Control::Flush(self.flushed.clone()))
                .is_err()
        {
            self.flushed.store(true, Ordering::Release);
        }
        self.flushed.load(Ordering::Acquire)
    }

    pub fn wrap(&self, core: Box<dyn RetroCore>) -> Box<dyn RetroCore> {
        Box::new(core::Tracked::new(core, self))
    }

    pub fn take_notice(&self) -> Option<Notice> {
        while let Ok(notice) = self.notices.try_recv() {
            // Earned notices survive an eject; stale loading notices do not.
            if notice.kind == NoticeKind::Earned
                || notice.generation == 0
                || notice.generation == self.current.load(Ordering::Acquire)
            {
                return Some(notice);
            }
        }
        None
    }
}

struct Playing {
    generation: u64,
    hash: String,
    runtime: runtime::Runtime,
    achievements: BTreeMap<u32, storage::Achievement>,
    previous: Option<(u64, u64)>,
    warned_gap: bool,
    catalog: storage::Game,
    unlocked: std::collections::BTreeSet<u32>,
    unsupported: std::collections::BTreeSet<u32>,
    state: GameAchievementState,
    dirty: bool,
}

fn run(
    root: PathBuf,
    enabled: Arc<AtomicBool>,
    controls: mpsc::Receiver<Control>,
    frames: mpsc::Receiver<Frame>,
    notices: mpsc::Sender<Notice>,
    status: Arc<Status>,
    transport: impl network::Transport + Send + 'static,
) {
    let config_path = root.join("Config/retroachievements.toml");
    let mut config = if config_path.exists() {
        match Config::read(&config_path) {
            Ok(config) => config,
            Err(error) => {
                status.set(SyncStatus::Attention);
                status.account.lock().unwrap().message = error.into();
                Config::default()
            }
        }
    } else {
        Config::default()
    };
    let initial_store = if config.username.is_empty() {
        Store {
            dir: root.join("Saves/RetroAchievements/idle"),
            unlocks: BTreeMap::new(),
        }
    } else {
        match Store::open(&root, &config.username) {
            Ok(store) => store,
            Err(error) => {
                status.set(SyncStatus::Attention);
                status.account.lock().unwrap().message = error;
                let dir = storage::account_dir(&root, &config.username);
                config.username.clear();
                Store {
                    dir,
                    unlocks: BTreeMap::new(),
                }
            }
        }
    };
    let ready = config.enabled && !config.username.trim().is_empty();
    if !status.account.lock().unwrap().busy {
        enabled.store(ready, Ordering::Release);
    }
    let store = Arc::new(Mutex::new(initial_store));
    let (loads, load_rx) = mpsc::channel();
    let (prepared_tx, prepared_rx) = mpsc::channel();
    let network_store = store.clone();
    let network_notices = notices.clone();
    let cached_prepared = prepared_tx.clone();
    let network_status = status.clone();
    if ready {
        status.set(SyncStatus::Syncing);
    }
    let network_enabled = enabled.clone();
    if std::thread::Builder::new()
        .name("slot-ra-http".into())
        .spawn(move || {
            network::run(
                root,
                config,
                network_store,
                load_rx,
                prepared_tx,
                network_notices,
                network_status,
                network_enabled,
                transport,
            );
        })
        .is_err()
    {
        status.set(SyncStatus::Attention);
        let _ = notices.send(Notice::status(0, "Achievements network worker failed"));
        return;
    }
    let mut generation = 0;
    let mut epoch = status.epoch.load(Ordering::Acquire);
    let mut current_load: Option<network::Load> = None;
    let mut account_pending = false;
    let mut snapshot_state = GameAchievementState::Disabled;
    let mut snapshot_title = String::new();
    let mut snapshot_dirty = true;
    let mut revision = 0;
    let mut last_snapshot = status.snapshot.lock().unwrap().snapshot.clone();
    let mut last_epoch = epoch;
    let mut snapshot_updated = Instant::now();
    let mut playing: Option<Playing> = None;
    let mut unsaved = BTreeMap::new();
    let mut retried = Instant::now();
    let mut presence_updated = Instant::now();
    loop {
        if retried.elapsed() >= Duration::from_secs(1) {
            retry_unsaved(&mut unsaved, &store, &notices);
            retried = Instant::now();
        }
        loop {
            if !enabled.load(Ordering::Acquire) {
                playing = None;
            }
            // Frames queued before an eject/load must be evaluated before retiring that game.
            for _ in 0..QUEUE_SIZE {
                let Ok(frame) = frames.try_recv() else {
                    break;
                };
                if !enabled.load(Ordering::Acquire) {
                    playing = None;
                }
                evaluate(frame, &mut playing, &store, &notices, &mut unsaved);
            }
            match controls.try_recv() {
                Ok(Control::Flush(done)) => {
                    retry_unsaved(&mut unsaved, &store, &notices);
                    done.store(true, Ordering::Release);
                }
                Ok(Control::Account(control)) => {
                    retry_unsaved(&mut unsaved, &store, &notices);
                    epoch = status.epoch.load(Ordering::Acquire);
                    snapshot_dirty = true;
                    if !unsaved.is_empty() && !matches!(&control, AccountControl::SetEnabled(_)) {
                        let mut account = status.account.lock().unwrap();
                        account.busy = false;
                        account.message = "Cannot change account until unlocks are saved".into();
                        enabled.store(account.enabled && account.signed_in, Ordering::Release);
                        continue;
                    }
                    generation = 0;
                    account_pending = true;
                    snapshot_state = GameAchievementState::SigningIn;
                    snapshot_dirty = true;
                    playing = None;
                    *status.presence.lock().unwrap() = None;
                    let _ = loads.send(network::Command::Account(control));
                }
                Ok(Control::Load(mut load)) => {
                    epoch = status.epoch.load(Ordering::Acquire);
                    load.epoch = epoch;
                    current_load = Some(load.clone());
                    snapshot_title = load
                        .path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    snapshot_state = if !load.supported {
                        GameAchievementState::NotSupportedPlatform
                    } else {
                        GameAchievementState::Loading
                    };
                    snapshot_dirty = true;
                    generation = load.generation;
                    playing = None;
                    *status.presence.lock().unwrap() = None;
                    // Cached games can start evaluating even while the HTTP worker is
                    // waiting on a timeout or preparing another ROM in the library.
                    let dir = store.lock().unwrap().dir.clone();
                    if load.supported
                        && !account_pending
                        && !status.account.lock().unwrap().busy
                        && storage::read::<storage::Auth>(&dir.join("auth.json")).is_ok()
                    {
                        if let Ok(hash) = library::hash(&dir, &load.path) {
                            if let Ok((game, unlocked)) =
                                storage::read::<(storage::Game, std::collections::BTreeSet<u32>)>(
                                    &dir.join(format!("{hash}.json")),
                                )
                            {
                                if game.console == 5 {
                                    let _ = cached_prepared.send(network::Prepared {
                                        generation,
                                        epoch,
                                        error: None,
                                        hash,
                                        game,
                                        unlocked,
                                        cached: true,
                                    });
                                }
                            }
                        }
                    }
                    let _ = loads.send(network::Command::Load(
                        (load.supported && enabled.load(Ordering::Acquire)).then_some(load),
                    ));
                }
                Ok(Control::Unload(id))
                    if current_load
                        .as_ref()
                        .is_some_and(|load| load.generation == id) =>
                {
                    current_load = None;
                    snapshot_title.clear();
                    snapshot_dirty = true;
                    generation = 0;
                    playing = None;
                    *status.presence.lock().unwrap() = None;
                    let _ = loads.send(network::Command::Load(None));
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if account_pending && !status.account.lock().unwrap().busy {
            account_pending = false;
            if let Some(load) = current_load.clone() {
                let mut load = load;
                load.epoch = epoch;
                generation = load.generation;
                current_load = Some(load.clone());
                snapshot_state = if load.supported {
                    GameAchievementState::Loading
                } else {
                    GameAchievementState::NotSupportedPlatform
                };
                let _ = loads.send(network::Command::Load(
                    (load.supported && enabled.load(Ordering::Acquire)).then_some(load),
                ));
            }
            snapshot_dirty = true;
        }
        for prepared in prepared_rx.try_iter() {
            if prepared.generation != generation
                || prepared.epoch != epoch
                || !enabled.load(Ordering::Acquire)
            {
                continue;
            }
            if let Some(error) = prepared.error {
                if let Some(game) = playing.as_mut() {
                    game.state = if error == GameAchievementState::Ready {
                        snapshot::catalog_state(&game.catalog)
                    } else {
                        error.clone()
                    };
                    game.dirty = true;
                }
                snapshot_state = playing.as_ref().map_or(error, |game| game.state.clone());
                snapshot_dirty = true;
                continue;
            }
            if playing.is_none() {
                let Some(runtime) = runtime::Runtime::new() else {
                    snapshot_state =
                        GameAchievementState::Error("Achievement runtime unavailable".into());
                    snapshot_dirty = true;
                    continue;
                };
                playing = Some(Playing {
                    generation,
                    hash: prepared.hash.clone(),
                    runtime,
                    achievements: BTreeMap::new(),
                    previous: None,
                    warned_gap: false,
                    catalog: prepared.game.clone(),
                    unlocked: Default::default(),
                    unsupported: Default::default(),
                    state: GameAchievementState::Loading,
                    dirty: true,
                });
            }
            let game = playing.as_mut().unwrap();
            snapshot::prepare(
                game,
                prepared.game,
                prepared.unlocked,
                &store.lock().unwrap(),
                &unsaved,
            );
            snapshot_state = game.state.clone();
            let state = if prepared.cached { "cached" } else { "online" };
            let _ = notices.send(Notice {
                badge: None,
                generation,
                title: if game.catalog.id == 0 {
                    "No achievements for this ROM".into()
                } else {
                    format!("Achievements ready ({state}, softcore)")
                },
                detail: if game.unsupported.is_empty() {
                    game.catalog.title.clone()
                } else {
                    format!(
                        "{} - {} unsupported",
                        game.catalog.title,
                        game.unsupported.len()
                    )
                },
                kind: NoticeKind::Status,
            });
            snapshot_dirty = true;
        }
        if !enabled.load(Ordering::Acquire) {
            playing = None;
        }
        match frames.recv_timeout(Duration::from_millis(5)) {
            Ok(frame) => {
                if !enabled.load(Ordering::Acquire) {
                    playing = None;
                }
                evaluate(frame, &mut playing, &store, &notices, &mut unsaved);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if presence_updated.elapsed() >= Duration::from_secs(1) {
            *status.presence.lock().unwrap() = playing
                .as_ref()
                .map(|game| (game.generation, game.runtime.presence()));
            presence_updated = Instant::now();
        }
        if snapshot_dirty
            || playing.as_ref().is_some_and(|game| game.dirty)
            || snapshot_updated.elapsed() >= Duration::from_secs(1)
        {
            let account = status.account.lock().unwrap().clone();
            let state = if current_load.as_ref().is_some_and(|load| !load.supported) {
                GameAchievementState::NotSupportedPlatform
            } else if account.busy || account_pending {
                GameAchievementState::SigningIn
            } else if !account.enabled {
                GameAchievementState::Disabled
            } else if !account.signed_in {
                GameAchievementState::SignedOut
            } else {
                snapshot_state.clone()
            };
            let mut next = if let Some(game) = playing.as_ref() {
                snapshot::build(game, &store.lock().unwrap(), &unsaved)
            } else {
                GameAchievementSnapshot::empty(
                    current_load.as_ref().map_or(0, |load| load.generation),
                    state.clone(),
                    snapshot_title.clone(),
                )
            };
            next.state = state;
            if next.game_title.is_empty() {
                next.game_title = snapshot_title.clone();
            }
            // All allocation, runtime queries and filesystem checks precede publication.
            next.revision = last_snapshot.revision;
            if last_epoch != epoch || *last_snapshot != next {
                revision += 1;
                next.revision = revision;
                let next = Arc::new(next);
                *status.snapshot.lock().unwrap() = snapshot::Published {
                    epoch,
                    snapshot: next.clone(),
                };
                last_snapshot = next;
                last_epoch = epoch;
            }
            if let Some(game) = playing.as_mut() {
                game.dirty = false;
            }
            snapshot_dirty = false;
            snapshot_updated = Instant::now();
        }
        status.unsaved.store(!unsaved.is_empty(), Ordering::Release);
    }
}

fn evaluate(
    frame: Frame,
    playing: &mut Option<Playing>,
    store: &Arc<Mutex<Store>>,
    notices: &mpsc::Sender<Notice>,
    unsaved: &mut BTreeMap<u32, (Unlock, Notice)>,
) {
    if let Some(game) = playing
        .as_mut()
        .filter(|g| g.generation == frame.generation)
    {
        if let Some((sequence, timeline)) = game.previous {
            if timeline != frame.timeline || sequence + 1 != frame.sequence {
                game.runtime.reset();
                if timeline == frame.timeline && !game.warned_gap {
                    let _ = notices.send(Notice::status(
                        game.generation,
                        "Achievement tracking interrupted",
                    ));
                    game.warned_gap = true;
                }
            }
        }
        game.previous = Some((frame.sequence, frame.timeline));
        if frame.valid[0] == 0 || frame.valid[1] == 0 {
            if !game.warned_gap {
                let _ = notices.send(Notice::status(
                    game.generation,
                    "Core does not expose achievement RAM",
                ));
                game.warned_gap = true;
            }
        } else {
            let earned = game.runtime.frame(&frame.ram, &frame.valid).to_vec();
            for id in earned {
                let unlock = Unlock {
                    id,
                    hash: game.hash.clone(),
                    earned_at: frame.earned_at,
                    synced: false,
                };
                if let Some(achievement) = game.achievements.get(&id) {
                    let notice = Notice {
                        badge: badges::path(&store.lock().unwrap().dir, &achievement.badge),
                        generation: game.generation,
                        title: format!("{} (+{})", achievement.title, achievement.points),
                        detail: achievement.description.clone(),
                        kind: NoticeKind::Earned,
                    };
                    match store.lock().unwrap().record(unlock.clone()) {
                        Ok(true) => {
                            let _ = notices.send(notice);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let _ = notices.send(Notice::status(game.generation, &error));
                            unsaved.insert(id, (unlock, notice));
                        }
                    }
                }
                game.runtime.deactivate(id);
                game.unlocked.insert(id);
                game.dirty = true;
            }
        }
    }
    let _ = frame.recycle.try_send(frame.ram);
}

fn retry_unsaved(
    unsaved: &mut BTreeMap<u32, (Unlock, Notice)>,
    store: &Arc<Mutex<Store>>,
    notices: &mpsc::Sender<Notice>,
) {
    unsaved.retain(
        |_, (unlock, notice)| match store.lock().unwrap().record(unlock.clone()) {
            Ok(true) => {
                let _ = notices.send(notice.clone());
                false
            }
            Ok(false) => false,
            Err(_) => true,
        },
    );
}

/// Canonical title from the achievement cache identified by this ROM's content hash.
/// Call on an I/O worker. This does not authenticate, send ROM data, or update the shared
/// library index. A renamed ROM can reuse the same cached identity immediately.
pub fn artwork_title(root: &std::path::Path, rom: &std::path::Path) -> Option<String> {
    let config = storage::Config::read(&root.join("Config/retroachievements.toml")).ok()?;
    if !config.enabled || config.username.is_empty() {
        return None;
    }
    let account = format!("{:x}", md5::compute(config.username.to_lowercase()));
    let dir = root.join("Saves/RetroAchievements").join(account);
    let hash = library::content_hash(rom).ok()?;
    let (game, _): (storage::Game, std::collections::BTreeSet<u32>) =
        storage::read(&dir.join(format!("{hash}.json"))).ok()?;
    (game.console == 5
        && game.id != 0
        && !game.title.trim().is_empty()
        && !game.title.starts_with("Unsupported Game Version"))
    .then_some(game.title)
}
