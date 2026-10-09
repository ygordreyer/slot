use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::storage::{self, Auth, Config, Game, Store, Unlock};
use crate::{AccountControl, Notice, Status, SyncStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use zeroize::{Zeroize, Zeroizing};

pub(crate) enum Command {
    Load(Option<Load>),
    Account(AccountControl),
}

const API: &str = "https://retroachievements.org/dorequest.php";
const VERSION: &str = "12.2.1";

#[derive(Clone)]
pub(crate) struct Load {
    pub generation: u64,
    pub path: PathBuf,
    pub epoch: u64,
    pub supported: bool,
}

pub(crate) struct Prepared {
    pub epoch: u64,
    pub error: Option<crate::GameAchievementState>,
    pub generation: u64,
    pub hash: String,
    pub game: Game,
    pub unlocked: BTreeSet<u32>,
    pub cached: bool,
}

fn empty_game() -> Game {
    Game {
        id: 0,
        console: 5,
        title: String::new(),
        presence: String::new(),
        achievements: Vec::new(),
    }
}

fn failed(load: &Load, state: crate::GameAchievementState) -> Prepared {
    Prepared {
        generation: load.generation,
        epoch: load.epoch,
        error: Some(state),
        hash: String::new(),
        game: empty_game(),
        unlocked: BTreeSet::new(),
        cached: false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Network,
    Server,
    Rejected,
    Authentication,
    Invalid,
}

impl Failure {
    fn status(self) -> SyncStatus {
        match self {
            Self::Network => SyncStatus::Offline,
            Self::Server => SyncStatus::ServerBusy,
            _ => SyncStatus::Attention,
        }
    }
}

pub(crate) trait Transport {
    fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure>;
    fn diagnostic(&self) -> Option<String> {
        None
    }
    fn badge(&mut self, _name: &str) -> Result<Vec<u8>, Failure> {
        Err(Failure::Invalid)
    }
}

struct EnabledTransport<'a, T> {
    inner: &'a mut T,
    enabled: &'a AtomicBool,
    diagnostic: &'a mut String,
}

impl<T: Transport> Transport for EnabledTransport<'_, T> {
    fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err(Failure::Invalid);
        }
        let operation = fields
            .iter()
            .find(|(key, _)| *key == "r")
            .map_or("unknown", |(_, value)| operation_name(value));
        let result = self.inner.call(fields);
        let detail = self.inner.diagnostic().unwrap_or_else(|| match &result {
            Err(error) => failure_detail(*error).into(),
            Ok(_) => "RA rejection or invalid data".into(),
        });
        *self.diagnostic = format!("r={operation} failed: {detail}");
        result
    }

    fn badge(&mut self, name: &str) -> Result<Vec<u8>, Failure> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err(Failure::Invalid);
        }
        self.inner.badge(name)
    }
}

fn operation_name(value: &str) -> &'static str {
    match value {
        "login2" => "login2",
        "gameid" => "gameid",
        "patch" => "patch",
        "unlocks" => "unlocks",
        "startsession" => "startsession",
        "ping" => "ping",
        "awardachievement" => "awardachievement",
        _ => "unknown",
    }
}

fn failure_detail(error: Failure) -> &'static str {
    match error {
        Failure::Network => "network (connection)",
        Failure::Server => "server busy",
        Failure::Authentication => "authentication",
        Failure::Rejected => "RA rejection",
        Failure::Invalid => "invalid data or storage",
    }
}

fn invalid_token(body: &Value) -> bool {
    ["Code", "Error"].iter().any(|field| {
        let text = body[*field].as_str().unwrap_or("").to_ascii_lowercase();
        text.contains("token")
            && ["invalid", "expired", "revoked", "not valid"]
                .iter()
                .any(|word| text.contains(word))
    })
}

// Only fixed protocol labels may reach stderr; even a plausible Code can echo a credential.
fn diagnostic_code(body: &Value) -> Option<&'static str> {
    match body["Code"].as_str()? {
        "invalid_token" => Some("invalid_token"),
        "expired_token" => Some("expired_token"),
        "invalid_credentials" => Some("invalid_credentials"),
        "access_denied" => Some("access_denied"),
        "invalid_parameter" => Some("invalid_parameter"),
        "missing_parameter" => Some("missing_parameter"),
        "not_found" => Some("not_found"),
        "rate_limited" => Some("rate_limited"),
        _ => None,
    }
}

pub(crate) fn classify_response(status: u16, body: Option<Value>) -> Result<Value, Failure> {
    if matches!(status, 401 | 403) {
        return Err(Failure::Authentication);
    }
    if status == 429 || (500..600).contains(&status) {
        return Err(Failure::Server);
    }
    if body.as_ref().is_some_and(invalid_token) {
        return Err(Failure::Authentication);
    }
    if (200..300).contains(&status) {
        body.ok_or(Failure::Invalid)
    } else if (400..500).contains(&status)
        && body
            .as_ref()
            .is_some_and(|body| body.get("Success").is_some() || body.get("Error").is_some())
    {
        Err(Failure::Rejected)
    } else {
        Err(Failure::Invalid)
    }
}

fn classify_badge_status(status: u16) -> Result<(), Failure> {
    match status {
        200..=299 => Ok(()),
        401 | 403 => Err(Failure::Authentication),
        429 | 500..=599 => Err(Failure::Server),
        400..=499 => Err(Failure::Rejected),
        _ => Err(Failure::Invalid),
    }
}

fn transport_failure(error: &ureq::Error) -> (Failure, &'static str) {
    match error {
        ureq::Error::Timeout(_) => (Failure::Network, "network (timeout)"),
        ureq::Error::HostNotFound => (Failure::Network, "network (DNS)"),
        ureq::Error::Tls(_) | ureq::Error::Rustls(_) => (Failure::Network, "network (TLS)"),
        ureq::Error::Io(_) | ureq::Error::ConnectionFailed => {
            (Failure::Network, "network (connection)")
        }
        _ => (Failure::Invalid, "invalid HTTP response"),
    }
}

