use super::*;
use serde_json::{json, Value};
use slot_retro::{AvInfo, ButtonMask, CoreError};
use std::path::Path;
use storage::{Achievement, Auth, Game};

fn achievement(definition: &str) -> Achievement {
    Achievement {
        badge: "12345".into(),
        id: 7,
        title: "Test unlocked".into(),
        description: "A real runtime trigger".into(),
        points: 5,
        flags: 3,
        definition: definition.into(),
    }
}

fn game() -> Game {
    Game {
        presence: String::new(),
        id: 1,
        console: 5,
        title: "Test game".into(),
        achievements: vec![achievement("0xH000000=1")],
    }
}

fn configured() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("Config")).unwrap();
    std::fs::create_dir_all(root.path().join("Games/GBA")).unwrap();
    std::fs::write(
        root.path().join("Config/retroachievements.toml"),
        "enabled = true\nusername = 'Player'\ntoken = 'fixture-token'\n",
    )
    .unwrap();
    root
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "worker did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn toml_config_accepts_comments_and_literal_credentials_without_leaking_errors() {
    let root = configured();
    let path = root.path().join("Config/retroachievements.toml");
    std::fs::write(
        &path,
        "# Account\nenabled = true\nusername = 'Player'\npassword = 'secret\\with#characters'\n",
    )
    .unwrap();
    let config = Config::read(&path).unwrap();
    assert!(config.enabled);
    assert_eq!(config.password, "secret\\with#characters");
    assert!(config.token.is_empty());
    std::fs::write(&path, "password = 'secret'\nenabeld = true\n").unwrap();
    assert!(matches!(
        Config::read(&path),
        Err("Invalid achievement config")
    ));
    let service = Service::start_with(root.path().into(), Offline);
    wait_for(|| service.sync_status() == SyncStatus::Attention);
    assert!(!service.enabled.load(Ordering::Acquire));
}

struct Offline;
impl network::Transport for Offline {
    fn call(&mut self, _: &[(&str, String)]) -> Result<Value, network::Failure> {
        Err(network::Failure::Network)
    }
}

struct Server {
    calls: mpsc::Sender<String>,
}

#[test]
fn wifi_reconnect_wakes_offline_sync_without_restarting_or_opening_a_game() {
    struct Reconnect {
        online: Arc<AtomicBool>,
        server: Server,
    }
    impl network::Transport for Reconnect {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            if self.online.load(Ordering::Acquire) {
                self.server.call(fields)
            } else {
                Err(network::Failure::Network)
            }
        }
    }
    let root = configured();
    let mut store = Store::open(root.path(), "Player").unwrap();
    store
        .record(Unlock {
            id: 7,
            hash: "abcdef".into(),
            earned_at: network::now(),
            synced: false,
        })
        .unwrap();
    let online = Arc::new(AtomicBool::new(false));
    let (calls, _) = mpsc::channel();
    let service = Service::start_with(
        root.path().into(),
        Reconnect {
            online: online.clone(),
            server: Server { calls },
        },
    );
    wait_for(|| service.sync_status() == SyncStatus::Offline);
    online.store(true, Ordering::Release);
    service.network_available();
    // wait_for's five-second deadline is much shorter than the thirty-second backoff.
    wait_for(|| service.sync_status() == SyncStatus::Ready);
    assert!(Store::open(root.path(), "Player").unwrap().unlocks[&7].synced);
}
impl network::Transport for Server {
    fn badge(&mut self, name: &str) -> Result<Vec<u8>, network::Failure> {
        let _ = self.calls.send(format!("badge:{name}"));
        Ok(badge_png())
    }
    fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
        let request = fields.iter().find(|(k, _)| *k == "r").unwrap().1.as_str();
        let _ = self.calls.send(request.into());
        Ok(match request {
            "login2" => json!({"Success":true,"User":"Player","Token":"fixture-token"}),
            "gameid" => json!({"Success":true,"GameID":1}),
            "patch" => json!({"Success":true,"PatchData":game()}),
            "unlocks" => json!({"Success":true,"UserUnlocks":[]}),
            "startsession" => json!({"Success":true,"Unlocks":[],"HardcoreUnlocks":[]}),
            "ping" => {
                assert!(fields
                    .iter()
                    .any(|(k, v)| *k == "m" && v == "Playing Test game"));
                assert!(fields.iter().any(|(k, v)| *k == "x" && v.len() == 32));
                json!({"Success":true})
            }
            "awardachievement" => json!({"Success":true}),
            _ => panic!("unexpected API {request}"),
        })
    }
}

fn badge_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[40, 120, 200, 255].repeat(4))
            .unwrap();
    }
    bytes
}

#[test]
fn badge_cache_survives_restart_and_repairs_corrupt_images() {
    let root = configured();
    let store = Store::open(root.path(), "Player").unwrap();
    let path = badges::path(&store.dir, "12345").unwrap();
    let (calls, requests) = mpsc::channel();
    let mut http = Server { calls };
    let mut queue = badges::Queue::new(&store.dir);
    queue.enqueue(&game());
    queue.enqueue(&game());
    queue.step(&mut http);
    assert!(!queue.pending());
    assert_eq!(requests.try_iter().count(), 1);
    let mut restarted = badges::Queue::new(&store.dir);
    restarted.enqueue(&game());
    assert!(!restarted.pending());
    // Notifications carry a local path even offline, with no network dependency.
    assert_eq!(load_badge(&path).unwrap().width, 2);
    std::fs::write(&path, b"corrupted PNG").unwrap();
    restarted.enqueue(&game());
    restarted.step(&mut http);
    assert!(load_badge(&path).is_some());
    assert!(badges::path(&store.dir, "../../auth").is_none());
    assert!(badges::path(&store.dir, "https://example.org/badge").is_none());
}

#[test]
fn a_badge_failure_does_not_hold_up_awards_or_cached_game_data() {
    struct NoBadges(Server);
    impl network::Transport for NoBadges {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            self.0.call(fields)
        }
    }
    let root = configured();
    std::fs::write(root.path().join("Games/GBA/Test.gba"), b"fixture").unwrap();
    let mut store = Store::open(root.path(), "Player").unwrap();
    store
        .record(Unlock {
            id: 7,
            hash: "abcdef".into(),
            earned_at: network::now(),
            synced: false,
        })
        .unwrap();
    let (calls, _) = mpsc::channel();
    let service = Service::start_with(root.path().into(), NoBadges(Server { calls }));
    wait_for(|| service.sync_status() == SyncStatus::Attention);
    let store = Store::open(root.path(), "Player").unwrap();
    assert!(store.unlocks[&7].synced);
    let hash = format!("{:x}", md5::compute(b"fixture"));
    assert!(store.dir.join(format!("{hash}.json")).exists());
    assert!(load_badge(&badges::path(&store.dir, "12345").unwrap()).is_none());
}

