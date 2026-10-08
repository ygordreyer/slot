//! `Cheats/<platform>/<stem>.cht`, in the format RetroArch writes and the libretro database ships:
//!
//! ```text
//! cheats = 2
//! cheat0_desc = "Infinite HP"
//! cheat0_code = "82003B4C+0063"
//! cheat0_enable = true
//! cheat1_desc = "Max Money"
//! cheat1_code = "..."
//! cheat1_enable = false
//! ```
//!
//! The file is the only record of which cheats are on. The device's cheat list reads it when
//! it opens and writes the `cheatN_enable` lines back when it closes, leaving every other line
//! exactly as it was, so the same file still loads in RetroArch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::atomic::atomic_write;
use crate::Platform;

pub const CHEATS_DIR: &str = "Cheats";

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cheat {
    /// The `N` of `cheatN_*` in the file. Not the position in the list: a cheat with no code
    /// is left out of the list, and the number is what the write-back has to find again.
    pub index: usize,
    pub desc: String,
    pub code: String,
    pub enabled: bool,
}

impl Cheat {
    /// What the list shows: the file's own description, or a name made from the number for
    /// the hand-written files that leave it out.
    pub fn title(&self) -> String {
        match self.desc.trim() {
            "" => format!("Cheat {}", self.index + 1),
            d => d.to_string(),
        }
    }
}

pub fn cheat_path(root: &Path, platform: Platform, stem: &str) -> PathBuf {
    root.join(CHEATS_DIR)
        .join(platform.dir_name())
        .join(format!("{stem}.cht"))
}

/// A key and its value off one line, or `None` for a line that is not one. Keys are compared
/// without regard to case; values lose one pair of surrounding quotes.
fn entry(line: &str) -> Option<(String, &str)> {
    let line = line.trim().trim_start_matches('\u{feff}');
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    Some((key.trim().to_ascii_lowercase(), value))
}

/// Best effort, like every file on the card a person edits by hand. A line that cannot be
/// read is skipped and the rest of the file still counts.
///
/// `cheats = N` is honoured when it is there, and when it is missing every index that has a
/// code is taken, in order: hand-written files forget the count more often than not.
pub fn parse_cht(text: &str) -> Vec<Cheat> {
    let kv: HashMap<String, String> = text
        .lines()
        .filter_map(entry)
        .map(|(k, v)| (k, v.to_string()))
        .collect();
    let count = kv
        .get("cheats")
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or_else(|| {
            (0..)
                .take_while(|i| kv.contains_key(&format!("cheat{i}_code")))
                .count()
        });
    (0..count)
        .filter_map(|i| {
            let code = kv.get(&format!("cheat{i}_code"))?.trim().to_string();
            if code.is_empty() {
                return None;
            }
            let enabled = kv
                .get(&format!("cheat{i}_enable"))
                .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "true" | "1"));
            Some(Cheat {
                index: i,
                desc: kv
                    .get(&format!("cheat{i}_desc"))
                    .cloned()
                    .unwrap_or_default(),
                code,
                enabled,
            })
        })
        .collect()
}

/// Every cheat in the cart's file, or nothing when there is no file.
pub fn read_cheats(root: &Path, platform: Platform, stem: &str) -> Vec<Cheat> {
    std::fs::read_to_string(cheat_path(root, platform, stem))
        .map(|t| parse_cht(&t))
        .unwrap_or_default()
}

/// The codes to hand the core: the enabled cheats, in file order.
pub fn enabled_codes(cheats: &[Cheat]) -> Vec<String> {
    cheats
        .iter()
        .filter(|c| c.enabled)
        .map(|c| c.code.clone())
        .collect()
}

/// `text` with every `cheatN_enable` line set to what `cheats` says, and nothing else touched.
/// A cheat whose file had no enable line gets one added at the end. Line endings are kept as
/// the file had them, so a file written on Windows stays a Windows file.
pub fn set_enables(text: &str, cheats: &[Cheat]) -> String {
    let wanted: HashMap<String, bool> = cheats
        .iter()
        .map(|c| (format!("cheat{}_enable", c.index), c.enabled))
        .collect();
    let crlf = text.contains("\r\n");
    let newline = if crlf { "\r\n" } else { "\n" };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let (body, cr) = match line.strip_suffix('\r') {
            Some(b) => (b, "\r"),
            None => (line, ""),
        };
        let replaced = entry(body).and_then(|(key, _)| {
            let on = *wanted.get(&key)?;
            seen.insert(key.clone());
            Some(format!("{key} = {on}{cr}"))
        });
        out.push(replaced.unwrap_or_else(|| line.to_string()));
    }
    let mut text = out.join("\n");
    for c in cheats {
        let key = format!("cheat{}_enable", c.index);
        if !seen.contains(&key) {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push_str(newline);
            }
            text.push_str(&format!("{key} = {}{newline}", c.enabled));
        }
    }
    text
}

