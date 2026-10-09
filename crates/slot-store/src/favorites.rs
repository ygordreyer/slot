use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use crate::atomic_write;

pub const FAVORITES_FILE: &str = "Config/favorites.txt";

#[derive(Debug)]
pub enum FavoritesError {
    Read(std::io::Error),
    Malformed,
    Write(std::io::Error),
}

impl fmt::Display for FavoritesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(e) => write!(f, "read: {e}"),
            Self::Malformed => write!(f, "malformed favorites paths"),
            Self::Write(e) => write!(f, "write: {e}"),
        }
    }
}

impl std::error::Error for FavoritesError {}

fn valid(key: &str) -> bool {
    !key.is_empty()
        && !key.chars().any(char::is_control)
        && key.split('/').all(|part| !matches!(part, "" | "." | ".."))
        && !Path::new(key).is_absolute()
}

pub fn read_favorites(root: &Path) -> Result<BTreeSet<String>, FavoritesError> {
    let bytes = match std::fs::read(root.join(FAVORITES_FILE)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(e) => return Err(FavoritesError::Read(e)),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| FavoritesError::Malformed)?;
    text.lines()
        .map(|key| {
            if valid(key) {
                Ok(key.to_string())
            } else {
                Err(FavoritesError::Malformed)
            }
        })
        .collect()
}

pub fn write_favorites(root: &Path, keys: &BTreeSet<String>) -> Result<(), FavoritesError> {
    // Refuse to replace a file we cannot interpret, even if it changed since boot.
    read_favorites(root)?;
    if !keys.iter().all(|key| valid(key)) {
        return Err(FavoritesError::Malformed);
    }
    let text: String = keys.iter().map(|key| format!("{key}\n")).collect();
    atomic_write(&root.join(FAVORITES_FILE), text.as_bytes()).map_err(FavoritesError::Write)
}