#[derive(Default)]
struct TestCore {
    value: u8,
}
impl RetroCore for TestCore {
    fn load(&mut self, _: &Path) -> Result<(), CoreError> {
        Ok(())
    }
    fn run_frame(&mut self, input: ButtonMask) {
        self.value = u8::from(input.0 != 0);
    }
    fn video_xrgb8888(&self) -> &[u8] {
        &[]
    }
    fn take_audio(&mut self) -> Vec<i16> {
        vec![]
    }
    fn serialize(&mut self) -> Result<Vec<u8>, CoreError> {
        Ok(vec![self.value])
    }
    fn unserialize(&mut self, _: &[u8]) -> Result<(), CoreError> {
        Ok(())
    }
    fn save_ram(&self) -> Option<Vec<u8>> {
        None
    }
    fn load_save_ram(&mut self, _: &[u8]) -> Result<(), CoreError> {
        Ok(())
    }
    fn av_info(&self) -> AvInfo {
        AvInfo {
            fps: 60.0,
            sample_rate: 48000.0,
        }
    }
    fn set_cheats(&mut self, codes: &[String]) -> bool {
        self.value = codes.len() as u8;
        !codes.iter().any(|code| code == "unsupported")
    }
    fn achievement_memory(&self, ram: &mut [u8]) -> [usize; 3] {
        ram[0] = self.value;
        [0x8000, 0x40000, 0]
    }
}

#[test]
fn runtime_evaluates_real_definitions_and_never_awards_missing_memory() {
    let mut runtime = runtime::Runtime::new().unwrap();
    assert!(runtime.activate(7, "0xH000000=1"));
    assert!(runtime.activate(8, "0xH048000=1"));
    assert!(!runtime.activate(9, "not a definition"));
    let mut ram = vec![0; RAM_SIZE];
    assert!(runtime.frame(&ram, &[0x8000, 0x40000, 0]).is_empty());
    ram[0] = 1;
    ram[0x48000] = 1;
    assert_eq!(runtime.frame(&ram, &[0x8000, 0x40000, 0]), &[7]);
    assert!(runtime.frame(&ram, &[0x8000, 0x40000, 0]).is_empty());
}

#[test]
fn offline_unlock_is_durable_then_syncs_without_reopening_the_game() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"fixture ROM").unwrap();
    let store = Store::open(root.path(), "Player").unwrap();
    let hash = library::hash(&store.dir, &rom).unwrap();
    storage::write(
        &store.dir.join("auth.json"),
        &Auth {
            username: "Player".into(),
            token: "fixture-token".into(),
        },
    )
    .unwrap();
    storage::write(
        &store.dir.join(format!("{hash}.json")),
        &(game(), std::collections::BTreeSet::<u32>::new()),
    )
    .unwrap();
    let service = Service::start_with(root.path().into(), Offline);
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service
            .take_notice()
            .is_some_and(|n| n.title.starts_with("Achievements ready"))
    });
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    // Eject immediately: queued final frames must be evaluated before Unload retires them.
    drop(core);
    wait_for(|| {
        service
            .take_notice()
            .is_some_and(|n| n.kind == NoticeKind::Earned)
    });
    let saved = Store::open(root.path(), "Player").unwrap();
    assert!(!saved.unlocks[&7].synced);
    assert_eq!(saved.unlocks[&7].hash, hash);
    wait_for(|| service.sync_status() == SyncStatus::Offline);
    drop(service);

    let restarted = Service::start_with(root.path().into(), Offline);
    let mut core = restarted.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        restarted
            .take_notice()
            .is_some_and(|n| n.title.starts_with("Achievements ready"))
    });
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    drop(core);
    // A reconnect uses the same persistent account ledger without needing a running core.
    drop(restarted);
    let (calls, requests) = mpsc::channel();
    let online = Service::start_with(root.path().into(), Server { calls });
    wait_for(|| {
        online
            .take_notice()
            .is_some_and(|n| n.title == "Achievements synced")
    });
    assert!(Store::open(root.path(), "Player").unwrap().unlocks[&7].synced);
    assert_eq!(
        requests
            .try_iter()
            .filter(|r| r == "awardachievement")
            .count(),
        1
    );
    assert!(Store::open(root.path(), "OtherPlayer")
        .unwrap()
        .unlocks
        .is_empty());
}

#[test]
fn an_unopened_library_is_prepared_automatically_without_starting_sessions() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Unopened.gba");
    std::fs::write(&rom, b"unopened ROM").unwrap();
    let (calls, requests) = mpsc::channel();
    let service = Service::start_with(root.path().into(), Server { calls });
    let store = Store::open(root.path(), "Player").unwrap();
    let hash = format!("{:x}", md5::compute(b"unopened ROM"));
    wait_for(|| store.dir.join(format!("{hash}.json")).exists());
    wait_for(|| service.sync_status() == SyncStatus::Ready);
    let badge = badges::path(&store.dir, "12345").unwrap();
    assert_eq!(
        load_badge(&badge).unwrap().rgba,
        [40, 120, 200, 255].repeat(4)
    );
    let (cached, _): (Game, std::collections::BTreeSet<u32>) =
        storage::read(&store.dir.join(format!("{hash}.json"))).unwrap();
    assert_eq!(cached.achievements[0].id, 7);
    let calls: Vec<_> = requests.try_iter().collect();
    assert!(calls.iter().any(|c| c == "unlocks"));
    assert!(!calls.iter().any(|c| c == "startsession"));
    wait_for(|| library::pending(root.path(), &store.dir, network::now()).is_empty());
    std::fs::write(&rom, b"changed content, different size").unwrap();
    assert_eq!(
        library::pending(root.path(), &store.dir, network::now()).len(),
        1
    );
    assert_ne!(library::hash(&store.dir, &rom).unwrap(), hash);
}

#[test]
fn a_full_worker_queue_cannot_block_the_emulator() {
    let (controls, _controls_rx) = mpsc::channel();
    let (frames, _frames_rx) = mpsc::sync_channel(QUEUE_SIZE);
    let (_, notices) = mpsc::channel();
    let service = Service {
        controls,
        frames,
        notices,
        enabled: Arc::new(AtomicBool::new(true)),
        current: Arc::new(AtomicU64::new(1)),
        flushing: AtomicBool::new(false),
        flushed: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Status::default()),
    };
    let mut core = service.wrap(Box::<TestCore>::default());
    let (done, completed) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        for _ in 0..1000 {
            core.run_frame(ButtonMask(1));
        }
        done.send(()).unwrap();
    });
    let result = completed.recv_timeout(Duration::from_secs(2));
    // Release the receiver even on failure, so a regressed blocking send can unwind.
    drop(_frames_rx);
    worker.join().unwrap();
    result.expect("emulation waited for the achievement worker");
}

#[test]
fn damaged_ledger_is_preserved_and_failed_writes_are_not_acknowledged() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path(), "Player").unwrap();
    let unlock = Unlock {
        id: 7,
        hash: "hash".into(),
        earned_at: 1000,
        synced: false,
    };
    store.record(unlock.clone()).unwrap();
    assert!(!store.record(unlock).unwrap());
    let path = store.dir.join("unlocks.json");
    std::fs::write(&path, b"broken").unwrap();
    assert!(Store::open(root.path(), "Player").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"broken");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.ack(7).is_err());
    assert!(!store.unlocks[&7].synced);
}

