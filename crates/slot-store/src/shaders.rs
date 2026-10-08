//! `Shaders/`: single-pass RetroArch `.glsl` files, one look each, chosen from the quick menu.
//!
//! Two looks are built in and always listed first: `LCD`, the mask slot has always drawn,
//! and `Off`, the bare 3x picture. Every `.glsl` file in the folder follows, by file name,
//! sorted without regard to case. A file named after a built-in is left out rather than
//! allowed to shadow it, so the default can never be taken away by what is on the card.

use std::path::{Path, PathBuf};

pub const SHADERS_DIR: &str = "Shaders";
pub const SHADER_LCD: &str = "LCD";
pub const SHADER_OFF: &str = "Off";

/// Every look on offer, in the order the quick menu steps through them.
pub fn list_shaders(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(root.join(SHADERS_DIR))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let is_glsl = path
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("glsl"));
            let stem = path.file_stem()?.to_str()?.to_string();
            // `._name.glsl` is macOS's resource fork, dropped on the card beside every file
            // copied from a Mac. It is not a shader.
            (is_glsl && !stem.starts_with('.') && !is_builtin(&stem)).then_some(stem)
        })
        .collect();
    found.sort_by_key(|s| s.to_lowercase());
    let mut all = vec![SHADER_LCD.to_string(), SHADER_OFF.to_string()];
    all.extend(found);
    all
}

pub fn is_builtin(name: &str) -> bool {
    name.eq_ignore_ascii_case(SHADER_LCD) || name.eq_ignore_ascii_case(SHADER_OFF)
}

/// Where a look that is not built in comes from. The extension is found again rather than
/// assumed, because the card may spell it `.GLSL`.
pub fn shader_path(root: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(root.join(SHADERS_DIR))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_stem().and_then(|s| s.to_str()) == Some(name)
                && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("glsl"))
        })
}
