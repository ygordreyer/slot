use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

#[derive(Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub password: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Self, &'static str> {
        let text = Zeroizing::new(
            std::fs::read_to_string(path).map_err(|_| "Cannot read achievement config")?,
        );
        // TOML diagnostics include source lines, which may contain credentials.
        toml::from_str(&text).map_err(|_| "Invalid achievement config")
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let dir = root.join("Config");
        std::fs::create_dir_all(&dir).map_err(|_| "Cannot create achievement settings")?;
        // JSON and TOML basic strings share escapes, except JSON leaves DEL literal.
        let username = serde_json::to_string(&self.username)
            .map_err(|_| "Invalid username")?
            .replace('\u{7f}', "\\u007F");
        let text = format!("enabled = {}\nusername = {}\n", self.enabled, username);
        private_write(&dir.join("retroachievements.toml"), text.as_bytes())
    }

    pub fn save_enabled_preserving_legacy(&self, root: &Path) -> Result<(), String> {
        #[derive(Deserialize)]
        struct Toggle {
            enabled: Option<toml::Spanned<bool>>,
        }
        let path = root.join("Config/retroachievements.toml");
        let source = Zeroizing::new(
            std::fs::read_to_string(&path).map_err(|_| "Cannot read achievement config")?,
        );
        let toggle: Toggle = toml::from_str(&source).map_err(|_| "Invalid achievement config")?;
        let value = if self.enabled {
            b"true".as_slice()
        } else {
            b"false".as_slice()
        };
        // Replace only the toggle so handwritten credentials retain their original spelling.
        let mut text = Zeroizing::new(Vec::with_capacity(source.len() + 16));
        if let Some(enabled) = toggle.enabled {
            let span = enabled.span();
            text.extend_from_slice(&source.as_bytes()[..span.start]);
            text.extend_from_slice(value);
            text.extend_from_slice(&source.as_bytes()[span.end..]);
        } else {
            text.extend_from_slice(b"enabled = ");
            text.extend_from_slice(value);
            text.push(b'\n');
            text.extend_from_slice(source.as_bytes());
        }
        private_write(&path, &text)
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        self.password.zeroize();
        self.token.zeroize();
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Auth {
    pub username: String,
    pub token: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Achievement {
    #[serde(rename = "BadgeName", default)]
    pub badge: String,
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "Title")]
    pub title: String,
    #[serde(rename = "Description")]
    pub description: String,
    #[serde(rename = "Points")]
    pub points: u32,
    #[serde(rename = "Flags")]
    pub flags: u32,
    #[serde(rename = "MemAddr")]
    pub definition: String,
}

fn nullable_string<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Game {
    #[serde(
        rename = "RichPresencePatch",
        default,
        deserialize_with = "nullable_string"
    )]
    pub presence: String,
    #[serde(rename = "ID")]
    pub id: u32,
    #[serde(rename = "ConsoleID")]
    pub console: u32,
    #[serde(rename = "Title")]
    pub title: String,
    #[serde(rename = "Achievements")]
    pub achievements: Vec<Achievement>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Unlock {
    pub id: u32,
    pub hash: String,
    pub earned_at: u64,
    pub synced: bool,
}

/// Confirmed records are retained to suppress repeated unlocks after an offline restart.
/// One account owns a ledger. A token is never copied into an unlock or game cache.
pub(crate) struct Store {
    pub dir: PathBuf,
    pub unlocks: BTreeMap<u32, Unlock>,
}

pub(crate) fn account_dir(root: &Path, username: &str) -> PathBuf {
    let account = format!("{:x}", md5::compute(username.to_lowercase()));
    root.join("Saves/RetroAchievements").join(account)
}

impl Store {
    pub fn open(root: &Path, username: &str) -> Result<Self, String> {
        let dir = account_dir(root, username);
        std::fs::create_dir_all(&dir).map_err(|_| "Cannot create achievement cache")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let path = dir.join("unlocks.json");
        // A broken ledger is an error, never an empty ledger that overwrites unsynced awards.
        let unlocks = if path.exists() {
            read(&path)?
        } else {
            BTreeMap::new()
        };
        Ok(Self { dir, unlocks })
    }

    pub fn record(&mut self, unlock: Unlock) -> Result<bool, String> {
        if self.unlocks.contains_key(&unlock.id) {
            return Ok(false);
        }
        let id = unlock.id;
        self.unlocks.insert(id, unlock);
        if let Err(e) = self.save() {
            self.unlocks.remove(&id);
            return Err(e);
        }
        Ok(true)
    }

    pub fn ack(&mut self, id: u32) -> Result<(), String> {
        let was_synced = self.unlocks.get(&id).is_some_and(|u| u.synced);
        if let Some(unlock) = self.unlocks.get_mut(&id) {
            unlock.synced = true;
        }
        if let Err(e) = self.save() {
            if let Some(unlock) = self.unlocks.get_mut(&id) {
                unlock.synced = was_synced;
            }
            return Err(e);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), String> {
        write(&self.dir.join("unlocks.json"), &self.unlocks)
    }
}

pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = std::fs::File::open(path).map_err(|_| "Cannot read achievement data")?;
    serde_json::from_reader(file).map_err(|_| "Invalid achievement data".into())
}

pub(crate) fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "Cannot encode achievement data")?;
    private_write(path, &bytes)
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        if let Some(parent) = path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(|_| "Cannot save achievement data".into())
}