fn frame(sequence: u64, timeline: u64, value: u8) -> Frame {
    let mut ram = vec![0; RAM_SIZE];
    ram[0] = value;
    let (recycle, _) = mpsc::sync_channel(1);
    Frame {
        generation: 1,
        sequence,
        timeline,
        earned_at: 1000,
        ram,
        valid: [0x8000, 0x40000, 0],
        recycle,
    }
}

#[test]
fn dropped_frames_and_rewinds_reset_partial_hit_counts() {
    for timeline in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Mutex::new(Store::open(root.path(), "Player").unwrap()));
        let mut runtime = runtime::Runtime::new().unwrap();
        let a = achievement("0xH000000=1.2.");
        assert!(runtime.activate(a.id, &a.definition));
        let mut playing = Some(Playing {
            generation: 1,
            hash: "hash".into(),
            runtime,
            achievements: [(7, a)].into(),
            previous: None,
            warned_gap: false,
            catalog: game(),
            unlocked: Default::default(),
            unsupported: Default::default(),
            state: GameAchievementState::Ready,
            dirty: false,
        });
        let (notices, _) = mpsc::channel();
        let mut unsaved = BTreeMap::new();
        evaluate(frame(1, 0, 0), &mut playing, &store, &notices, &mut unsaved);
        evaluate(frame(2, 0, 1), &mut playing, &store, &notices, &mut unsaved);
        evaluate(
            frame(if timeline == 0 { 4 } else { 3 }, timeline, 1),
            &mut playing,
            &store,
            &notices,
            &mut unsaved,
        );
        assert!(store.lock().unwrap().unlocks.is_empty());
        evaluate(
            frame(5, timeline, 0),
            &mut playing,
            &store,
            &notices,
            &mut unsaved,
        );
        evaluate(
            frame(6, timeline, 1),
            &mut playing,
            &store,
            &notices,
            &mut unsaved,
        );
        evaluate(
            frame(7, timeline, 1),
            &mut playing,
            &store,
            &notices,
            &mut unsaved,
        );
        assert!(store.lock().unwrap().unlocks.contains_key(&7));
    }
}

#[test]
fn a_cached_game_can_earn_and_flush_while_https_is_stalled() {
    struct Stalled {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }
    impl network::Transport for Stalled {
        fn call(&mut self, _: &[(&str, String)]) -> Result<Value, network::Failure> {
            let _ = self.entered.send(());
            let _ = self.release.recv();
            Err(network::Failure::Network)
        }
    }
    let root = configured();
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"fixture ROM").unwrap();
    let store = Store::open(root.path(), "Player").unwrap();
    let hash = library::hash(&store.dir, &rom).unwrap();
    storage::write(
        &store.dir.join("auth.json"),
        &Auth {
            username: "Player".into(),
            token: "fixture-token".into(),
        },
    )
    .unwrap();
    storage::write(
        &store.dir.join(format!("{hash}.json")),
        &(game(), std::collections::BTreeSet::<u32>::new()),
    )
    .unwrap();
    let (entered, blocked) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let service = Service::start_with(
        root.path().into(),
        Stalled {
            entered,
            release: wait,
        },
    );
    blocked.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(service.sync_status(), SyncStatus::Syncing);
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service
            .take_notice()
            .is_some_and(|n| n.title.starts_with("Achievements ready"))
    });
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    wait_for(|| service.flush_ready());
    assert!(Store::open(root.path(), "Player")
        .unwrap()
        .unlocks
        .contains_key(&7));
    // No HTTPS response was needed for preparation, evaluation, notification, or flushing.
    drop(release);
}

#[test]
fn disabling_achievements_cannot_hold_up_poweroff() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::start_with(root.path().into(), Offline);
    wait_for(|| service.flush_ready());
    assert_eq!(service.sync_status(), SyncStatus::Disabled);
}

#[test]
fn unsaved_unlocks_retry_with_the_original_time_after_storage_recovers() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(root.path(), "Player").unwrap()));
    let path = store.lock().unwrap().dir.join("unlocks.json");
    std::fs::create_dir(&path).unwrap();
    let mut runtime = runtime::Runtime::new().unwrap();
    let a = achievement("0xH000000=1");
    runtime.activate(a.id, &a.definition);
    let mut playing = Some(Playing {
        generation: 1,
        hash: "hash".into(),
        runtime,
        achievements: [(7, a)].into(),
        previous: None,
        warned_gap: false,
        catalog: game(),
        unlocked: Default::default(),
        unsupported: Default::default(),
        state: GameAchievementState::Ready,
        dirty: false,
    });
    let (notices, received) = mpsc::channel();
    let mut unsaved = BTreeMap::new();
    evaluate(frame(1, 0, 0), &mut playing, &store, &notices, &mut unsaved);
    evaluate(frame(2, 0, 1), &mut playing, &store, &notices, &mut unsaved);
    assert_eq!(unsaved.len(), 1);
    assert!(store.lock().unwrap().unlocks.is_empty());
    assert!(!received.try_iter().any(|n| n.kind == NoticeKind::Earned));
    std::fs::remove_dir(path).unwrap();
    retry_unsaved(&mut unsaved, &store, &notices);
    assert!(unsaved.is_empty());
    assert_eq!(
        Store::open(root.path(), "Player").unwrap().unlocks[&7].earned_at,
        1000
    );
    assert!(received.try_iter().any(|n| n.kind == NoticeKind::Earned));
}

#[test]
fn cache_percentage_counts_unique_badges_and_resumes_from_disk() {
    let root = configured();
    let store = Store::open(root.path(), "Player").unwrap();
    let mut data = game();
    let mut second = achievement("0xH000000=2");
    second.id = 8;
    second.badge = "00002".into();
    data.achievements.push(second);
    let (calls, _) = mpsc::channel();
    let mut http = Server { calls };
    let mut queue = badges::Queue::new(&store.dir);
    queue.enqueue(&data);
    queue.enqueue(&data);
    assert_eq!(queue.percent(), 0);
    queue.step(&mut http);
    assert_eq!(queue.percent(), 50);
    queue.step(&mut Offline);
    assert_eq!(queue.percent(), 50); // Failures never count as completed downloads.
    let mut restarted = badges::Queue::new(&store.dir);
    restarted.enqueue(&data);
    assert_eq!(restarted.percent(), 50);
    restarted.step(&mut http);
    assert_eq!(restarted.percent(), 100);
    assert!(!restarted.pending());
}

