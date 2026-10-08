//! Home Wi-Fi scanning, asynchronous device work and button-only screen state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use slot_input::Btn;
use slot_store::{forget_wifi, read_wifi, save_wifi, WifiNetwork};
use slot_ui::{Keyboard, KeyboardInput, KeyboardResult, UndoFace, WifiMenu, WifiMenuRow};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyNetwork {
    pub ssid: String,
    pub signal: i32,
    pub secured: bool,
    pub supported: bool,
}

/// Parse `iw dev wlan0 scan`, retaining the strongest BSS for each visible SSID.
pub fn parse_scan(text: &str) -> Vec<NearbyNetwork> {
    let mut networks = BTreeMap::<String, NearbyNetwork>::new();
    let mut blocks = Vec::new();
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if bss_header(line) {
            if let Some(start) = start {
                blocks.push(&text[start..offset]);
            }
            start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(start) = start {
        blocks.push(&text[start..]);
    }
    for block in blocks {
        let mut ssid = None;
        let mut signal = None;
        let mut privacy = false;
        let mut rsn = false;
        let mut wpa = false;
        let mut psk = false;
        for line in block.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("SSID: ") {
                ssid = decode_ssid(value);
            }
            if let Some(value) = line.strip_prefix("signal: ") {
                signal = value
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                    .map(|v| v as i32);
            }
            if line.starts_with("capability:") && line.contains("Privacy") {
                privacy = true;
            }
            if line.starts_with("RSN:") {
                rsn = true;
            }
            if line.starts_with("WPA:") {
                wpa = true;
            }
            if line.starts_with("* Authentication suites:")
                && line.split_whitespace().any(|v| v == "PSK")
            {
                psk = true;
            }
        }
        let (Some(ssid), Some(signal)) = (ssid, signal) else {
            continue;
        };
        if ssid.is_empty() || ssid.len() > 32 || ssid.contains('\0') {
            continue;
        }
        let secured = privacy || rsn || wpa;
        let network = NearbyNetwork {
            ssid: ssid.clone(),
            signal,
            secured,
            supported: !secured || (rsn && psk),
        };
        if networks.get(&ssid).is_none_or(|n| signal > n.signal) {
            networks.insert(ssid, network);
        }
    }
    let mut networks: Vec<_> = networks.into_values().collect();
    networks.sort_by(|a, b| b.signal.cmp(&a.signal).then_with(|| a.ssid.cmp(&b.ssid)));
    networks
}

fn bss_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("BSS ") else {
        return false;
    };
    let Some(mac) = rest.as_bytes().get(..17) else {
        return false;
    };
    mac.iter().enumerate().all(|(i, b)| {
        if i % 3 == 2 {
            *b == b':'
        } else {
            b.is_ascii_hexdigit()
        }
    // iw prints "(on <dev>)" only when the reply carries an interface index.
    }) && matches!(rest.as_bytes().get(17), None | Some(b'(' | b' ' | b'\r' | b'\n'))
}