/// Writes the list's choices back into the cart's file, all or nothing.
pub fn write_cheat_enables(
    root: &Path,
    platform: Platform,
    stem: &str,
    cheats: &[Cheat],
) -> std::io::Result<()> {
    let path = cheat_path(root, platform, stem);
    let text = std::fs::read_to_string(&path)?;
    atomic_write(&path, set_enables(&text, cheats).as_bytes())
}

/// A copy of the cart's battery save, taken the first time cheats run on it and never again.
///
/// Cheats write to memory the game believes it owns, and a game that saves while one is
/// running writes whatever it was made to believe. Some of that cannot be undone from inside
/// the game — a walk-through-walls code saved inside a wall, an item the game was never meant
/// to hold — so the save as it was before any cheat touched it is kept beside it, once.
pub fn backup_save_once(root: &Path, platform: Platform, stem: &str) {
    let dir = root.join("Saves").join(platform.dir_name());
    for ext in ["sav", "srm"] {
        let from = dir.join(format!("{stem}.{ext}"));
        let to = dir.join(format!("{stem}.{ext}.before-cheats"));
        if from.exists() && !to.exists() {
            if let Err(e) = std::fs::copy(&from, &to) {
                eprintln!("slot: cheats: could not back up {}: {e}", from.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "cheats = 3\n\ncheat0_desc = \"Infinite HP\"\ncheat0_code = \"82003B4C+0063\"\ncheat0_enable = false\n\ncheat1_desc = \"\"\ncheat1_code = \"\"\ncheat1_enable = false\n\ncheat2_desc = \"Money\"\ncheat2_code = \"1234\"\ncheat2_enable = true\n";

    #[test]
    fn reads_a_retroarch_file_and_keeps_each_cheats_own_number() {
        let c = parse_cht(FILE);
        assert_eq!(c.len(), 2, "the cheat with no code is left out");
        assert_eq!((c[0].index, c[0].title().as_str()), (0, "Infinite HP"));
        assert_eq!(c[1].index, 2);
        assert!(!c[0].enabled && c[1].enabled);
        assert_eq!(enabled_codes(&c), vec!["1234".to_string()]);
    }

    #[test]
    fn a_missing_count_takes_every_numbered_code() {
        let c = parse_cht("cheat0_code = A\ncheat0_enable = true\ncheat1_code = B\n");
        assert_eq!(c.len(), 2);
        assert!(c[0].enabled && !c[1].enabled);
        assert_eq!(c[1].title(), "Cheat 2");
    }

    #[test]
    fn writing_back_changes_only_the_enable_lines() {
        let mut c = parse_cht(FILE);
        c[0].enabled = true;
        c[1].enabled = false;
        let out = set_enables(FILE, &c);
        assert!(out.contains("cheat0_enable = true\n"));
        assert!(out.contains("cheat2_enable = false\n"));
        assert!(
            out.contains("cheat1_enable = false\n"),
            "a line for a skipped cheat is left alone"
        );
        assert!(out.contains("cheat0_code = \"82003B4C+0063\"\n"));
        assert_eq!(out.lines().count(), FILE.lines().count());
        assert!(parse_cht(&out)[0].enabled);
    }

    #[test]
    fn a_windows_file_stays_a_windows_file() {
        let file = FILE.replace('\n', "\r\n");
        let mut c = parse_cht(&file);
        c[0].enabled = true;
        let out = set_enables(&file, &c);
        assert!(out.contains("cheat0_enable = true\r\n"));
        assert!(!out.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn a_cheat_with_no_enable_line_gets_one() {
        let file = "cheat0_desc = X\ncheat0_code = 1\n";
        let mut c = parse_cht(file);
        c[0].enabled = true;
        let out = set_enables(file, &c);
        assert!(parse_cht(&out)[0].enabled);
    }
}