#[test]
fn rich_presence_tracks_memory_and_clears_invalid_replacements() {
    let mut runtime = runtime::Runtime::new().unwrap();
    assert!(runtime.activate_presence("Display:\n?0xH000000=1?In the castle\nOn the map"));
    let mut ram = vec![0; RAM_SIZE];
    runtime.frame(&ram, &[0x8000, 0x40000, 0x10000]);
    assert_eq!(runtime.presence(), "On the map");
    ram[0] = 1;
    runtime.frame(&ram, &[0x8000, 0x40000, 0x10000]);
    assert_eq!(runtime.presence(), "In the castle");
    assert!(!runtime.activate_presence("not a valid script"));
    runtime.frame(&ram, &[0x8000, 0x40000, 0x10000]);
    assert!(runtime.presence().is_empty());
}

#[test]
fn rich_presence_cache_accepts_old_data_and_retains_new_scripts() {
    let mut value = serde_json::to_value(game()).unwrap();
    value.as_object_mut().unwrap().remove("RichPresencePatch");
    assert!(serde_json::from_value::<Game>(value)
        .unwrap()
        .presence
        .is_empty());
    let mut data = game();
    data.presence = "Display:\nOn the map".into();
    let root = configured();
    let path = root.path().join("game.json");
    storage::write(&path, &data).unwrap();
    assert_eq!(
        storage::read::<Game>(&path).unwrap().presence,
        data.presence
    );
}

#[test]
fn loading_a_game_sends_presence_without_waiting_for_heartbeat() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"fixture ROM").unwrap();
    let (calls, requests) = mpsc::channel();
    let service = Service::start_with(root.path().into(), Server { calls });
    wait_for(|| service.enabled.load(Ordering::Acquire));
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| requests.try_iter().any(|request| request == "ping"));
}

#[test]
fn null_rich_presence_from_the_service_is_an_empty_script() {
    let mut value = serde_json::to_value(game()).unwrap();
    value["RichPresencePatch"] = Value::Null;
    let parsed = serde_json::from_value::<Game>(value.clone()).unwrap();
    assert!(parsed.presence.is_empty());
    assert_eq!(parsed.achievements.len(), 1);
    value["RichPresencePatch"] = json!(42);
    assert!(serde_json::from_value::<Game>(value).is_err());
}

#[test]
fn artwork_identity_follows_rom_bytes_instead_of_its_filename() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Completely renamed.gba");
    std::fs::write(&rom, b"same rom bytes").unwrap();
    let hash = library::content_hash(&rom).unwrap();
    let store = Store::open(root.path(), "Player").unwrap();
    let mut data = game();
    data.title = "Tomb Raider: Legend".into();
    let ids = std::collections::BTreeSet::<u32>::new();
    storage::write(&store.dir.join(format!("{hash}.json")), &(&data, ids)).unwrap();
    assert_eq!(
        artwork_title(root.path(), &rom).as_deref(),
        Some("Tomb Raider: Legend")
    );
    let moved = rom.with_file_name("Yet another name.gba");
    std::fs::rename(&rom, &moved).unwrap();
    assert_eq!(
        artwork_title(root.path(), &moved).as_deref(),
        Some("Tomb Raider: Legend")
    );
    std::fs::write(&moved, b"different rom bytes").unwrap();
    assert!(artwork_title(root.path(), &moved).is_none());
}

#[test]
fn tracked_forwards_cheats_clearing_and_the_cores_result() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::start_with(root.path().into(), Offline);
    let mut tracked = service.wrap(Box::<TestCore>::default());
    assert!(tracked.set_cheats(&["code1".into(), "code2".into()]));
    assert_eq!(tracked.serialize().unwrap(), [2]);
    assert!(tracked.set_cheats(&[]));
    assert_eq!(tracked.serialize().unwrap(), [0]);
    assert!(!tracked.set_cheats(&["unsupported".into()]));
    assert_eq!(tracked.serialize().unwrap(), [1]);
}

struct AccountServer {
    requests: mpsc::Sender<Vec<(String, String)>>,
    reply: Result<Value, network::Failure>,
}
impl network::Transport for AccountServer {
    fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
        self.requests
            .send(
                fields
                    .iter()
                    .map(|(key, value)| (key.to_string(), value.clone()))
                    .collect(),
            )
            .ok();
        match &self.reply {
            Ok(reply) => Ok(reply.clone()),
            Err(_) => Err(network::Failure::Network),
        }
    }
}
fn sign_in(service: &Service) {
    service.account_control(AccountControl::SignIn {
        username: "Player".into(),
        password: Zeroizing::new("private-password".into()),
    });
    wait_for(|| !service.account_state().busy);
}
fn account_service(
    root: &Path,
    reply: Result<Value, network::Failure>,
) -> (Service, mpsc::Receiver<Vec<(String, String)>>) {
    let (requests, received) = mpsc::channel();
    (
        Service::start_with(root.into(), AccountServer { requests, reply }),
        received,
    )
}