pub(crate) struct Http {
    agent: ureq::Agent,
    diagnostic: Option<String>,
}

impl Http {
    pub(crate) fn new() -> Self {
        Self {
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(5)))
                .max_redirects(0)
                .user_agent(concat!(
                    "slot/",
                    env!("CARGO_PKG_VERSION"),
                    " rcheevos/12.2.1"
                ))
                .build()
                .into(),
            diagnostic: None,
        }
    }

    fn error(&mut self, error: ureq::Error) -> Failure {
        let (failure, detail) = transport_failure(&error);
        self.diagnostic = Some(detail.into());
        failure
    }

    fn call_url(&mut self, url: &str, fields: &[(&str, String)]) -> Result<Value, Failure> {
        self.diagnostic = None;
        let result = if fields
            .iter()
            .any(|(key, value)| *key == "r" && value == "login2")
        {
            let body = login_body(fields);
            self.agent
                .post(url)
                .header("Content-Type", "application/x-www-form-urlencoded")
                .send(body.as_slice())
        } else {
            let form: Vec<_> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
            self.agent.post(url).send_form(form)
        };
        let mut response = result.map_err(|error| self.error(error))?;
        let status = response.status().as_u16();
        let bytes = response
            .body_mut()
            .with_config()
            .limit(8 * 1024 * 1024)
            .read_to_vec();
        let body = match bytes {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
            Err(error) => {
                // An HTTP error status is authoritative even if its body is truncated.
                if (200..300).contains(&status) {
                    return Err(self.error(error));
                }
                None
            }
        };
        self.diagnostic = Some(match body.as_ref().and_then(diagnostic_code) {
            Some(code) => format!("http {status} ({code})"),
            None => format!("http {status}"),
        });
        classify_response(status, body)
    }
}

impl Transport for Http {
    fn diagnostic(&self) -> Option<String> {
        self.diagnostic.clone()
    }
    fn badge(&mut self, name: &str) -> Result<Vec<u8>, Failure> {
        self.diagnostic = None;
        crate::badges::path(std::path::Path::new(""), name).ok_or(Failure::Invalid)?;
        let mut response = self
            .agent
            .get(format!(
                "https://media.retroachievements.org/Badge/{name}.png"
            ))
            .call()
            .map_err(|error| self.error(error))?;
        let status = response.status().as_u16();
        self.diagnostic = Some(format!("http {status}"));
        classify_badge_status(status)?;
        response
            .body_mut()
            .with_config()
            .limit(crate::badges::MAX_BYTES)
            .read_to_vec()
            .map_err(|error| self.error(error))
    }
    fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure> {
        self.call_url(API, fields)
    }
}

pub(crate) fn login_body(fields: &[(&str, String)]) -> Zeroizing<Vec<u8>> {
    fn plain(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_')
    }
    let capacity = fields
        .iter()
        .map(|(key, value)| {
            key.bytes()
                .chain(value.bytes())
                .map(|byte| if plain(byte) || byte == b' ' { 1 } else { 3 })
                .sum::<usize>()
                + 1
        })
        .sum::<usize>()
        + fields.len().saturating_sub(1);
    let mut body = Zeroizing::new(Vec::with_capacity(capacity));
    for (key, value) in fields {
        if !body.is_empty() {
            body.push(b'&');
        }
        for (index, part) in [*key, value.as_str()].into_iter().enumerate() {
            if index == 1 {
                body.push(b'=');
            }
            for byte in part.bytes() {
                if plain(byte) {
                    body.push(byte);
                } else if byte == b' ' {
                    body.push(b'+');
                } else {
                    body.push(b'%');
                    body.push(b"0123456789ABCDEF"[(byte >> 4) as usize]);
                    body.push(b"0123456789ABCDEF"[(byte & 15) as usize]);
                }
            }
        }
    }
    debug_assert_eq!(body.len(), capacity);
    body
}

fn contains_ascii_case_insensitive(text: &str, secret: &str) -> bool {
    !secret.is_empty()
        && text
            .as_bytes()
            .windows(secret.len())
            .any(|window| window.eq_ignore_ascii_case(secret.as_bytes()))
}

fn success(value: Value) -> Result<Value, Failure> {
    if value.get("Success") == Some(&Value::Bool(true)) {
        Ok(value)
    } else if invalid_token(&value) {
        Err(Failure::Authentication)
    } else {
        Err(Failure::Rejected)
    }
}

fn call(
    http: &mut impl Transport,
    auth: &Auth,
    request: &str,
    extra: &[(&str, String)],
) -> Result<Value, Failure> {
    let mut fields = vec![
        ("r", request.into()),
        ("u", auth.username.clone()),
        ("t", auth.token.clone()),
    ];
    fields.extend_from_slice(extra);
    success(http.call(&fields)?)
}

struct LoginError {
    failure: Failure,
    message: String,
}