/// iw escapes non-printable bytes and literal backslashes as hexadecimal bytes.
pub fn decode_ssid(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at..].starts_with(b"\\x") && at + 4 <= bytes.len() {
            let digits = std::str::from_utf8(&bytes[at + 2..at + 4]).ok()?;
            decoded.push(u8::from_str_radix(digits, 16).ok()?);
            at += 4;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiStatus {
    pub ssid: Option<String>,
    pub ip: Option<String>,
    pub error: String,
}

/// Radio access is isolated from screen state, storage and host builds.
pub trait WifiRadio: Send {
    fn enable(&mut self) -> Result<(), String>;
    fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String>;
    fn reload(&mut self) -> Result<(), String>;
    fn connect(&mut self, ssid: &str) -> Result<(), String>;
    fn status(&mut self) -> Result<WifiStatus, String>;
}

#[derive(Default)]
pub struct HostWifi {
    pub networks: Vec<NearbyNetwork>,
    pub status: WifiStatus,
}

impl WifiRadio for HostWifi {
    fn enable(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String> {
        Ok(self.networks.clone())
    }
    fn reload(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn connect(&mut self, ssid: &str) -> Result<(), String> {
        self.status = WifiStatus {
            ssid: Some(ssid.into()),
            ip: Some("192.0.2.2".into()),
            error: String::new(),
        };
        Ok(())
    }
    fn status(&mut self) -> Result<WifiStatus, String> {
        Ok(self.status.clone())
    }
}

#[cfg(feature = "device")]
pub struct DeviceWifi {
    root: PathBuf,
}

#[cfg(feature = "device")]
impl DeviceWifi {
    fn command(&self, domain: &str, action: &str) -> Result<String, String> {
        let out = crate::link_radio::helper()
            .env("SLOT_ROOT", &self.root)
            .args([domain, action])
            .output()
            .map_err(|_| "Wi-Fi service unavailable".to_string())?;
        if !out.status.success() {
            // Only fixed service error codes are shown, never configuration or credentials.
            return Err(wifi_error(&String::from_utf8_lossy(&out.stderr)));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[cfg(feature = "device")]
impl WifiRadio for DeviceWifi {
    fn enable(&mut self) -> Result<(), String> {
        self.command("home", "on").map(|_| ())
    }
    fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String> {
        self.command("home", "scan").map(|text| parse_scan(&text))
    }
    fn reload(&mut self) -> Result<(), String> {
        self.command("home", "reload").map(|_| ())
    }
    fn connect(&mut self, ssid: &str) -> Result<(), String> {
        self.command("home", &format!("connect:{}", hex(ssid)))
            .map(|_| ())
    }
    fn status(&mut self) -> Result<WifiStatus, String> {
        self.command("service", "status").map(|s| parse_status(&s))
    }
}

pub fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_status(text: &str) -> WifiStatus {
    let value = |key| text.split_whitespace().find_map(|s| s.strip_prefix(key));
    let ssid = value("home_ssid=").and_then(|s| {
        if s.len() % 2 != 0 || !s.is_ascii() {
            return None;
        }
        let bytes: Option<Vec<_>> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
            .collect();
        String::from_utf8(bytes?).ok().filter(|s| !s.is_empty())
    });
    WifiStatus {
        ssid,
        ip: value("home_ip=")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        error: value("home_error=").unwrap_or("").into(),
    }
}

fn wifi_error(error: &str) -> String {
    match error.split_whitespace().last().unwrap_or("") {
        "CONFIG_TOO_MANY_NETWORKS" => "32 networks saved. Forget one first",
        "CONFIG_WRITE" | "CONFIG_SERIALIZE" => "Could not save Wi-Fi networks",
        code if code.starts_with("CONFIG_") => "Saved Wi-Fi networks could not be read",
        "SCAN_BUSY" => "A Wi-Fi scan is already running",
        "SCAN_CANCELLED" => "Wi-Fi scan cancelled",
        "LINK_BUSY" => "Wi-Fi is in use by Link",
        "EXTERNAL_OWNER" => "Wi-Fi is in use by another app",
        "HOME_DISABLED" => "Home Wi-Fi is Off",
        "RADIO_UNAVAILABLE" => "Wi-Fi radio unavailable",
        "TOOL_UNAVAILABLE" => "Wi-Fi tools unavailable",
        "COMMAND_TIMEOUT" | "COMMAND_FAILED" => "Wi-Fi request failed. Try again",
        "NETWORK_NOT_SAVED" => "Network is no longer saved. Rescan",
        "WRONG_PASSWORD" => "Wrong password",
        "NO_DHCP_LEASE" => "No DHCP lease",
        "HOME_CONNECT_FAILED" => "Could not connect to network",
        _ => return error.to_owned(),
    }
    .into()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WifiEffect {
    Scan,
    Enable,
    Connect(WifiNetwork),
    Forget(String),
    Back,
    Status,
}

pub enum WifiReply {
    Scan(Result<(Vec<NearbyNetwork>, Vec<WifiNetwork>, WifiStatus), String>),
    Changed(Result<Vec<WifiNetwork>, String>),
    Status(Result<WifiStatus, String>),
}

type WifiJob = (u64, WifiEffect, Option<Receiver<()>>);

pub struct WifiWorker {
    jobs: Sender<WifiJob>,
    replies: Receiver<(u64, WifiReply)>,
    generation: Arc<AtomicU64>,
}

impl WifiWorker {
    pub fn new(root: PathBuf, mut radio: Box<dyn WifiRadio>) -> Self {
        let (jobs, receive) = mpsc::channel::<WifiJob>();
        let (send, replies) = mpsc::channel();
        let current = Arc::new(AtomicU64::new(0));
        let generation = current.clone();
        std::thread::spawn(move || {
            while let Ok((generation, job, ready)) = receive.recv() {
                if current.load(Ordering::Acquire) != generation {
                    continue;
                }
                if let Some(ready) = ready {
                    if ready.recv().is_err() {
                        continue;
                    }
                }
                if current.load(Ordering::Acquire) != generation {
                    continue;
                }
                let reply = match job {
                    WifiEffect::Scan | WifiEffect::Enable => WifiReply::Scan((|| {
                        if job == WifiEffect::Enable {
                            radio.enable()?;
                        }
                        let saved = read_wifi(&root).map_err(str::to_owned)?;
                        Ok((radio.scan()?, saved, radio.status()?))
                    })(
                    )),
                    WifiEffect::Connect(network) => WifiReply::Changed((|| {
                        save_wifi(&root, network.clone()).map_err(str::to_owned)?;
                        radio.reload()?;
                        radio.connect(&network.ssid)?;
                        read_wifi(&root).map_err(str::to_owned)
                    })()),
                    WifiEffect::Forget(ssid) => WifiReply::Changed((|| {
                        forget_wifi(&root, &ssid).map_err(str::to_owned)?;
                        radio.reload()?;
                        read_wifi(&root).map_err(str::to_owned)
                    })()),
                    WifiEffect::Status => WifiReply::Status(radio.status()),
                    WifiEffect::Back => continue,
                };
                if send.send((generation, reply)).is_err() {
                    break;
                }
            }
        });
        Self {
            jobs,
            replies,
            generation,
        }
    }

    pub fn device_or_host(root: &Path) -> Self {
        #[cfg(feature = "device")]
        let radio = Box::new(DeviceWifi { root: root.into() });
        #[cfg(not(feature = "device"))]
        let radio = Box::new(HostWifi::default());
        Self::new(root.into(), radio)
    }

    pub fn ask(&self, generation: u64, effect: WifiEffect) {
        self.generation.store(generation, Ordering::Release);
        let _ = self.jobs.send((generation, effect, None));
    }
    pub fn scan_after_enable(&self, generation: u64, ready: Receiver<()>) {
        self.generation.store(generation, Ordering::Release);
        let _ = self.jobs.send((generation, WifiEffect::Scan, Some(ready)));
    }

    pub fn cancel(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }

    pub fn take(&self) -> Option<(u64, WifiReply)> {
        self.replies.try_recv().ok()
    }
}

impl Drop for WifiWorker {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Ssid,
    Password(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiScreen {
    pub enabled: bool,
    pub nearby: Vec<NearbyNetwork>,
    pub saved: Vec<WifiNetwork>,
    pub selected: usize,
    pub status: String,
    pub observed: WifiStatus,
    pub keyboard: Option<Keyboard>,
    entry: Option<Entry>,
    pub forget: Option<String>,
    pub busy: bool,
    connecting: Option<String>,
}

impl WifiScreen {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            nearby: Vec::new(),
            saved: Vec::new(),
            selected: 0,
            status: if enabled {
                "Scanning..."
            } else {
                "Press A to turn Home Wi-Fi On"
            }
            .into(),
            observed: WifiStatus::default(),
            keyboard: None,
            entry: None,
            forget: None,
            busy: enabled,
            connecting: None,
        }
    }

    fn entries(&self) -> Vec<(String, bool, bool, Option<i32>)> {
        let mut entries: Vec<_> = self
            .nearby
            .iter()
            .map(|n| (n.ssid.clone(), n.secured, n.supported, Some(n.signal)))
            .collect();
        for network in &self.saved {
            if !entries.iter().any(|(ssid, ..)| ssid == &network.ssid) {
                entries.push((network.ssid.clone(), network.password.is_some(), true, None));
            }
        }
        entries
    }

    pub fn rows(&self) -> Vec<WifiMenuRow> {
        let mut rows: Vec<_> = self
            .entries()
            .iter()
            .map(|(ssid, secured, _, signal)| WifiMenuRow {
                ssid: ssid.clone(),
                signal: *signal,
                secured: *secured,
                saved: self.saved.iter().any(|n| &n.ssid == ssid),
                connected: self.observed.ssid.as_ref() == Some(ssid) && self.observed.ip.is_some(),
            })
            .collect();
        rows.push(WifiMenuRow {
            ssid: "Other network...".into(),
            signal: None,
            secured: false,
            saved: false,
            connected: false,
        });
        rows
    }

    pub fn face(&self) -> UndoFace {
        if let Some(keyboard) = &self.keyboard {
            return keyboard.face();
        }
        WifiMenu {
            rows: &self.rows(),
            selected: self.selected,
            status: &self.status,
            enabled: self.enabled,
            forget: self.forget.as_deref(),
        }
        .face()
    }

    pub fn input(&mut self, button: Btn) -> Option<WifiEffect> {
        if let Some(keyboard) = &mut self.keyboard {
            let input = match button {
                Btn::Up => KeyboardInput::Up,
                Btn::Down => KeyboardInput::Down,
                Btn::Left => KeyboardInput::Left,
                Btn::Right => KeyboardInput::Right,
                Btn::A => KeyboardInput::Type,
                Btn::B => KeyboardInput::Delete,
                Btn::X => KeyboardInput::Caps,
                Btn::Y => KeyboardInput::Symbols,
                Btn::L1 => KeyboardInput::CursorLeft,
                Btn::R1 => KeyboardInput::CursorRight,
                Btn::Start => KeyboardInput::Confirm,
                Btn::Select => KeyboardInput::Reveal,
                _ => return None,
            };
            match keyboard.input(input) {
                KeyboardResult::Editing => {}
                KeyboardResult::Cancelled => {
                    self.keyboard = None;
                    self.entry = None;
                }
                KeyboardResult::Confirmed(text) => {
                    self.keyboard = None;
                    match self.entry.take() {
                        Some(Entry::Ssid) => {
                            self.password(text);
                            if let Some(keyboard) = self.keyboard.take() {
                                let mut keyboard = keyboard.allow_empty();
                                keyboard.hint = "Start with no password for an open network".into();
                                self.keyboard = Some(keyboard);
                            }
                        }
                        Some(Entry::Password(ssid)) => {
                            return self.connect(WifiNetwork {
                                ssid,
                                password: (!text.is_empty()).then_some(text),
                            })
                        }
                        None => {}
                    }
                }
            }
            return None;
        }
        if let Some(ssid) = self.forget.clone() {
            match button {
                Btn::B => self.forget = None,
                Btn::A => {
                    self.forget = None;
                    self.busy = true;
                    self.status = "Forgetting...".into();
                    self.connecting = None;
                    return Some(WifiEffect::Forget(ssid));
                }
                _ => {}
            }
            return None;
        }
        if button == Btn::B {
            return Some(WifiEffect::Back);
        }
        if self.busy {
            return None;
        }
        if !self.enabled {
            if button == Btn::A {
                self.enabled = true;
                self.busy = true;
                self.status = "Turning Home Wi-Fi On...".into();
                return Some(WifiEffect::Enable);
            }
            return None;
        }
        let entries = self.entries();
        let len = entries.len() + 1;
        match button {
            Btn::Up => self.selected = (self.selected + len - 1) % len,
            Btn::Down => self.selected = (self.selected + 1) % len,
            Btn::Y => {
                self.busy = true;
                self.status = "Scanning...".into();
                return Some(WifiEffect::Scan);
            }
            Btn::X => {
                if let Some((ssid, ..)) = entries.get(self.selected) {
                    if self.saved.iter().any(|n| &n.ssid == ssid) {
                        self.forget = Some(ssid.clone());
                    }
                }
            }
            Btn::A => {
                let Some((ssid, secured, supported, _)) = entries.get(self.selected) else {
                    self.keyboard = Some(Keyboard::new("Network name (SSID)", 1, 32, false));
                    self.entry = Some(Entry::Ssid);
                    return None;
                };
                if let Some(network) = self.saved.iter().find(|n| &n.ssid == ssid).cloned() {
                    return self.connect(network);
                }
                if !supported {
                    self.status = "Only WPA2 personal or open networks are supported".into();
                } else if *secured {
                    self.password(ssid.clone());
                } else {
                    return self.connect(WifiNetwork {
                        ssid: ssid.clone(),
                        password: None,
                    });
                }
            }
            _ => {}
        }
        None
    }

    fn password(&mut self, ssid: String) {
        self.keyboard = Some(Keyboard::new(format!("Password for {ssid}"), 8, 63, true));
        self.entry = Some(Entry::Password(ssid));
    }

    fn connect(&mut self, network: WifiNetwork) -> Option<WifiEffect> {
        self.status = format!("Connecting to {}...", network.ssid);
        self.connecting = Some(network.ssid.clone());
        self.busy = true;
        Some(WifiEffect::Connect(network))
    }

    pub fn reply(&mut self, reply: WifiReply) {
        match reply {
            WifiReply::Scan(Ok((nearby, saved, status))) => {
                self.nearby = nearby;
                self.saved = saved;
                self.selected = self.selected.min(self.entries().len());
                self.busy = false;
                self.status = "Choose a network".into();
                self.observe(status);
            }
            WifiReply::Scan(Err(error)) | WifiReply::Changed(Err(error)) => {
                self.busy = false;
                self.connecting = None;
                self.status = wifi_error(&error);
                if self.status == "Home Wi-Fi is Off" {
                    self.enabled = false;
                }
            }
            WifiReply::Changed(Ok(saved)) => {
                self.saved = saved;
                self.selected = self.selected.min(self.entries().len());
                self.busy = false;
                if self.connecting.is_none() {
                    self.status = "Network forgotten".into();
                }
            }
            WifiReply::Status(Ok(status)) => self.observe(status),
            WifiReply::Status(Err(error)) => self.status = wifi_error(&error),
        }
    }

    fn observe(&mut self, status: WifiStatus) {
        if let (Some(ssid), Some(ip)) = (&status.ssid, &status.ip) {
            if self.connecting.as_ref().is_none_or(|target| target == ssid) {
                self.status = format!("Connected: {ip} ({ssid})");
                self.connecting = None;
            }
        } else if !status.error.is_empty() && status.error != "NO_NETWORKS" {
            self.status = wifi_error(&status.error);
            self.connecting = None;
        }
        self.observed = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "BSS 00:11:22:33:44:55(on wlan0)\n\tcapability: ESS Privacy (0x0011)\n\tsignal: -64.00 dBm\n\tSSID: Home\n\tRSN:\n\t\t * Authentication suites: PSK\nBSS 00:11:22:33:44:56(on wlan0)\n\tcapability: ESS Privacy\n\tsignal: -36.00 dBm\n\tSSID: Home\n\tRSN:\n\t\t * Authentication suites: PSK\nBSS 00:11:22:33:44:57(on wlan0)\n\tcapability: ESS\n\tsignal: -49.00 dBm\n\tSSID: Guest cafe\nBSS 00:11:22:33:44:58(on wlan0)\n\tsignal: -10.00 dBm\n\tSSID: \nBSS 00:11:22:33:44:59(on wlan0)\n\tcapability: ESS Privacy\n\tsignal: -70.00 dBm\n\tSSID: Office\n\tRSN:\n\t\t * Authentication suites: IEEE 802.1X\n";

    #[test]
    fn iw_scan_deduplicates_sorts_skips_hidden_and_distinguishes_security() {
        let scan = parse_scan(SCAN);
        assert_eq!(
            scan.iter()
                .map(|n| (n.ssid.as_str(), n.signal, n.secured, n.supported))
                .collect::<Vec<_>>(),
            vec![
                ("Home", -36, true, true),
                ("Guest cafe", -49, false, true),
                ("Office", -70, true, false)
            ]
        );
        assert!(parse_scan("BSS broken\nSSID: Name\nsignal: NaN dBm\n").is_empty());
        assert_eq!(decode_ssid("a\\x5cb\\xc3\\xa9"), Some("a\\bé".into()));
        assert_eq!(decode_ssid("\\x00"), Some("\0".into()));
        assert!(parse_scan("BSS 00\nsignal: -20 dBm\nSSID: \\x00\n").is_empty());
    }

    #[test]
    fn iw_headers_do_not_split_ssids_or_information_elements() {
        let text = "BSS aa:bb:cc:dd:ee:01(on wlan0)\n\tcapability: ESS\n\tsignal: -30.00 dBm\n\tSSID: Cafe BSS Lounge\nBSS aa:bb:cc:dd:ee:02(on wlan0) -- associated\n\tcapability: ESS Privacy\n\tsignal: -40.00 dBm\n\tSSID: Home\n\tBSS Load:\n\t\t * station count: 2\n\tRSN:\n\t\t * Authentication suites: PSK\n";
        let networks = parse_scan(text);
        assert_eq!(networks.len(), 2);
        assert_eq!(networks[0].ssid, "Cafe BSS Lounge");
        assert!(!networks[0].secured);
        assert_eq!(networks[1].ssid, "Home");
        assert!(networks[1].secured && networks[1].supported);
        assert!(!bss_header("\tBSS aa:bb:cc:dd:ee:03(on wlan0)"));
        assert!(!bss_header("BSS Load:"));
        assert!(!bss_header("BSS aa:bb:cc:dd:ee:gg(on wlan0)"));
        assert_eq!(
            wifi_error("slot-services: 1 SCAN_BUSY"),
            "A Wi-Fi scan is already running"
        );
    }

    #[test]
    fn stale_jobs_are_skipped_before_radio_or_storage_changes() {
        use std::sync::Mutex;
        use std::time::Duration;
        struct Radio {
            calls: Arc<Mutex<Vec<&'static str>>>,
            started: Sender<()>,
            release: Receiver<()>,
        }
        impl WifiRadio for Radio {
            fn enable(&mut self) -> Result<(), String> {
                self.calls.lock().unwrap().push("home on");
                Ok(())
            }
            fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String> {
                self.calls.lock().unwrap().push("scan");
                self.started.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(2)).unwrap();
                Err("HOME_DISABLED".into())
            }
            fn reload(&mut self) -> Result<(), String> {
                self.calls.lock().unwrap().push("reload");
                Ok(())
            }
            fn connect(&mut self, _: &str) -> Result<(), String> {
                self.calls.lock().unwrap().push("connect");
                Ok(())
            }
            fn status(&mut self) -> Result<WifiStatus, String> {
                self.calls.lock().unwrap().push("status");
                Ok(WifiStatus::default())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (started, start) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let worker = WifiWorker::new(
            root.path().into(),
            Box::new(Radio {
                calls: calls.clone(),
                started,
                release: wait,
            }),
        );
        worker.ask(1, WifiEffect::Scan);
        start.recv_timeout(Duration::from_secs(1)).unwrap();
        worker.ask(2, WifiEffect::Enable);
        worker.ask(
            2,
            WifiEffect::Connect(WifiNetwork {
                ssid: "Stale".into(),
                password: None,
            }),
        );
        worker.cancel(3);
        worker.ask(3, WifiEffect::Status);
        release.send(()).unwrap();
        let (_, reply) = worker.replies.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(reply, WifiReply::Scan(Err(ref e)) if e == "HOME_DISABLED"));
        let (generation, _) = worker.replies.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(generation, 3);
        assert_eq!(*calls.lock().unwrap(), vec!["scan", "status"]);
        assert!(read_wifi(root.path()).unwrap().is_empty());
        let mut screen = WifiScreen::new(true);
        screen.reply(reply);
        assert!(!screen.enabled);
        assert_eq!(screen.status, "Home Wi-Fi is Off");
    }

    fn screen(saved: Vec<WifiNetwork>) -> WifiScreen {
        let mut screen = WifiScreen::new(true);
        screen.reply(WifiReply::Scan(Ok((
            parse_scan(SCAN),
            saved,
            WifiStatus::default(),
        ))));
        screen
    }

    #[test]
    fn unsaved_secured_network_requires_a_valid_password_before_connecting() {
        let mut s = screen(Vec::new());
        assert_eq!(s.input(Btn::A), None);
        assert!(s.keyboard.as_ref().unwrap().masked());
        assert_eq!(s.input(Btn::Start), None);
        assert_eq!(
            s.keyboard.as_ref().unwrap().hint,
            "Enter at least 8 characters"
        );
        for _ in 0..8 {
            s.input(Btn::A);
        }
        assert_eq!(
            s.input(Btn::Start),
            Some(WifiEffect::Connect(WifiNetwork {
                ssid: "Home".into(),
                password: Some("aaaaaaaa".into())
            }))
        );
        assert!(s.busy);
        assert!(s.status.starts_with("Connecting"));
    }

    #[test]
    fn open_and_saved_networks_connect_without_keyboard_and_forget_requires_confirmation() {
        let home = WifiNetwork {
            ssid: "Home".into(),
            password: Some("password".into()),
        };
        let mut s = screen(vec![home.clone()]);
        assert_eq!(s.input(Btn::X), None);
        assert_eq!(s.forget.as_deref(), Some("Home"));
        s.input(Btn::B);
        assert!(s.forget.is_none());
        assert_eq!(s.saved, vec![home.clone()]);
        assert_eq!(s.input(Btn::A), Some(WifiEffect::Connect(home.clone())));
        s.reply(WifiReply::Changed(Ok(vec![home])));
        s.input(Btn::X);
        assert_eq!(s.input(Btn::A), Some(WifiEffect::Forget("Home".into())));
        s.reply(WifiReply::Changed(Ok(Vec::new())));
        s.input(Btn::Down);
        assert_eq!(
            s.input(Btn::A),
            Some(WifiEffect::Connect(WifiNetwork {
                ssid: "Guest cafe".into(),
                password: None
            }))
        );
        assert!(s.keyboard.is_none());
    }

    #[test]
    fn off_screen_enables_before_scanning_and_back_is_available_during_scan() {
        let mut s = WifiScreen::new(false);
        assert_eq!(s.input(Btn::Y), None);
        assert_eq!(s.input(Btn::A), Some(WifiEffect::Enable));
        assert!(s.enabled && s.busy);
        assert_eq!(s.input(Btn::B), Some(WifiEffect::Back));
        s.reply(WifiReply::Scan(Ok((
            Vec::new(),
            Vec::new(),
            WifiStatus::default(),
        ))));
        assert_eq!(s.input(Btn::Y), Some(WifiEffect::Scan));
    }

    #[test]
    fn hidden_network_flow_and_keyboard_cancel_return_to_the_list() {
        let mut s = screen(Vec::new());
        s.input(Btn::Up);
        s.input(Btn::A);
        assert_eq!(s.keyboard.as_ref().unwrap().title, "Network name (SSID)");
        s.input(Btn::B);
        assert!(s.keyboard.is_none());
        s.input(Btn::A);
        s.input(Btn::A);
        s.input(Btn::Start);
        assert_eq!(s.keyboard.as_ref().unwrap().title, "Password for a");
        for _ in 0..8 {
            s.input(Btn::A);
        }
        assert_eq!(
            s.input(Btn::Start),
            Some(WifiEffect::Connect(WifiNetwork {
                ssid: "a".into(),
                password: Some("aaaaaaaa".into())
            }))
        );
    }

    #[test]
    fn hidden_open_network_accepts_empty_password_but_refuses_partial_wpa_keys() {
        let mut s = screen(Vec::new());
        s.input(Btn::Up);
        s.input(Btn::A);
        s.input(Btn::A);
        s.input(Btn::Start);
        assert_eq!(
            s.input(Btn::Start),
            Some(WifiEffect::Connect(WifiNetwork {
                ssid: "a".into(),
                password: None
            }))
        );
        let mut s = screen(Vec::new());
        s.input(Btn::Up);
        s.input(Btn::A);
        s.input(Btn::A);
        s.input(Btn::Start);
        s.input(Btn::A);
        assert_eq!(s.input(Btn::Start), None);
        assert_eq!(
            s.keyboard.as_ref().unwrap().hint,
            "Enter at least 8 characters"
        );
        assert_eq!(
            wifi_error("CONFIG_TOO_MANY_NETWORKS"),
            "32 networks saved. Forget one first"
        );
    }

    #[test]
    fn results_include_address_and_specific_failure_hints() {
        let status = parse_status(
            "0 home_ssid=486f6d65 home_ip=192.168.1.2 home_error= home_connected=true",
        );
        assert_eq!(status.ssid.as_deref(), Some("Home"));
        let mut s = screen(Vec::new());
        s.reply(WifiReply::Status(Ok(status)));
        assert_eq!(s.status, "Connected: 192.168.1.2 (Home)");
        assert!(s.rows()[0].connected);
        for (error, hint) in [
            ("WRONG_PASSWORD", "Wrong password"),
            ("NO_DHCP_LEASE", "No DHCP lease"),
        ] {
            s.reply(WifiReply::Status(Ok(WifiStatus {
                error: error.into(),
                ..Default::default()
            })));
            assert_eq!(s.status, hint);
        }
        assert_eq!(parse_status("home_ssid=éé home_ip= home_error=").ssid, None);
    }

    #[test]
    fn worker_saves_before_reload_and_connect_and_preserves_other_profiles() {
        use std::sync::{Arc, Mutex};
        struct Radio(Arc<Mutex<Vec<String>>>);
        impl WifiRadio for Radio {
            fn enable(&mut self) -> Result<(), String> {
                self.0.lock().unwrap().push("enable".into());
                Ok(())
            }
            fn scan(&mut self) -> Result<Vec<NearbyNetwork>, String> {
                self.0.lock().unwrap().push("scan".into());
                Ok(parse_scan(SCAN))
            }
            fn reload(&mut self) -> Result<(), String> {
                self.0.lock().unwrap().push("reload".into());
                Ok(())
            }
            fn connect(&mut self, ssid: &str) -> Result<(), String> {
                self.0.lock().unwrap().push(format!("connect:{ssid}"));
                Ok(())
            }
            fn status(&mut self) -> Result<WifiStatus, String> {
                Ok(WifiStatus::default())
            }
        }
        let root = tempfile::tempdir().unwrap();
        save_wifi(
            root.path(),
            WifiNetwork {
                ssid: "Old".into(),
                password: None,
            },
        )
        .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let worker = WifiWorker::new(root.path().into(), Box::new(Radio(calls.clone())));
        worker.ask(1, WifiEffect::Enable);
        let (generation, reply) = worker
            .replies
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(generation, 1);
        assert!(matches!(reply, WifiReply::Scan(Ok(_))));
        worker.ask(
            1,
            WifiEffect::Connect(WifiNetwork {
                ssid: "Home".into(),
                password: Some("password".into()),
            }),
        );
        assert!(matches!(
            worker
                .replies
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap()
                .1,
            WifiReply::Changed(Ok(_))
        ));
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["enable", "scan", "reload", "connect:Home"]
        );
        assert_eq!(
            read_wifi(root.path())
                .unwrap()
                .iter()
                .map(|n| n.ssid.as_str())
                .collect::<Vec<_>>(),
            vec!["Home", "Old"]
        );
        worker.ask(2, WifiEffect::Forget("Home".into()));
        let (generation, reply) = worker
            .replies
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(generation, 2);
        assert!(matches!(reply, WifiReply::Changed(Ok(_))));
        assert_eq!(read_wifi(root.path()).unwrap()[0].ssid, "Old");
    }

    #[test]
    fn screen_face_has_a_full_panel_and_legends_fit() {
        let mut s = screen(Vec::new());
        assert_eq!(s.face().rgba.len(), 720 * 480 * 4);
        s.enabled = false;
        s.face();
        s.forget = Some("Home".into());
        s.face();
        if let Ok(dir) = std::env::var("SCRATCH_PNG_DIR") {
            let mut list = screen(vec![WifiNetwork {
                ssid: "Home".into(),
                password: Some("password".into()),
            }]);
            list.observed = WifiStatus {
                ssid: Some("Home".into()),
                ip: Some("192.168.1.2".into()),
                error: String::new(),
            };
            let mut keyboard = Keyboard::new("Password for MyNetwork", 8, 63, true);
            keyboard.input(KeyboardInput::Type);
            keyboard.input(KeyboardInput::Reveal);
            for (name, face) in [
                ("wifi-list", list.face()),
                ("wifi-keyboard", keyboard.face()),
            ] {
                let file =
                    std::fs::File::create(Path::new(&dir).join(format!("{name}.png"))).unwrap();
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), face.w, face.h);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&face.rgba)
                    .unwrap();
            }
        }
    }
}