#[test]
fn an_idle_worker_can_sign_in_save_toggle_resume_and_sign_out() {
    let root = tempfile::tempdir().unwrap();
    let (service, requests) = account_service(
        root.path(),
        Ok(json!({"Success":true,"User":"Player","Token":"saved-token"})),
    );
    wait_for(|| service.flush_ready());
    assert_eq!(service.sync_status(), SyncStatus::Disabled);
    sign_in(&service);
    assert_eq!(service.account_state().message, "Signed in as Player");
    assert!(service.account_state().signed_in);
    assert!(!service.account_state().enabled);
    let store = Store::open(root.path(), "Player").unwrap();
    let auth: Auth = storage::read(&store.dir.join("auth.json")).unwrap();
    assert_eq!(auth.token, "saved-token");
    let settings = root.path().join("Config/retroachievements.toml");
    let text = std::fs::read_to_string(&settings).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(parsed.as_table().unwrap().len(), 2);
    assert!(parsed.get("password").is_none());
    assert!(parsed.get("token").is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(store.dir.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&store.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(first.contains(&("p".into(), "private-password".into())));
    for on in [true, false, true] {
        service.account_control(AccountControl::SetEnabled(on));
        wait_for(|| !service.account_state().busy);
        assert_eq!(service.account_state().enabled, on);
        if on {
            let fields = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(fields.contains(&("t".into(), "saved-token".into())));
            assert!(!fields.iter().any(|(key, _)| key == "p"));
        } else {
            assert!(store.dir.join("auth.json").exists());
            assert!(!service.enabled.load(Ordering::Acquire));
            assert!(requests.recv_timeout(Duration::from_millis(250)).is_err());
        }
    }
    service.account_control(AccountControl::SignOut);
    wait_for(|| !service.account_state().busy);
    assert!(!service.account_state().signed_in);
    assert_eq!(service.account_state().message, "Signed out");
    assert!(!store.dir.join("auth.json").exists());
    assert!(Config::read(&settings).unwrap().username.is_empty());
    assert!(!service.enabled.load(Ordering::Acquire));
    assert!(requests.recv_timeout(Duration::from_millis(250)).is_err());
}

#[test]
fn sign_in_errors_are_safe_and_transport_is_distinct() {
    for (reply, expected) in [
        (
            Ok(json!({"Success":false,"Error":"Wrong credentials\n\u{1b}☃"})),
            "Wrong credentials",
        ),
        (
            Ok(json!({"Success":false,"Error":"No Player: private-password"})),
            "Sign in rejected",
        ),
        (
            Ok(json!({"Success":false,"Error":"pRiVaTe-PaSsWoRd"})),
            "Sign in rejected",
        ),
        (
            Ok(json!({"Success":false,"Error":"pLaYeR"})),
            "Sign in rejected",
        ),
        (
            Ok(json!({"Success":false,"Error":"Pla\u{1b}yer"})),
            "Sign in rejected",
        ),
        (
            Ok(json!({"Success":true,"User":"Player","Token":""})),
            "Invalid RetroAchievements response",
        ),
        (
            Ok(json!({"Success":true,"User":"Player"})),
            "Invalid RetroAchievements response",
        ),
        (
            Err(network::Failure::Network),
            "Can't reach RetroAchievements",
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let (service, _) = account_service(root.path(), reply);
        sign_in(&service);
        let account = service.account_state();
        assert_eq!(account.message, expected);
        assert!(!account.signed_in);
        assert!(!account.message.contains("Player"));
        assert!(!account.message.contains("private-password"));
        assert!(!root.path().join("Config/retroachievements.toml").exists());
    }
    let root = tempfile::tempdir().unwrap();
    let (service, _) = account_service(
        root.path(),
        Ok(json!({"Success":false,"Error":"x".repeat(200)})),
    );
    sign_in(&service);
    assert_eq!(service.account_state().message, "x".repeat(80));
}

#[test]
fn legacy_password_logs_in_and_a_screen_save_removes_it() {
    let root = configured();
    let path = root.path().join("Config/retroachievements.toml");
    std::fs::write(
        &path,
        "enabled = true\nusername = 'Player'\npassword = 'legacy-secret'\n",
    )
    .unwrap();
    let (service, requests) = account_service(
        root.path(),
        Ok(json!({"Success":true,"User":"Player","Token":"saved-token"})),
    );
    wait_for(|| service.account_state().signed_in);
    let fields = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(fields.contains(&("p".into(), "legacy-secret".into())));
    assert!(std::fs::read_to_string(&path).unwrap().contains("password"));
    service.account_control(AccountControl::SetEnabled(false));
    wait_for(|| !service.account_state().busy);
    let settings: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(settings.get("password").is_none());
    assert!(settings.get("token").is_none());
    assert!(Store::open(root.path(), "Player")
        .unwrap()
        .dir
        .join("auth.json")
        .exists());
}

#[test]
fn account_changes_retire_current_runtime_until_the_next_load() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"fixture ROM").unwrap();
    let (calls, _) = mpsc::channel();
    let service = Service::start_with(root.path().into(), Server { calls });
    wait_for(|| service.account_state().signed_in);
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service
            .take_notice()
            .is_some_and(|n| n.title.starts_with("Achievements ready"))
    });
    service.account_control(AccountControl::SetEnabled(false));
    wait_for(|| !service.account_state().busy);
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    service.account_control(AccountControl::SetEnabled(true));
    wait_for(|| !service.account_state().busy);
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    wait_for(|| service.flush_ready());
    assert!(Store::open(root.path(), "Player")
        .unwrap()
        .unlocks
        .is_empty());
    drop(core);
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service
            .take_notice()
            .is_some_and(|n| n.title.starts_with("Achievements ready"))
    });
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    wait_for(|| {
        Store::open(root.path(), "Player")
            .unwrap()
            .unlocks
            .contains_key(&7)
    });
}

#[test]
fn even_short_credentials_cannot_appear_in_rejection_fallbacks() {
    let root = tempfile::tempdir().unwrap();
    let (service, _) = account_service(
        root.path(),
        Ok(json!({"Success":false,"Error":"in Rejected"})),
    );
    service.account_control(AccountControl::SignIn {
        username: "in".into(),
        password: Zeroizing::new("Rejected".into()),
    });
    wait_for(|| !service.account_state().busy);
    let message = service.account_state().message;
    assert!(!message.to_ascii_lowercase().contains("in"));
    assert!(!message.to_ascii_lowercase().contains("rejected"));
    assert!(!message.is_empty());
    assert!(message.bytes().all(|b| (32..=126).contains(&b)));
}

#[test]
fn enabling_a_disabled_legacy_account_preserves_credentials_until_token_login_succeeds() {
    struct Recovering(Arc<AtomicBool>);
    impl network::Transport for Recovering {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            assert!(fields
                .iter()
                .any(|(key, value)| *key == "p" && value == "legacy-secret"));
            if self.0.load(Ordering::Acquire) {
                Ok(json!({"Success":true,"User":"Player","Token":"saved-token"}))
            } else {
                Err(network::Failure::Network)
            }
        }
    }
    let root = configured();
    let path = root.path().join("Config/retroachievements.toml");
    std::fs::write(
        &path,
        "enabled = false\nusername = 'Player'\npassword = 'legacy-secret'\n",
    )
    .unwrap();
    let online = Arc::new(AtomicBool::new(false));
    let service = Service::start_with(root.path().into(), Recovering(online.clone()));
    wait_for(|| service.account_state().username == "Player");
    service.account_control(AccountControl::SetEnabled(true));
    wait_for(|| !service.account_state().busy);
    wait_for(|| service.account_state().message == "Can't reach RetroAchievements");
    assert!(Config::read(&path).unwrap().enabled);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("legacy-secret"));
    online.store(true, Ordering::Release);
    service.network_available();
    wait_for(|| service.account_state().signed_in);
    let config = Config::read(&path).unwrap();
    assert!(config.enabled);
    assert!(config.password.is_empty());
    assert!(config.token.is_empty());
    let auth: Auth = storage::read(
        &Store::open(root.path(), "Player")
            .unwrap()
            .dir
            .join("auth.json"),
    )
    .unwrap();
    assert_eq!(auth.token, "saved-token");
}

#[test]
fn turning_off_during_a_request_prevents_following_session_requests() {
    struct StalledGame {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        server: Server,
    }
    impl network::Transport for StalledGame {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            if fields
                .iter()
                .any(|(key, value)| *key == "r" && value == "gameid")
            {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            self.server.call(fields)
        }
    }
    let root = configured();
    let (calls, requests) = mpsc::channel();
    let (entered, stalled) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let service = Service::start_with(
        root.path().into(),
        StalledGame {
            entered,
            release: released,
            server: Server { calls },
        },
    );
    wait_for(|| service.sync_status() == SyncStatus::Ready);
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"fixture ROM").unwrap();
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    stalled.recv_timeout(Duration::from_secs(2)).unwrap();
    service.account_control(AccountControl::SetEnabled(false));
    release.send(()).unwrap();
    wait_for(|| !service.account_state().busy);
    assert_eq!(service.sync_status(), SyncStatus::Disabled);
    assert!(!requests
        .try_iter()
        .any(|request| ["patch", "startsession", "ping"].contains(&request.as_str())));
}