struct LoginFields(Vec<(&'static str, String)>);
impl Drop for LoginFields {
    fn drop(&mut self) {
        for (_, value) in &mut self.0 {
            value.zeroize();
        }
    }
}

fn login_error(error: Failure) -> LoginError {
    let message = match error {
        Failure::Network => "Can't reach RetroAchievements",
        Failure::Server => "RetroAchievements server busy, retrying",
        Failure::Authentication => "Sign in again",
        _ => "Invalid RetroAchievements response",
    };
    LoginError {
        failure: error,
        message: message.into(),
    }
}

fn rejection(value: &Value, config: &Config) -> LoginError {
    let raw = value["Error"].as_str().unwrap_or("");
    let mut text = Zeroizing::new(String::with_capacity(raw.len()));
    text.extend(raw.chars().filter(char::is_ascii_graphic));
    // Check before truncation and after removing controls, so echoed secrets cannot escape.
    let unsafe_text = [&config.username, &config.password, &config.token]
        .iter()
        .filter(|secret| !secret.is_empty())
        .any(|secret| {
            contains_ascii_case_insensitive(raw, secret)
                || contains_ascii_case_insensitive(&text, secret)
        });
    let mut printable = Zeroizing::new(String::with_capacity(raw.len()));
    printable.extend(raw.chars().filter(|c| c.is_ascii_graphic() || *c == ' '));
    let exposes_secret = |message: &str| {
        [&config.username, &config.password, &config.token]
            .iter()
            .filter(|secret| !secret.is_empty())
            .any(|secret| contains_ascii_case_insensitive(message, secret))
    };
    let fallback = || {
        for message in ["Sign in rejected", "Rejected"] {
            if !exposes_secret(message) {
                return message.to_string();
            }
        }
        (33u8..=126)
            .map(|byte| (byte as char).to_string())
            .find(|message| !exposes_secret(message))
            .unwrap()
    };
    let message = if unsafe_text || printable.trim().is_empty() {
        fallback()
    } else {
        let mut message = Zeroizing::new(String::with_capacity(printable.len().min(80)));
        message.extend(printable.chars().take(80));
        if exposes_secret(&message) {
            fallback()
        } else {
            std::mem::take(&mut *message)
        }
    };
    LoginError {
        failure: Failure::Rejected,
        message,
    }
}

fn login_detailed(
    http: &mut impl Transport,
    config: &Config,
    remembered: Option<&Auth>,
) -> Result<Auth, LoginError> {
    let mut fields = LoginFields(vec![("r", "login2".into()), ("u", config.username.clone())]);
    if !config.token.is_empty() {
        fields.0.push(("t", config.token.clone()));
    } else if let Some(auth) = remembered {
        fields.0.push(("t", auth.token.clone()));
    } else {
        fields.0.push(("p", config.password.clone()));
    }
    let response = http.call(&fields.0);
    let retry_password = remembered.is_some()
        && config.token.is_empty()
        && !config.password.is_empty()
        && match &response {
            Ok(body) => body["Success"] != Value::Bool(true),
            Err(error) => matches!(error, Failure::Authentication | Failure::Rejected),
        };
    let mut response = if retry_password {
        let retry = LoginFields(vec![
            ("r", "login2".into()),
            ("u", config.username.clone()),
            ("p", config.password.clone()),
        ]);
        http.call(&retry.0).map_err(login_error)?
    } else {
        response.map_err(login_error)?
    };
    if response["Success"] != Value::Bool(true) {
        let error = rejection(&response, config);
        if let Some(Value::String(message)) = response.get_mut("Error") {
            message.zeroize();
        }
        return Err(error);
    }
    let username = response["User"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| login_error(Failure::Invalid))?;
    let token = response["Token"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| login_error(Failure::Invalid))?;
    Ok(Auth {
        username: username.into(),
        token: token.into(),
    })
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Matches rc_api_init_award_achievement_request, including the original unlock age.
pub(crate) fn award(
    http: &mut impl Transport,
    auth: &Auth,
    unlock: &Unlock,
    now: u64,
) -> Result<(), Failure> {
    let age = now
        .saturating_sub(unlock.earned_at)
        .min(u64::from(u32::MAX));
    let mut signature = format!("{}{}0", unlock.id, auth.username);
    if age > 0 {
        signature.push_str(&format!("{}{age}", unlock.id));
    }
    let mut extra = vec![
        ("a", unlock.id.to_string()),
        ("h", "0".into()),
        ("m", unlock.hash.clone()),
        ("v", format!("{:x}", md5::compute(signature))),
    ];
    if age > 0 {
        extra.push(("o", age.to_string()));
    }
    let mut fields = vec![
        ("r", "awardachievement".into()),
        ("u", auth.username.clone()),
        ("t", auth.token.clone()),
    ];
    fields.extend(extra);
    let response = http.call(&fields)?;
    // A retry after a lost response may say the server already has this unlock.
    if response["Success"] == true
        || response["Error"]
            .as_str()
            .is_some_and(|s| s.starts_with("User already has"))
    {
        Ok(())
    } else if invalid_token(&response) {
        Err(Failure::Authentication)
    } else {
        Err(Failure::Rejected)
    }
}

fn unlocked(response: &Value) -> Result<BTreeSet<u32>, Failure> {
    let mut ids = BTreeSet::new();
    for field in ["Unlocks", "HardcoreUnlocks"] {
        // Like rc_api_process_start_session_response, accept omitted empty lists.
        if response[field].is_null() {
            continue;
        }
        let entries = response[field].as_array().ok_or(Failure::Invalid)?;
        for entry in entries {
            let id = entry["ID"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or(Failure::Invalid)?;
            ids.insert(id);
        }
    }
    Ok(ids)
}

struct Active {
    generation: u64,
    epoch: u64,
    hash: String,
    ready: bool,
    online: bool,
    last_ping: Instant,
}

fn prefetch(
    http: &mut impl Transport,
    auth: &Auth,
    dir: &std::path::Path,
    hash: &str,
) -> Result<Game, Failure> {
    let resolved = call(http, auth, "gameid", &[("m", hash.into())])?;
    let id = resolved["GameID"].as_u64().ok_or(Failure::Invalid)?;
    let (game, ids) = if id == 0 {
        (
            Game {
                presence: String::new(),
                id: 0,
                console: 5,
                title: "No achievements for this ROM".into(),
                achievements: vec![],
            },
            BTreeSet::new(),
        )
    } else {
        let patch = call(http, auth, "patch", &[("g", id.to_string())])?;
        let game: Game =
            serde_json::from_value(patch["PatchData"].clone()).map_err(|_| Failure::Invalid)?;
        if game.console != 5 || u64::from(game.id) != id {
            return Err(Failure::Invalid);
        }
        let unlocks = call(
            http,
            auth,
            "unlocks",
            &[("g", id.to_string()), ("h", "0".into())],
        )?;
        let ids: BTreeSet<u32> =
            serde_json::from_value(unlocks["UserUnlocks"].clone()).map_err(|_| Failure::Invalid)?;
        (game, ids)
    };
    storage::write(&dir.join(format!("{hash}.json")), &(&game, ids))
        .map_err(|_| Failure::Invalid)?;
    Ok(game)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run(
    root: PathBuf,
    mut config: Config,
    store: Arc<Mutex<Store>>,
    loads: mpsc::Receiver<Command>,
    prepared: mpsc::Sender<Prepared>,
    notices: mpsc::Sender<Notice>,
    status: Arc<Status>,
    enabled: Arc<AtomicBool>,
    mut http: impl Transport,
) {
    let mut dir = store.lock().unwrap().dir.clone();
    let mut auth: Option<Auth> = storage::read(&dir.join("auth.json")).ok();
    {
        let mut account = status.account.lock().unwrap();
        account.enabled = config.enabled;
        account.username = config.username.clone();
        account.signed_in = auth.is_some();
        if !account.busy && account.message.is_empty() {
            account.message = auth.as_ref().map_or_else(
                || "Signed out".into(),
                |a| format!("Signed in as {}", a.username),
            );
        }
    }
    let mut verified = false;
    let mut save_settings_after_login = false;
    let mut active: Option<Active> = None;
    let mut failure_reported = false;
    let mut next_attempt = Instant::now();
    let mut delay = 30;
    let mut warned = false;
    let mut last_award = 0;
    let mut library = crate::library::pending(&root, &dir, now());
    let mut library_total = library.len();
    let mut library_failed = false;
    let mut rescanned = Instant::now();
    let mut background_reported = None;
    let mut badges = crate::badges::Queue::new(&dir);
    loop {
        match loads.recv_timeout(Duration::from_millis(200)) {
            Ok(Command::Load(load)) => {
                failure_reported = false;
                active = load.and_then(|load| {
                    // GBA identification is the MD5 of the complete uncompressed ROM.
                    // File I/O and hashing happen here, never on the emulator/UI threads.
                    let hash = match crate::library::hash(&dir, &load.path) {
                        Ok(hash) => hash,
                        Err(_) => {
                            let _ = prepared.send(failed(
                                &load,
                                crate::GameAchievementState::Error("Cannot read game".into()),
                            ));
                            return None;
                        }
                    };
                    let mut game = Active {
                        generation: load.generation,
                        epoch: load.epoch,
                        hash,
                        ready: false,
                        online: false,
                        last_ping: Instant::now(),
                    };
                    if auth.is_some() {
                        let cache = dir.join(format!("{}.json", game.hash));
                        if let Ok((data, ids)) = storage::read::<(Game, BTreeSet<u32>)>(&cache) {
                            if data.console == 5 {
                                badges.enqueue(&data);
                                let _ = prepared.send(Prepared {
                                    generation: game.generation,
                                    epoch: game.epoch,
                                    error: None,
                                    hash: game.hash.clone(),
                                    game: data,
                                    unlocked: ids,
                                    cached: true,
                                });
                                game.ready = true;
                            }
                        }
                    }
                    Some(game)
                });
                next_attempt = Instant::now();
                warned = false;
            }
            Ok(Command::Account(command)) => {
                active = None;
                failure_reported = false;
                background_reported = None;
                let mut login_failure = None;
                let result = (|| -> Result<(), String> {
                    match command {
                        AccountControl::SignIn {
                            username,
                            mut password,
                        } => {
                            if auth.is_some() {
                                return Err("Sign out first".into());
                            }
                            if username.trim().is_empty() || password.is_empty() {
                                return Err("Enter username and password".into());
                            }
                            let mut credentials = Config::default();
                            credentials.username = username;
                            credentials.password = std::mem::take(&mut *password);
                            let result = login_detailed(&mut http, &credentials, None);
                            if let Err(error) = &result {
                                let detail = http
                                    .diagnostic()
                                    .unwrap_or_else(|| failure_detail(error.failure).into());
                                login_failure =
                                    Some((error.failure, format!("r=login2 failed: {detail}")));
                            }
                            credentials.password.zeroize();
                            drop(password);
                            let logged_in = result.map_err(|e| e.message)?;
                            let new_store = Store::open(&root, &logged_in.username)?;
                            storage::write(&new_store.dir.join("auth.json"), &logged_in)?;
                            config.username = logged_in.username.clone();
                            config.token.zeroize();
                            config.password.zeroize();
                            auth = Some(logged_in);
                            verified = true;
                            *store.lock().unwrap() = new_store;
                            config.save(&root)?;
                            save_settings_after_login = false;
                        }
                        AccountControl::SignOut => {
                            match std::fs::remove_file(dir.join("auth.json")) {
                                Ok(()) => {
                                    if let Ok(directory) = std::fs::File::open(&dir) {
                                        let _ = directory.sync_all();
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(_) => return Err("Cannot remove saved session".into()),
                            }
                            auth = None;
                            verified = false;
                            config.username.clear();
                            config.password.zeroize();
                            config.token.zeroize();
                            config.save(&root)?;
                            save_settings_after_login = false;
                        }
                        AccountControl::SetEnabled(value) => {
                            config.enabled = value;
                            let legacy = !config.password.is_empty() || !config.token.is_empty();
                            let preserve_legacy = legacy && !verified;
                            if preserve_legacy {
                                config.save_enabled_preserving_legacy(&root)?;
                            } else {
                                config.save(&root)?;
                            }
                            verified = false;
                            save_settings_after_login = value && preserve_legacy;
                        }
                    }
                    Ok(())
                })();
                dir = store.lock().unwrap().dir.clone();
                library = if config.username.is_empty() {
                    Default::default()
                } else {
                    crate::library::pending(&root, &dir, now())
                };
                library_total = library.len();
                library_failed = false;
                badges = crate::badges::Queue::new(&dir);
                rescanned = Instant::now();
                next_attempt = Instant::now();
                warned = false;
                delay = 30;
                let mut account = status.account.lock().unwrap();
                account.enabled = config.enabled;
                account.username = config.username.clone();
                account.signed_in = auth.is_some();
                account.busy = false;
                if let Some((failure, diagnostic)) = login_failure {
                    status.set_diagnostic(failure.status(), &diagnostic);
                } else {
                    status.set(if result.is_err() {
                        SyncStatus::Attention
                    } else if config.enabled && auth.is_some() {
                        SyncStatus::Syncing
                    } else {
                        SyncStatus::Disabled
                    });
                }
                account.message = match result {
                    Ok(()) => auth.as_ref().map_or_else(
                        || "Signed out".into(),
                        |a| format!("Signed in as {}", a.username),
                    ),
                    Err(error) => error,
                };
                enabled.store(
                    config.enabled
                        && !config.username.is_empty()
                        && (auth.is_some()
                            || !config.token.is_empty()
                            || !config.password.is_empty()),
                    Ordering::Release,
                );
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if !config.enabled
            || config.username.trim().is_empty()
            || !enabled.load(Ordering::Acquire)
            || (auth.is_none() && config.token.is_empty() && config.password.is_empty())
        {
            if status.get() != SyncStatus::Attention {
                status.set(SyncStatus::Disabled);
            }
            status.pending.store(false, Ordering::Release);
            status.progress.store(0, Ordering::Release);
            continue;
        }
        // Metadata preparation and badge downloads each occupy half of a cache refresh.
        status.pending.store(
            !library.is_empty()
                || library_failed
                || badges.pending()
                || store.lock().unwrap().unlocks.values().any(|u| !u.synced)
                || active.as_ref().is_some_and(|game| !game.ready),
            std::sync::atomic::Ordering::Release,
        );
        status.progress.store(
            cache_progress(
                library_total,
                library.len(),
                badges.pending(),
                badges.percent(),
            )
            .map_or(0, |percent| percent + 1),
            std::sync::atomic::Ordering::Release,
        );
        if status
            .reconnect
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            next_attempt = Instant::now();
        }
        if Instant::now() < next_attempt {
            continue;
        }
        if rescanned.elapsed() >= Duration::from_secs(300) {
            library = crate::library::pending(&root, &dir, now());
            library_total = library.len();
            library_failed = false;
            badges.scan();
            rescanned = Instant::now();
        }
        let mut diagnostic = "failed: invalid data or storage".to_string();
        let mut background_error = None;
        let mut background_succeeded = false;
        let result = (|| -> Result<(), Failure> {
            let mut http = EnabledTransport {
                inner: &mut http,
                enabled: &enabled,
                diagnostic: &mut diagnostic,
            };
            if !verified {
                let logged_in =
                    login_detailed(&mut http, &config, auth.as_ref()).map_err(|error| {
                        let mut account = status.account.lock().unwrap();
                        if !account.busy {
                            account.message = error.message;
                        }
                        if error.failure == Failure::Rejected {
                            Failure::Authentication
                        } else {
                            error.failure
                        }
                    })?;
                storage::write(&dir.join("auth.json"), &logged_in).map_err(|_| Failure::Invalid)?;
                auth = Some(logged_in);
                verified = true;
                config.password.zeroize();
                config.token.zeroize();
                if save_settings_after_login {
                    config.save(&root).map_err(|_| Failure::Invalid)?;
                    save_settings_after_login = false;
                }
                let mut account = status.account.lock().unwrap();
                account.signed_in = true;
                if !account.busy {
                    account.message = format!("Signed in as {}", auth.as_ref().unwrap().username);
                }
            }
            if !enabled.load(Ordering::Acquire) {
                return Ok(());
            }
            let auth = auth.as_ref().ok_or(Failure::Invalid)?;
            // Rotate through pending awards so one rejected/deleted achievement does not
            // prevent other games from syncing or the library from being prepared.
            let mut deferred_error = None;
            let pending = {
                let store = store.lock().unwrap();
                store
                    .unlocks
                    .range((
                        std::ops::Bound::Excluded(last_award),
                        std::ops::Bound::Unbounded,
                    ))
                    .map(|(_, u)| u)
                    .find(|u| !u.synced)
                    .or_else(|| store.unlocks.values().find(|u| !u.synced))
                    .cloned()
            };
            if let Some(unlock) = pending {
                last_award = unlock.id;
                match award(&mut http, auth, &unlock, now()) {
                    Ok(()) => {
                        let mut store = store.lock().unwrap();
                        store.ack(unlock.id).map_err(|_| Failure::Invalid)?;
                        if store.unlocks.values().all(|u| u.synced) {
                            let _ = notices.send(Notice::status(0, "Achievements synced"));
                        }
                    }
                    Err(Failure::Network) => return Err(Failure::Network),
                    Err(error) => deferred_error = Some((error, http.diagnostic.clone())),
                }
            }
            if !enabled.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Some(game) = active.as_mut() {
                if !game.online {
                    let resolved = call(&mut http, auth, "gameid", &[("m", game.hash.clone())])?;
                    let id = resolved["GameID"].as_u64().ok_or(Failure::Invalid)?;
                    if id == 0 {
                        let _ = prepared.send(Prepared {
                            generation: game.generation,
                            epoch: game.epoch,
                            error: None,
                            hash: game.hash.clone(),
                            game: empty_game(),
                            unlocked: BTreeSet::new(),
                            cached: false,
                        });
                        let _ = notices.send(Notice::status(
                            game.generation,
                            "No achievements for this ROM",
                        ));
                        game.online = true;
                        game.ready = false;
                    } else {
                        let patch = call(&mut http, auth, "patch", &[("g", id.to_string())])?;
                        let data: Game = serde_json::from_value(patch["PatchData"].clone())
                            .map_err(|_| Failure::Invalid)?;
                        if data.console != 5 || u64::from(data.id) != id {
                            return Err(Failure::Invalid);
                        }
                        let session = call(
                            &mut http,
                            auth,
                            "startsession",
                            &[
                                ("g", id.to_string()),
                                ("h", "0".into()),
                                ("m", game.hash.clone()),
                                ("l", VERSION.into()),
                            ],
                        )?;
                        let ids = unlocked(&session)?;
                        storage::write(&dir.join(format!("{}.json", game.hash)), &(&data, &ids))
                            .map_err(|_| Failure::Invalid)?;
                        badges.enqueue(&data);
                        ping(&mut http, auth, &status, game, &data)?;
                        let _ = prepared.send(Prepared {
                            generation: game.generation,
                            epoch: game.epoch,
                            error: None,
                            hash: game.hash.clone(),
                            game: data,
                            unlocked: ids,
                            cached: false,
                        });
                        game.ready = true;
                        game.online = true;
                        game.last_ping = Instant::now();
                    }
                } else if game.ready && game.last_ping.elapsed() >= Duration::from_secs(120) {
                    let cached: (Game, BTreeSet<u32>) =
                        storage::read(&dir.join(format!("{}.json", game.hash)))
                            .map_err(|_| Failure::Invalid)?;
                    ping(&mut http, auth, &status, game, &cached.0)?;
                    game.last_ping = Instant::now();
                }
            }
            if !enabled.load(Ordering::Acquire) {
                return Ok(());
            }
            // One ROM per pass, with a one-second gap. Prefetch uses `unlocks`, not
            // `startsession`, so preparing a library doesn't claim the user played it.
            if let Some(path) = library.front() {
                if let Ok(hash) = crate::library::hash(&dir, path) {
                    match prefetch(&mut http, auth, &dir, &hash) {
                        Ok(game) => {
                            background_succeeded = true;
                            crate::library::checked(&dir, path, now());
                            badges.enqueue(&game);
                        }
                        Err(error) => {
                            library_failed |= !matches!(error, Failure::Network | Failure::Server);
                            background_error = Some((error, http.diagnostic.clone()));
                        }
                    }
                } else {
                    library_failed = true;
                }
                // Removed/unreadable ROMs are reconsidered on the next library scan.
                if background_error
                    .as_ref()
                    .is_none_or(|(error, _)| !matches!(error, Failure::Network | Failure::Server))
                {
                    library.pop_front();
                }
                if library.is_empty() && !library_failed && !badges.pending() {
                    let _ = notices.send(Notice::status(0, "Offline achievement cache ready"));
                }
            }
            if enabled.load(Ordering::Acquire) {
                badges.step(&mut http);
            }
            if let Some((error, detail)) = deferred_error {
                *http.diagnostic = detail;
                Err(error)
            } else {
                Ok(())
            }
        })();
        status.pending.store(
            !library.is_empty()
                || library_failed
                || badges.pending()
                || store.lock().unwrap().unlocks.values().any(|u| !u.synced)
                || active.as_ref().is_some_and(|game| !game.ready),
            std::sync::atomic::Ordering::Release,
        );
        status.progress.store(
            cache_progress(
                library_total,
                library.len(),
                badges.pending(),
                badges.percent(),
            )
            .map_or(0, |percent| percent + 1),
            std::sync::atomic::Ordering::Release,
        );
        if !enabled.load(Ordering::Acquire) {
            status.set(SyncStatus::Disabled);
            continue;
        }
        // Background cache requests do not describe the active game's verified session.
        let result = if result.is_ok() {
            if let Some((error, detail)) = background_error.as_ref() {
                if verified && active.as_ref().is_some_and(|game| game.online) {
                    if background_reported != Some(*error) {
                        eprintln!("slot: achievements: background {detail}");
                        background_reported = Some(*error);
                    }
                    Ok(())
                } else {
                    diagnostic = detail.clone();
                    Err(*error)
                }
            } else {
                if background_succeeded && background_reported.take().is_some() {
                    eprintln!("slot: achievements: background sync ok");
                }
                Ok(())
            }
        } else {
            result
        };
        match result {
            Ok(()) => {
                if failure_reported {
                    if let Some(game) = active.as_ref().filter(|game| game.online) {
                        let _ = prepared.send(Prepared {
                            generation: game.generation,
                            epoch: game.epoch,
                            error: Some(crate::GameAchievementState::Ready),
                            hash: game.hash.clone(),
                            game: empty_game(),
                            unlocked: BTreeSet::new(),
                            cached: false,
                        });
                        failure_reported = false;
                    }
                }
                status.set(
                    if verified && active.as_ref().is_some_and(|game| game.online) {
                        SyncStatus::Ready
                    } else if library_failed || badges.waiting {
                        SyncStatus::Attention
                    } else if !library.is_empty()
                        || badges.pending()
                        || store.lock().unwrap().unlocks.values().any(|u| !u.synced)
                    {
                        SyncStatus::Syncing
                    } else {
                        SyncStatus::Ready
                    },
                );
                warned = false;
                if background_error
                    .as_ref()
                    .is_some_and(|(error, _)| matches!(error, Failure::Network | Failure::Server))
                {
                    next_attempt = Instant::now() + Duration::from_secs(delay);
                    delay = (delay * 2).min(300);
                } else {
                    delay = 30;
                    next_attempt = Instant::now() + Duration::from_secs(1);
                }
            }
            Err(error) => {
                if let Some(game) = active.as_ref() {
                    failure_reported = true;
                    let state = if error == Failure::Network {
                        crate::GameAchievementState::Offline(
                            "Cannot reach RetroAchievements".into(),
                        )
                    } else if error == Failure::Server {
                        crate::GameAchievementState::ServerBusy
                    } else {
                        crate::GameAchievementState::Error(
                            match error {
                                Failure::Authentication => "Sign in again",
                                Failure::Rejected => "Achievement sync needs attention",
                                _ => "Achievement data or storage error",
                            }
                            .into(),
                        )
                    };
                    let _ = prepared.send(Prepared {
                        generation: game.generation,
                        epoch: game.epoch,
                        error: Some(state),
                        hash: game.hash.clone(),
                        game: empty_game(),
                        unlocked: BTreeSet::new(),
                        cached: false,
                    });
                }
                status.set_diagnostic(error.status(), &diagnostic);
                // Reauthenticate after errors. No rejected or malformed response consumes an award.
                verified = false;
                let quiet_offline = error == Failure::Network
                    && (active.as_ref().is_some_and(|g| g.ready)
                        || (active.is_none() && auth.is_some()));
                if !warned && !quiet_offline {
                    let generation = active.as_ref().map_or(0, |g| g.generation);
                    let title = match error {
                        Failure::Network if active.as_ref().is_some_and(|g| g.ready) => {
                            "Achievements offline - sync later"
                        }
                        Failure::Network => "Achievements need internet once",
                        Failure::Server => "Achievements: server busy, retrying",
                        Failure::Rejected => "Achievement sync needs attention",
                        Failure::Authentication => "Achievements: sign in again",
                        Failure::Invalid => "Achievements: data or storage error",
                    };
                    let _ = notices.send(Notice::status(generation, title));
                    warned = true;
                }
                next_attempt = Instant::now() + Duration::from_secs(delay);
                delay = (delay * 2).min(300);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn http_response_classification_does_not_confuse_status_with_connectivity() {
        let ok = json!({"Success":true});
        assert_eq!(classify_response(200, Some(ok.clone())), Ok(ok));
        let rejected = json!({"Success":false,"Error":"Missing game","Code":"invalid_parameter"});
        for (status, expected) in [
            (401, Failure::Authentication),
            (403, Failure::Authentication),
            (422, Failure::Rejected),
            (429, Failure::Server),
            (500, Failure::Server),
        ] {
            assert_eq!(
                classify_response(status, Some(rejected.clone())),
                Err(expected)
            );
        }
        let malformed = serde_json::from_slice::<Value>(b"not JSON").ok();
        assert_eq!(
            classify_response(200, malformed.clone()),
            Err(Failure::Invalid)
        );
        assert_eq!(
            classify_response(422, malformed.clone()),
            Err(Failure::Invalid)
        );
        assert_eq!(
            classify_response(401, malformed.clone()),
            Err(Failure::Authentication)
        );
        assert_eq!(
            classify_response(429, malformed.clone()),
            Err(Failure::Server)
        );
        assert_eq!(classify_response(500, malformed), Err(Failure::Server));
        for body in [
            json!({"Success":false,"Code":"invalid_token"}),
            json!({"Success":false,"Error":"The token has expired"}),
        ] {
            assert_eq!(
                classify_response(200, Some(body.clone())),
                Err(Failure::Authentication)
            );
            assert_eq!(
                classify_response(422, Some(body.clone())),
                Err(Failure::Authentication)
            );
            assert_eq!(success(body), Err(Failure::Authentication));
        }
        assert_eq!(
            transport_failure(&ureq::Error::HostNotFound),
            (Failure::Network, "network (DNS)")
        );
        assert_eq!(
            transport_failure(&ureq::Error::Timeout(ureq::Timeout::Global)),
            (Failure::Network, "network (timeout)")
        );
        let error = ureq::Error::Io(std::io::Error::other("https://example.com/?t=secret"));
        assert_eq!(
            transport_failure(&error),
            (Failure::Network, "network (connection)")
        );
    }

    #[test]
    fn diagnostics_only_include_fixed_operation_and_code_labels() {
        assert_eq!(operation_name("login2"), "login2");
        assert_eq!(operation_name("https://example.com/?t=secret"), "unknown");
        assert_eq!(
            diagnostic_code(&json!({"Code":"expired_token"})),
            Some("expired_token")
        );
        assert_eq!(
            diagnostic_code(&json!({"Code":"secret","Error":"secret"})),
            None
        );
        assert_eq!(
            diagnostic_code(&json!({"Code":"https://example.com/?t=secret"})),
            None
        );
    }

    #[test]
    fn missing_badges_are_rejections_and_server_errors_are_retryable() {
        assert_eq!(classify_badge_status(200), Ok(()));
        assert_eq!(classify_badge_status(404), Err(Failure::Rejected));
        assert_eq!(classify_badge_status(429), Err(Failure::Server));
        assert_eq!(classify_badge_status(503), Err(Failure::Server));
    }

    #[test]
    fn http_authentication_failure_keeps_the_legacy_password_fallback() {
        struct Expired(bool);
        impl Transport for Expired {
            fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure> {
                if !self.0 {
                    self.0 = true;
                    assert!(fields.iter().any(|(key, _)| *key == "t"));
                    Err(Failure::Authentication)
                } else {
                    assert!(fields.iter().any(|(key, _)| *key == "p"));
                    Ok(json!({"Success":true,"User":"Player","Token":"new-token"}))
                }
            }
        }
        let mut config = Config::default();
        config.username = "Player".into();
        config.password = "legacy-password".into();
        let remembered = Auth {
            username: "Player".into(),
            token: "expired-token".into(),
        };
        let result = login_detailed(&mut Expired(false), &config, Some(&remembered));
        assert_eq!(result.ok().unwrap().token, "new-token");
    }

    #[test]
    #[ignore = "requires loopback sockets; run explicitly on an unrestricted host"]
    fn real_http_status_responses_are_not_network_failures() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        for (status, body, expected) in [
            (
                422,
                r#"{"Success":false,"Error":"Missing game","Code":"invalid_parameter"}"#,
                Failure::Rejected,
            ),
            (
                401,
                r#"{"Success":false,"Error":"Expired token","Code":"expired_token"}"#,
                Failure::Authentication,
            ),
            (
                429,
                r#"{"Success":false,"Error":"Rate limit"}"#,
                Failure::Server,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/dorequest.php", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let headers = String::from_utf8(request).unwrap();
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                stream.read_exact(&mut vec![0; length]).unwrap();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let mut http = Http::new();
            let result = http.call_url(&url, &[("r", "gameid".into())]);
            assert_ne!(result, Err(Failure::Network));
            assert_eq!(result, Err(expected));
            assert!(http
                .diagnostic()
                .unwrap()
                .starts_with(&format!("http {status}")));
            server.join().unwrap();
        }
    }

    #[test]
    fn empty_session_unlock_lists_may_be_omitted() {
        assert!(unlocked(&json!({"Success":true})).unwrap().is_empty());
        assert_eq!(
            unlocked(&json!({"Unlocks":[{"ID":7,"When":123}]})).unwrap(),
            [7].into()
        );
        assert!(unlocked(&json!({"Unlocks":"invalid"})).is_err());
    }
    struct Fake {
        reply: Value,
        fields: Vec<(String, String)>,
    }
    impl Transport for Fake {
        fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure> {
            self.fields = fields
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            Ok(self.reply.clone())
        }
    }
    #[test]
    fn delayed_awards_use_original_time_and_softcore_signature() {
        let mut http = Fake {
            reply: json!({"Success":true}),
            fields: vec![],
        };
        let auth = Auth {
            username: "Player".into(),
            token: "secret".into(),
        };
        let unlock = Unlock {
            id: 123,
            hash: "abcdef".into(),
            earned_at: 1000,
            synced: false,
        };
        award(&mut http, &auth, &unlock, 1060).unwrap();
        let fields: std::collections::BTreeMap<_, _> = http.fields.into_iter().collect();
        assert_eq!(fields["o"], "60");
        assert_eq!(fields["h"], "0");
        assert_eq!(
            fields["v"],
            format!("{:x}", md5::compute("123Player012360"))
        );
    }
    #[test]
    fn an_http_success_is_not_an_award_acknowledgment() {
        let auth = Auth {
            username: "Player".into(),
            token: "secret".into(),
        };
        let unlock = Unlock {
            id: 1,
            hash: "a".into(),
            earned_at: 100,
            synced: false,
        };
        for (reply, expected) in [
            (
                json!({"Success":false,"Error":"Expired token"}),
                Failure::Authentication,
            ),
            (json!({}), Failure::Rejected),
        ] {
            let mut http = Fake {
                reply,
                fields: vec![],
            };
            assert_eq!(award(&mut http, &auth, &unlock, 50), Err(expected));
            assert!(!http.fields.iter().any(|(k, _)| k == "o"));
        }
    }
}

fn ping(
    http: &mut impl Transport,
    auth: &Auth,
    status: &Status,
    active: &Active,
    game: &Game,
) -> Result<(), Failure> {
    let presence = status
        .presence
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(generation, text)| *generation == active.generation && !text.is_empty())
        .map(|(_, text)| text.clone())
        .unwrap_or_else(|| format!("Playing {}", game.title));
    call(
        http,
        auth,
        "ping",
        &[
            ("g", game.id.to_string()),
            ("h", "0".into()),
            ("x", active.hash.clone()),
            ("m", presence),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    #[test]
    fn heartbeat_uses_current_generation_and_separate_hash_field() {
        struct Capture(Vec<(String, String)>);
        impl Transport for Capture {
            fn call(&mut self, fields: &[(&str, String)]) -> Result<Value, Failure> {
                self.0 = fields
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect();
                Ok(serde_json::json!({"Success":true}))
            }
        }
        let mut http = Capture(vec![]);
        let auth = Auth {
            username: "test".into(),
            token: "fixture".into(),
        };
        let status = Status::default();
        let active = Active {
            epoch: 0,
            generation: 2,
            hash: "rom-hash".into(),
            ready: true,
            online: true,
            last_ping: Instant::now(),
        };
        let game = Game {
            id: 1,
            console: 5,
            title: "Test".into(),
            presence: String::new(),
            achievements: vec![],
        };
        for (generation, text, expected) in [
            (1, "Old game", "Playing Test"),
            (2, "Level 3", "Level 3"),
            (2, "", "Playing Test"),
        ] {
            *status.presence.lock().unwrap() = Some((generation, text.into()));
            ping(&mut http, &auth, &status, &active, &game).unwrap();
            assert!(http.0.contains(&("m".into(), expected.into())));
            assert!(http.0.contains(&("x".into(), "rom-hash".into())));
        }
    }
}

// A fixed phase split keeps newly discovered badges from moving progress backwards.
fn cache_progress(
    total: usize,
    remaining: usize,
    badges_pending: bool,
    badge_percent: u8,
) -> Option<u8> {
    if remaining > 0 {
        Some((total.saturating_sub(remaining) as u64 * 50 / total.max(1) as u64) as u8)
    } else if badges_pending {
        Some(if total > 0 {
            50 + badge_percent / 2
        } else {
            badge_percent
        })
    } else {
        None
    }
}

#[cfg(test)]
mod cache_progress_tests {
    use super::cache_progress;
    #[test]
    fn progress_covers_metadata_then_badges_and_hides_when_finished() {
        assert_eq!(cache_progress(10, 10, false, 100), Some(0));
        assert_eq!(cache_progress(10, 5, true, 20), Some(25));
        assert_eq!(cache_progress(10, 1, true, 10), Some(45));
        assert_eq!(cache_progress(10, 0, true, 20), Some(60));
        assert_eq!(cache_progress(10, 0, false, 100), None);
        assert_eq!(cache_progress(0, 0, true, 62), Some(62));
        assert_eq!(cache_progress(0, 0, false, 100), None);
    }
}
