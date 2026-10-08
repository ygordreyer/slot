//! Cheat files and save backups are isolated by platform, even for matching ROM stems.

use slot_store::{backup_save_once, cheat_path, read_cheats, write_cheat_enables, Platform};

#[test]
fn matching_stems_keep_their_own_cheats_and_first_save_backup() {
    let root = tempfile::tempdir().unwrap();
    for (i, platform) in Platform::ALL.into_iter().enumerate() {
        let path = cheat_path(root.path(), platform, "Same");
        assert_eq!(
            path,
            root.path()
                .join(format!("Cheats/{}/Same.cht", platform.dir_name()))
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("cheat0_code = {i}\ncheat0_enable = false\n")).unwrap();
        let saves = root.path().join("Saves").join(platform.dir_name());
        std::fs::create_dir_all(&saves).unwrap();
        for ext in ["sav", "srm"] {
            std::fs::write(saves.join(format!("Same.{ext}")), [i as u8]).unwrap();
        }
    }
    for (i, platform) in Platform::ALL.into_iter().enumerate() {
        let mut cheats = read_cheats(root.path(), platform, "Same");
        assert_eq!(cheats[0].code, i.to_string());
        assert!(!cheats[0].enabled);
        cheats[0].enabled = true;
        write_cheat_enables(root.path(), platform, "Same", &cheats).unwrap();
        assert!(read_cheats(root.path(), platform, "Same")[0].enabled);
        backup_save_once(root.path(), platform, "Same");
        let saves = root.path().join("Saves").join(platform.dir_name());
        for ext in ["sav", "srm"] {
            std::fs::write(saves.join(format!("Same.{ext}")), [99]).unwrap();
        }
        backup_save_once(root.path(), platform, "Same");
        for ext in ["sav", "srm"] {
            assert_eq!(
                std::fs::read(saves.join(format!("Same.{ext}.before-cheats"))).unwrap(),
                [i as u8]
            );
        }
        for other in Platform::ALL.into_iter().skip(i + 1) {
            assert!(!read_cheats(root.path(), other, "Same")[0].enabled);
        }
    }
}