fn run_account_control(root: &Path, command: AccountControl, http: impl network::Transport) {
    let config = Config::read(&root.join("Config/retroachievements.toml")).unwrap();
    let store = Arc::new(Mutex::new(Store::open(root, &config.username).unwrap()));
    let (controls, commands) = mpsc::channel();
    controls.send(network::Command::Account(command)).unwrap();
    drop(controls);
    let (prepared, _) = mpsc::channel();
    let (notices, _) = mpsc::channel();
    network::run(
        root.into(),
        config,
        store,
        commands,
        prepared,
        notices,
        Arc::new(Status::default()),
        Arc::new(AtomicBool::new(true)),
        http,
    );
}

#[test]
fn off_preserves_unverified_legacy_fields_without_transport_calls() {
    for credentials in [
        "password = 'legacy-secret' # handwritten\n",
        "token = 'legacy-token'\n",
        "password = 'legacy-secret'\ntoken = ''\n",
    ] {
        let root = configured();
        let path = root.path().join("Config/retroachievements.toml");
        let original = format!("enabled = true # keep comment\nusername = 'Player'\n{credentials}");
        std::fs::write(&path, &original).unwrap();
        let (requests, received) = mpsc::channel();
        run_account_control(
            root.path(),
            AccountControl::SetEnabled(false),
            AccountServer {
                requests,
                reply: Err(network::Failure::Network),
            },
        );
        assert!(!Config::read(&path).unwrap().enabled);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original.replacen("true", "false", 1)
        );
        assert_eq!(received.try_iter().count(), 0);
    }
}

#[test]
fn on_persists_before_verifying_expired_auth_and_keeps_password_fallback() {
    struct ExpiredToken {
        path: std::path::PathBuf,
        calls: usize,
    }
    impl network::Transport for ExpiredToken {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            let settings = Config::read(&self.path).unwrap();
            assert!(settings.enabled);
            assert_eq!(settings.password, "legacy-secret");
            assert!(settings.token.is_empty());
            self.calls += 1;
            match self.calls {
                1 => {
                    assert!(fields
                        .iter()
                        .any(|(k, v)| *k == "t" && v == "expired-token"));
                    Ok(json!({"Success":false,"Error":"Token expired"}))
                }
                2 => {
                    assert!(fields
                        .iter()
                        .any(|(k, v)| *k == "p" && v == "legacy-secret"));
                    Ok(json!({"Success":true,"User":"Player","Token":"replacement-token"}))
                }
                _ => panic!("unexpected request"),
            }
        }
    }
    let root = configured();
    let path = root.path().join("Config/retroachievements.toml");
    std::fs::write(
        &path,
        "enabled = false\nusername = 'Player'\npassword = 'legacy-secret'\n",
    )
    .unwrap();
    let store = Store::open(root.path(), "Player").unwrap();
    storage::write(
        &store.dir.join("auth.json"),
        &Auth {
            username: "Player".into(),
            token: "expired-token".into(),
        },
    )
    .unwrap();
    run_account_control(
        root.path(),
        AccountControl::SetEnabled(true),
        ExpiredToken {
            path: path.clone(),
            calls: 0,
        },
    );
    let text = std::fs::read_to_string(&path).unwrap();
    let saved: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(saved.as_table().unwrap().len(), 2);
    assert!(saved["enabled"].as_bool().unwrap());
    let auth: Auth = storage::read(&store.dir.join("auth.json")).unwrap();
    assert_eq!(auth.token, "replacement-token");
}

#[test]
fn settings_usernames_round_trip_toml_special_characters() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("Config/retroachievements.toml");
    for username in [
        "quotes\"'",
        "back\\slash",
        "ユニコード☃",
        "DEL\u{7f}",
        "\0\u{1}\u{8}\t\n\u{b}\u{c}\r\u{1f}",
    ] {
        let mut config = Config::default();
        config.username = username.into();
        config.save(root.path()).unwrap();
        assert_eq!(Config::read(&path).unwrap().username, username);
    }
}

#[test]
fn account_control_debug_redacts_password() {
    let control = AccountControl::SignIn {
        username: "Player".into(),
        password: Zeroizing::new("secret".into()),
    };
    let debug = format!("{control:?}");
    assert!(!debug.contains("secret"));
    assert!(debug.contains("password_length: 6"));
}

#[test]
fn login_body_encodes_fields_without_extra_capacity_or_plaintext_temporaries() {
    let fields = [
        ("r", "login2".into()),
        ("u", "a b+&=☃".into()),
        ("p", "\"\\?/#%\0\u{7f}*-._~".into()),
    ];
    let body = network::login_body(&fields);
    assert_eq!(
        body.as_slice(),
        b"r=login2&u=a+b%2B%26%3D%E2%98%83&p=%22%5C%3F%2F%23%25%00%7F*-._%7E"
    );
    assert_eq!(body.len(), body.capacity());
    assert!(network::login_body(&[]).is_empty());
}

fn snapshot_game() -> Playing {
    Playing {
        generation: 42,
        hash: "hash".into(),
        runtime: runtime::Runtime::new().unwrap(),
        achievements: BTreeMap::new(),
        previous: None,
        warned_gap: false,
        catalog: game(),
        unlocked: Default::default(),
        unsupported: Default::default(),
        state: GameAchievementState::Loading,
        dirty: false,
    }
}

#[test]
fn snapshots_keep_the_official_catalog_and_union_all_unlock_sources() {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(root.path(), "Player").unwrap();
    let mut catalog = game();
    catalog.achievements = (7..14)
        .map(|id| {
            let mut a = achievement("0xH000000=1");
            a.id = id;
            a.points = id;
            if id == 10 {
                a.definition = "invalid trigger".into();
            }
            if id == 11 {
                a.flags = 5;
            }
            if id == 12 {
                a.definition = "M:0xH000000=50".into();
            }
            a
        })
        .collect();
    store
        .record(Unlock {
            id: 8,
            hash: "hash".into(),
            earned_at: 100,
            synced: true,
        })
        .unwrap();
    let pending = [(
        9,
        (
            Unlock {
                id: 9,
                hash: "hash".into(),
                earned_at: 200,
                synced: false,
            },
            Notice::status(42, "pending"),
        ),
    )]
    .into();
    let mut playing = snapshot_game();
    snapshot::prepare(&mut playing, catalog, [7].into(), &store, &pending);
    let mut ram = vec![0; RAM_SIZE];
    playing.runtime.frame(&ram, &[0x8000, 0x40000, 0]);
    ram[0] = 12;
    playing.runtime.frame(&ram, &[0x8000, 0x40000, 0]);
    let view = snapshot::build(&playing, &store, &pending);
    assert_eq!(
        view.achievements.iter().map(|a| a.id).collect::<Vec<_>>(),
        [7, 8, 9, 10, 12, 13]
    );
    assert_eq!(
        view.summary,
        AchievementSummary {
            unlocked: 3,
            total: 6,
            points_earned: 24,
            points_total: 59,
            unsupported: 1
        }
    );
    assert!(view.achievements[..3].iter().all(|a| a.unlocked));
    assert_eq!(view.achievements[0].unlocked_at, None);
    assert_eq!(view.achievements[1].unlocked_at, Some(100));
    assert_eq!(view.achievements[2].unlocked_at, Some(200));
    assert!(!view.achievements[3].supported);
    assert_eq!(view.achievements[4].progress, Some((12, 50)));
    assert_eq!(view.achievements[5].progress, None);
    assert!(view.achievements.iter().all(|a| a.badge_path.is_none()));
    let badge = badges::path(&store.dir, "12345").unwrap();
    std::fs::create_dir_all(badge.parent().unwrap()).unwrap();
    std::fs::write(&badge, badge_png()).unwrap();
    assert_eq!(
        snapshot::build(&playing, &store, &pending).achievements[0].badge_path,
        Some(badge)
    );
    assert_eq!(playing.achievements.len(), 2);
}

