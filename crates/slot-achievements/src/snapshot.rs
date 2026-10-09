use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::{badges, storage, Playing};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GameAchievementState {
    Disabled,
    SignedOut,
    SigningIn,
    Loading,
    Unrecognized,
    NoAchievements,
    Ready,
    ServerBusy,
    Offline(String),
    Error(String),
    NotSupportedPlatform,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AchievementSummary {
    pub unlocked: u32,
    pub total: u32,
    pub points_earned: u32,
    pub points_total: u32,
    pub unsupported: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AchievementView {
    pub id: u32,
    pub title: String,
    pub description: String,
    pub points: u32,
    pub unlocked: bool,
    /// Only locally recorded awards have a timestamp.
    pub unlocked_at: Option<u64>,
    pub badge_path: Option<PathBuf>,
    pub progress: Option<(u32, u32)>,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameAchievementSnapshot {
    pub generation: u64,
    pub revision: u64,
    pub state: GameAchievementState,
    pub game_id: Option<u32>,
    pub game_title: String,
    pub unsynced_unlocks: u32,
    pub summary: AchievementSummary,
    pub achievements: Vec<AchievementView>,
}

impl GameAchievementSnapshot {
    pub fn empty(generation: u64, state: GameAchievementState, game_title: String) -> Self {
        Self {
            generation,
            revision: 0,
            state,
            game_id: None,
            game_title,
            unsynced_unlocks: 0,
            summary: AchievementSummary::default(),
            achievements: Vec::new(),
        }
    }
}

pub(crate) struct Published {
    pub epoch: u64,
    pub snapshot: Arc<GameAchievementSnapshot>,
}

impl Default for Published {
    fn default() -> Self {
        Self {
            epoch: 0,
            snapshot: Arc::new(GameAchievementSnapshot::empty(
                0,
                GameAchievementState::Disabled,
                String::new(),
            )),
        }
    }
}

pub(crate) fn catalog_state(game: &storage::Game) -> GameAchievementState {
    if game.console != 5 {
        GameAchievementState::NotSupportedPlatform
    } else if game.id == 0 {
        GameAchievementState::Unrecognized
    } else if !game.achievements.iter().any(|a| a.flags == 3) {
        GameAchievementState::NoAchievements
    } else {
        GameAchievementState::Ready
    }
}

pub(crate) fn unsynced_unlocks(
    store: &storage::Store,
    pending: &BTreeMap<u32, (storage::Unlock, crate::Notice)>,
) -> u32 {
    let count = store
        .unlocks
        .values()
        .filter(|unlock| !unlock.synced)
        .count()
        + pending
            .keys()
            .filter(|id| !store.unlocks.contains_key(id))
            .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

pub(crate) fn build(
    game: &Playing,
    store: &storage::Store,
    pending: &BTreeMap<u32, (storage::Unlock, crate::Notice)>,
) -> GameAchievementSnapshot {
    let mut snapshot = GameAchievementSnapshot::empty(
        game.generation,
        game.state.clone(),
        game.catalog.title.clone(),
    );
    snapshot.unsynced_unlocks = unsynced_unlocks(store, pending);
    snapshot.game_id = (game.catalog.id != 0).then_some(game.catalog.id);
    for a in game.catalog.achievements.iter().filter(|a| a.flags == 3) {
        let local = store
            .unlocks
            .get(&a.id)
            .or_else(|| pending.get(&a.id).map(|p| &p.0));
        let unlocked = game.unlocked.contains(&a.id) || local.is_some();
        let supported = !game.unsupported.contains(&a.id);
        snapshot.summary.total += 1;
        snapshot.summary.points_total = snapshot.summary.points_total.saturating_add(a.points);
        snapshot.summary.unsupported += u32::from(!supported);
        if unlocked {
            snapshot.summary.unlocked += 1;
            snapshot.summary.points_earned =
                snapshot.summary.points_earned.saturating_add(a.points);
        }
        snapshot.achievements.push(AchievementView {
            id: a.id,
            title: a.title.clone(),
            description: a.description.clone(),
            points: a.points,
            unlocked,
            unlocked_at: local.map(|u| u.earned_at),
            badge_path: badges::path(&store.dir, &a.badge).filter(|path| path.is_file()),
            progress: (!unlocked && supported)
                .then(|| game.runtime.measured(a.id))
                .flatten(),
            supported,
        });
    }
    snapshot
}

pub(crate) fn prepare(
    game: &mut Playing,
    catalog: storage::Game,
    unlocked: BTreeSet<u32>,
    store: &storage::Store,
    pending: &BTreeMap<u32, (storage::Unlock, crate::Notice)>,
) {
    game.runtime.activate_presence(&catalog.presence);
    let mut updated = BTreeMap::new();
    let mut unsupported = BTreeSet::new();
    for a in catalog.achievements.iter().filter(|a| a.flags == 3) {
        let unchanged = game
            .achievements
            .get(&a.id)
            .is_some_and(|old| old.definition == a.definition);
        // Validate even unlocked entries so support and totals describe the complete catalog.
        let supported = unchanged || game.runtime.activate(a.id, &a.definition);
        if !supported {
            unsupported.insert(a.id);
        } else if !unlocked.contains(&a.id)
            && !store.unlocks.contains_key(&a.id)
            && !pending.contains_key(&a.id)
        {
            updated.insert(a.id, a.clone());
        } else {
            game.runtime.deactivate(a.id);
        }
    }
    for id in game
        .achievements
        .keys()
        .filter(|id| !updated.contains_key(id))
    {
        game.runtime.deactivate(*id);
    }
    game.achievements = updated;
    game.unsupported = unsupported;
    game.unlocked = unlocked;
    game.state = catalog_state(&catalog);
    game.catalog = catalog;
    game.dirty = true;
}