#[test]
fn catalog_states_distinguish_unrecognised_zero_official_and_other_platforms() {
    let mut catalog = game();
    assert_eq!(
        snapshot::catalog_state(&catalog),
        GameAchievementState::Ready
    );
    catalog.achievements[0].flags = 5;
    assert_eq!(
        snapshot::catalog_state(&catalog),
        GameAchievementState::NoAchievements
    );
    catalog.achievements.clear();
    assert_eq!(
        snapshot::catalog_state(&catalog),
        GameAchievementState::NoAchievements
    );
    catalog.id = 0;
    assert_eq!(
        snapshot::catalog_state(&catalog),
        GameAchievementState::Unrecognized
    );
    catalog.console = 4;
    assert_eq!(
        snapshot::catalog_state(&catalog),
        GameAchievementState::NotSupportedPlatform
    );
}

#[test]
fn snapshot_reads_are_nonblocking_and_reject_stale_games_and_accounts() {
    let (controls, _control_rx) = mpsc::channel();
    let (frames, _frame_rx) = mpsc::sync_channel(QUEUE_SIZE);
    let (_, notices) = mpsc::channel();
    let service = Service {
        controls,
        frames,
        notices,
        enabled: Arc::new(AtomicBool::new(true)),
        current: Arc::new(AtomicU64::new(42)),
        flushing: AtomicBool::new(false),
        flushed: Arc::new(AtomicBool::new(false)),
        status: Arc::new(Status::default()),
    };
    let view = Arc::new(GameAchievementSnapshot::empty(
        42,
        GameAchievementState::Ready,
        "Test".into(),
    ));
    *service.status.snapshot.lock().unwrap() = snapshot::Published {
        epoch: 0,
        snapshot: view.clone(),
    };
    assert!(Arc::ptr_eq(&service.game_snapshot().unwrap(), &view));
    let locked = service.status.snapshot.lock().unwrap();
    assert!(service.game_snapshot().is_none());
    drop(locked);
    service.current.store(0, Ordering::Release);
    assert!(service.game_snapshot().is_none());
    service.current.store(43, Ordering::Release);
    assert!(service.game_snapshot().is_none());
    service.current.store(42, Ordering::Release);
    service.status.epoch.store(1, Ordering::Release);
    assert!(service.game_snapshot().is_none());
}

#[test]
fn worker_snapshots_report_disabled_signed_out_and_unsupported_loads() {
    for (enabled, extension, expected) in [
        (false, "gba", GameAchievementState::Disabled),
        (true, "gba", GameAchievementState::SignedOut),
        (false, "gb", GameAchievementState::NotSupportedPlatform),
    ] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Config")).unwrap();
        std::fs::write(
            root.path().join("Config/retroachievements.toml"),
            format!("enabled = {enabled}\n"),
        )
        .unwrap();
        let rom = root.path().join(format!("Test.{extension}"));
        std::fs::write(&rom, b"ROM").unwrap();
        let service = Service::start_with(root.path().into(), Offline);
        let mut core = service.wrap(Box::<TestCore>::default());
        core.load(&rom).unwrap();
        wait_for(|| service.game_snapshot().is_some_and(|s| s.state == expected));
        assert_eq!(
            service.game_snapshot().unwrap().generation,
            service.game_generation()
        );
    }
}

#[test]
fn online_unrecognised_and_empty_catalog_results_reach_the_snapshot() {
    struct CatalogServer {
        id: u32,
        empty: bool,
    }
    impl network::Transport for CatalogServer {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            let request = fields.iter().find(|(k, _)| *k == "r").unwrap().1.as_str();
            Ok(match request {
                "login2" => json!({"Success":true,"User":"Player","Token":"fixture-token"}),
                "gameid" => json!({"Success":true,"GameID":self.id}),
                "patch" => {
                    let mut catalog = game();
                    if self.empty {
                        catalog.achievements.clear();
                    }
                    json!({"Success":true,"PatchData":catalog})
                }
                "startsession" | "ping" => json!({"Success":true}),
                "unlocks" => json!({"Success":true,"UserUnlocks":[]}),
                _ => panic!("unexpected API {request}"),
            })
        }
    }
    for (id, empty, state) in [
        (0, false, GameAchievementState::Unrecognized),
        (1, true, GameAchievementState::NoAchievements),
        (1, false, GameAchievementState::Ready),
    ] {
        let root = configured();
        let rom = root.path().join("Games/GBA/Test.gba");
        std::fs::write(&rom, b"fixture ROM").unwrap();
        let service = Service::start_with(root.path().into(), CatalogServer { id, empty });
        let mut core = service.wrap(Box::<TestCore>::default());
        core.load(&rom).unwrap();
        wait_for(|| service.game_snapshot().is_some_and(|s| s.state == state));
        assert_eq!(
            service.game_snapshot().unwrap().game_id,
            (id != 0).then_some(id)
        );
    }
}

#[test]
fn an_online_game_publishes_ready_after_a_failed_sync_recovers() {
    struct Recovering {
        fail_next: Arc<AtomicBool>,
        server: Server,
    }
    impl network::Transport for Recovering {
        fn badge(&mut self, name: &str) -> Result<Vec<u8>, network::Failure> {
            self.server.badge(name)
        }

        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            if fields
                .iter()
                .any(|(key, value)| *key == "r" && value == "awardachievement")
                && self.fail_next.swap(false, Ordering::AcqRel)
            {
                return Err(network::Failure::Network);
            }
            self.server.call(fields)
        }
    }
    let root = configured();
    let rom = root.path().join("Test.gba");
    std::fs::write(&rom, b"ROM").unwrap();
    let fail_next = Arc::new(AtomicBool::new(true));
    let (calls, requests) = mpsc::channel();
    let service = Service::start_with(
        root.path().into(),
        Recovering {
            fail_next,
            server: Server { calls },
        },
    );
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service.sync_status() == SyncStatus::Ready
            && service
                .game_snapshot()
                .is_some_and(|s| s.state == GameAchievementState::Ready)
    });
    let generation = service.game_generation();
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    wait_for(|| {
        service.sync_status() == SyncStatus::Offline
            && service
                .game_snapshot()
                .is_some_and(|s| matches!(s.state, GameAchievementState::Offline(_)))
    });
    let failed = service.game_snapshot().unwrap();
    service.network_available();
    wait_for(|| {
        service.sync_status() == SyncStatus::Ready
            && service
                .game_snapshot()
                .is_some_and(|s| s.state == GameAchievementState::Ready)
    });
    let recovered = service.game_snapshot().unwrap();
    assert_eq!(recovered.generation, generation);
    assert_eq!(recovered.game_id, Some(1));
    assert_eq!(recovered.summary.unlocked, 1);
    assert!(recovered.revision > failed.revision);
    let requests: Vec<_> = requests.try_iter().collect();
    for request in ["gameid", "patch", "startsession"] {
        assert_eq!(requests.iter().filter(|r| r.as_str() == request).count(), 1);
    }
    assert_eq!(requests.iter().filter(|r| *r == "login2").count(), 2);
}

#[test]
fn loading_and_transport_errors_are_visible_without_a_catalog() {
    struct Stalled {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        failure: network::Failure,
    }
    impl network::Transport for Stalled {
        fn call(&mut self, _fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            let _ = self.started.send(());
            let _ = self.release.recv();
            Err(match self.failure {
                network::Failure::Network => network::Failure::Network,
                _ => network::Failure::Invalid,
            })
        }
    }
    for failure in [network::Failure::Network, network::Failure::Invalid] {
        let root = configured();
        let store = Store::open(root.path(), "Player").unwrap();
        storage::write(
            &store.dir.join("auth.json"),
            &Auth {
                username: "Player".into(),
                token: "fixture-token".into(),
            },
        )
        .unwrap();
        let rom = root.path().join("Games/GBA/Test.gba");
        std::fs::write(&rom, b"ROM").unwrap();
        let (started, requests) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let service = Service::start_with(
            root.path().into(),
            Stalled {
                started,
                release: waiting,
                failure,
            },
        );
        let mut core = service.wrap(Box::<TestCore>::default());
        core.load(&rom).unwrap();
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        wait_for(|| {
            service
                .game_snapshot()
                .is_some_and(|s| s.state == GameAchievementState::Loading)
        });
        release.send(()).unwrap();
        wait_for(|| {
            service.game_snapshot().is_some_and(|s| {
                matches!(
                    s.state,
                    GameAchievementState::Offline(_) | GameAchievementState::Error(_)
                )
            })
        });
        drop(release);
    }
}

#[test]
fn delayed_http_results_cannot_restore_an_ejected_game_or_signed_out_account() {
    struct Delayed {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        delayed: bool,
    }
    impl network::Transport for Delayed {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, network::Failure> {
            let request = fields.iter().find(|(k, _)| *k == "r").unwrap().1.as_str();
            Ok(match request {
                "login2" => json!({"Success":true,"User":"Player","Token":"fixture-token"}),
                "gameid" if !self.delayed => {
                    self.delayed = true;
                    self.started.send(()).unwrap();
                    self.release.recv().unwrap();
                    json!({"Success":true,"GameID":0})
                }
                "gameid" => json!({"Success":true,"GameID":1}),
                "patch" => json!({"Success":true,"PatchData":game()}),
                "startsession" | "ping" => json!({"Success":true}),
                "unlocks" => json!({"Success":true,"UserUnlocks":[]}),
                _ => panic!("unexpected API {request}"),
            })
        }
    }
    for sign_out in [false, true] {
        let root = configured();
        let rom = root.path().join("Games/GBA/Test.gba");
        std::fs::write(&rom, b"ROM").unwrap();
        // Keep background prefetch out of the request we deliberately stall.
        let store = Store::open(root.path(), "Player").unwrap();
        library::checked(&store.dir, &rom, network::now());
        storage::write(
            &store.dir.join("auth.json"),
            &Auth {
                username: "Player".into(),
                token: "fixture-token".into(),
            },
        )
        .unwrap();
        let (started, requests) = mpsc::channel();
        let (release, waiting) = mpsc::channel();
        let service = Service::start_with(
            root.path().into(),
            Delayed {
                started,
                release: waiting,
                delayed: false,
            },
        );
        let mut core = service.wrap(Box::<TestCore>::default());
        core.load(&rom).unwrap();
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        wait_for(|| {
            service
                .game_snapshot()
                .is_some_and(|s| s.state == GameAchievementState::Loading)
        });
        let old_generation = service.game_generation();
        if sign_out {
            service.account_control(AccountControl::SignOut);
            assert!(service.game_snapshot().is_none());
            wait_for(|| {
                service
                    .game_snapshot()
                    .is_some_and(|s| s.state == GameAchievementState::SigningIn)
            });
        } else {
            drop(core);
            assert_eq!(service.game_generation(), 0);
            assert!(service.game_snapshot().is_none());
            core = service.wrap(Box::<TestCore>::default());
            core.load(&rom).unwrap();
            assert_ne!(service.game_generation(), old_generation);
        }
        release.send(()).unwrap();
        let state = if sign_out {
            GameAchievementState::SignedOut
        } else {
            GameAchievementState::Ready
        };
        wait_for(|| service.game_snapshot().is_some_and(|s| s.state == state));
        assert_eq!(
            service.game_snapshot().unwrap().generation,
            service.game_generation()
        );
    }
}

#[test]
fn unlocks_publish_a_new_revision_without_mutating_the_previous_snapshot() {
    let root = configured();
    let rom = root.path().join("Games/GBA/Test.gba");
    std::fs::write(&rom, b"ROM").unwrap();
    let store = Store::open(root.path(), "Player").unwrap();
    let hash = library::hash(&store.dir, &rom).unwrap();
    storage::write(
        &store.dir.join("auth.json"),
        &Auth {
            username: "Player".into(),
            token: "fixture-token".into(),
        },
    )
    .unwrap();
    storage::write(
        &store.dir.join(format!("{hash}.json")),
        &(game(), std::collections::BTreeSet::<u32>::new()),
    )
    .unwrap();
    let service = Service::start_with(root.path().into(), Offline);
    let mut core = service.wrap(Box::<TestCore>::default());
    core.load(&rom).unwrap();
    wait_for(|| {
        service.game_snapshot().is_some_and(|s| {
            s.summary.total == 1 && matches!(s.state, GameAchievementState::Offline(_))
        })
    });
    let before = service.game_snapshot().unwrap();
    core.run_frame(ButtonMask(0));
    core.run_frame(ButtonMask(1));
    wait_for(|| {
        service
            .game_snapshot()
            .is_some_and(|s| s.summary.unlocked == 1)
    });
    let after = service.game_snapshot().unwrap();
    assert!(after.revision > before.revision);
    assert_eq!(after.summary.points_earned, 5);
    assert!(after.achievements[0].unlocked_at.is_some());
    assert!(!before.achievements[0].unlocked);
    std::thread::sleep(Duration::from_millis(1100));
    assert!(Arc::ptr_eq(&after, &service.game_snapshot().unwrap()));
}
