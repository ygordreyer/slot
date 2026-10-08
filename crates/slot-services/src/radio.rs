//! Home and Link radio ownership, association, and bounded scanning.

use crate::config::{self, Network};
use crate::system::{self, field, output, Process};
use slot_store::wifi_status::{WifiFailure, WifiPhase, WifiSignal, WifiStatus};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn supplicant_file_logging(help: &str) -> bool {
    help.split_whitespace()
        .any(|word| word == "-f" || word.starts_with("[-f<"))
}

fn file_logging_supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        output("wpa_supplicant", &["-h"]).is_ok_and(|help| supplicant_file_logging(&help))
    })
}

fn scan_busy(stderr: &str) -> bool {
    stderr.contains("Device or resource busy") || stderr.contains("(-16)")
}

fn scan_with(
    deadline: Instant,
    mut command: impl FnMut(bool, Duration) -> Result<String, system::CommandError>,
) -> Result<String, &'static str> {
    let mut run = |cached, reserve| {
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .saturating_sub(reserve);
        if remaining.is_zero() {
            return Err(system::CommandError {
                code: "COMMAND_TIMEOUT",
                stderr: String::new(),
            });
        }
        command(cached, remaining)
    };
    match run(false, Duration::ZERO) {
        Ok(scan) => Ok(scan),
        Err(e) if scan_busy(&e.stderr) => {
            std::thread::sleep(
                Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())),
            );
            // Leave time to read cached results even if the retry times out.
            run(false, Duration::from_secs(1))
                .or_else(|_| run(true, Duration::ZERO))
                .map_err(|e| e.code)
        }
        Err(e) => Err(e.code),
    }
}

fn prepare_scan_with(
    powered_down: &mut bool,
    mut command: impl FnMut(&str, &[&str]) -> Result<String, &'static str>,
) -> Result<(), &'static str> {
    // Even a failed unblock may have changed the radio before reporting failure.
    *powered_down = false;
    command("rfkill", &["unblock", "wifi"])?;
    command("ip", &["link", "set", "wlan0", "up"])?;
    Ok(())
}

pub fn scan(cancelled: &AtomicBool) -> Result<String, &'static str> {
    scan_with(
        Instant::now() + Duration::from_secs(10),
        |cached, timeout| {
            let args = if cached {
                &["dev", "wlan0", "scan", "dump"][..]
            } else {
                &["dev", "wlan0", "scan"][..]
            };
            system::output_timeout_cancellable("iw", args, timeout, cancelled)
        },
    )
}

pub fn net_exists(name: &str) -> bool {
    std::env::var_os("SLOT_NET_SYS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/net"))
        .join(name)
        .exists()
}

pub struct Interface {
    pub name: &'static str,
    run: PathBuf,
    pub wpa: Option<Process>,
    dhcp: Option<Process>,
    address: Option<&'static str>,
}
impl Interface {
    pub fn new(name: &'static str, run: &Path) -> Self {
        let wpa = Process::adopt(run.join(format!("{name}.wpa.pid")), run);
        let address = if name == "wlan1" && wpa.is_some() {
            let conf = fs::read_to_string(run.join("wlan1.conf")).unwrap_or_default();
            Some(if conf.lines().any(|l| l == "mode=2") {
                "10.42.0.1/24"
            } else {
                "10.42.0.2/24"
            })
        } else {
            None
        };
        Self {
            name,
            run: run.to_owned(),
            wpa,
            dhcp: Process::adopt(run.join(format!("{name}.dhcp.pid")), run),
            address,
        }
    }
    fn ctrl(&self) -> PathBuf {
        self.run.join(self.name)
    }
    pub fn pids(&self) -> Vec<u32> {
        self.wpa
            .iter()
            .chain(self.dhcp.iter())
            .map(Process::pid)
            .collect()
    }
    pub fn observed_frequency(&self) -> Option<u32> {
        // Some drivers (the H700's 8821cs) never print a `channel` line in `iw dev info`, on a
        // station or an AP alike, so ask the supplicant that is holding the channel.
        output("iw", &["dev", self.name, "info"])
            .ok()
            .and_then(|info| observed_frequency(&info))
            .or_else(|| supplicant_frequency(&self.status()))
    }
    pub fn status(&self) -> String {
        if self.wpa.is_none() {
            return String::new();
        }
        output(
            "wpa_cli",
            &[
                "-p",
                self.ctrl().to_str().unwrap(),
                "-i",
                self.name,
                "status",
            ],
        )
        .unwrap_or_default()
    }
    pub fn start(&mut self, n: &Network, freq: Option<u32>, ap: bool) -> Result<(), &'static str> {
        self.stop();
        let ctrl = self.ctrl();
        fs::create_dir_all(&ctrl).map_err(|_| "RUNTIME_DIRECTORY")?;
        let _ = fs::remove_file(ctrl.join(self.name));
        let conf = self.run.join(format!("{}.conf", self.name));
        system::private_write(&conf, &config::supplicant(n, &ctrl, freq, ap))
            .map_err(|_| "RUNTIME_CONFIG")?;
        output("ip", &["link", "set", self.name, "up"])?;
        let log = self.run.join(format!("{}.log", self.name));
        let _ = fs::remove_file(&log);
        let mut args = vec!["-i", self.name, "-c", conf.to_str().unwrap(), "-Dnl80211"];
        if file_logging_supported() {
            args.extend(["-f", log.to_str().unwrap()]);
        }
        self.wpa = Some(Process::spawn(
            "wpa_supplicant",
            &args,
            self.run.join(format!("{}.wpa.pid", self.name)),
        )?);
        Ok(())
    }
    pub fn dhcp(&mut self) -> Result<(), &'static str> {
        if self.dhcp.as_mut().is_some_and(Process::running) {
            return Ok(());
        }
        // Stay foreground for renewals. -n exits after a bounded initial failure.
        self.dhcp = Some(Process::spawn(
            "udhcpc",
            &[
                "-f",
                "-n",
                "-i",
                self.name,
                "-t",
                "3",
                "-T",
                "3",
                "-s",
                "/usr/share/udhcpc/default.script",
                "-p",
                self.run.join("home-dhcp.pid").to_str().unwrap(),
            ],
            self.run.join(format!("{}.dhcp.pid", self.name)),
        )?);
        Ok(())
    }
    pub fn has_ip(&self) -> bool {
        output("ip", &["-4", "-o", "addr", "show", "dev", self.name])
            .is_ok_and(|s| s.contains(" inet "))
    }
    /// The first IPv4 address on this interface, as a bare address.
    pub fn ipv4(&self) -> Option<std::net::Ipv4Addr> {
        let out = output("ip", &["-4", "-o", "addr", "show", "dev", self.name]).ok()?;
        first_ipv4(&out)
    }
    pub fn restore_address(&mut self, address: &'static str) {
        self.address = Some(address);
    }
    pub fn address(&mut self, address: &'static str) -> Result<(), &'static str> {
        // Retain ownership even if the command adds the address but its reply times out.
        self.address = Some(address);
        output("ip", &["addr", "add", address, "dev", self.name])?;
        Ok(())
    }
    pub fn stop(&mut self) {
        let owned = self.wpa.is_some() || self.dhcp.is_some() || self.address.is_some();
        self.dhcp.take();
        self.wpa.take();
        if let Some(addr) = self.address.take() {
            let _ = output("ip", &["addr", "del", addr, "dev", self.name]);
        }
        if owned {
            if self.name == "wlan0" {
                // We exclusively owned this interface and its DHCP lease. Never flush Link.
                let _ = output("ip", &["-4", "addr", "flush", "dev", self.name]);
                let _ = output("ip", &["route", "flush", "dev", self.name]);
            }
            let _ = output("ip", &["link", "set", self.name, "down"]);
        }
        let _ = fs::remove_file(self.run.join(format!("{}.conf", self.name)));
    }
}
impl Drop for Interface {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn observed_frequency(info: &str) -> Option<u32> {
    info.lines().find_map(|line| {
        if !line.trim_start().starts_with("channel ") {
            return None;
        }
        line.split_once('(')?
            .1
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

/// The `freq=` line of `wpa_cli status`, which is only meaningful once the link is up.
pub fn supplicant_frequency(status: &str) -> Option<u32> {
    if !connected(status) {
        return None;
    }
    field(status, "freq")?.parse().ok().filter(|f| *f > 0)
}

/// The first `inet` address in `ip -4 -o addr show` output, without its prefix length.
pub fn first_ipv4(text: &str) -> Option<std::net::Ipv4Addr> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        words.find(|w| *w == "inet")?;
        words.next()?.split('/').next()?.parse().ok()
    })
}

pub fn connected(status: &str) -> bool {
    field(status, "wpa_state") == Some("COMPLETED")
}

fn connection_phase(status: &str, has_ip: bool, attempting: bool, error: &str) -> WifiPhase {
    if connected(status) && has_ip {
        return WifiPhase::Connected;
    }
    if !error.is_empty() && error != "NO_NETWORKS" {
        return WifiPhase::Failed(WifiFailure::from_error(error));
    }
    match field(status, "wpa_state") {
        Some("COMPLETED") => WifiPhase::ObtainingAddress,
        Some("AUTHENTICATING" | "4WAY_HANDSHAKE" | "GROUP_HANDSHAKE") => WifiPhase::Authenticating,
        Some("SCANNING" | "ASSOCIATING" | "ASSOCIATED") => WifiPhase::Associating,
        _ if attempting => WifiPhase::Associating,
        _ => WifiPhase::Idle,
    }
}

fn iw_signal(text: &str) -> Option<i32> {
    if !text.lines().any(|line| line.starts_with("Connected to ")) {
        return None;
    }
    text.lines().find_map(|line| {
        let mut words = line.trim().strip_prefix("signal:")?.split_whitespace();
        let dbm = words.next()?.parse::<i32>().ok()?;
        (words.next()? == "dBm" && (-127..=0).contains(&dbm)).then_some(dbm)
    })
}

fn wireless_quality(text: &str, interface: &str) -> Option<u8> {
    text.lines().find_map(|line| {
        let (name, values) = line.split_once(':')?;
        if name.trim() != interface {
            return None;
        }
        let quality = values
            .split_whitespace()
            .nth(1)?
            .trim_end_matches('.')
            .parse::<u32>()
            .ok()?;
        // Wireless extensions conventionally report link quality on a scale of 70.
        Some((quality.min(70) * 100 / 70) as u8)
    })
}

fn read_signal_with(
    interface: &str,
    proc_path: &Path,
    mut command: impl FnMut(&str, &[&str]) -> Result<String, &'static str>,
) -> Option<WifiSignal> {
    if let Ok(text) = command("iw", &["dev", interface, "link"]) {
        if text.trim() == "Not connected." {
            return None;
        }
        if let Some(dbm) = iw_signal(&text) {
            return Some(WifiSignal::Dbm(dbm));
        }
    }
    wireless_quality(&fs::read_to_string(proc_path).ok()?, interface).map(WifiSignal::Quality)
}

struct SignalCache {
    value: Option<WifiSignal>,
    refresh_at: Instant,
}

impl SignalCache {
    fn new() -> Self {
        Self {
            value: None,
            refresh_at: Instant::now(),
        }
    }

    fn refresh_with(
        &mut self,
        now: Instant,
        linked: bool,
        interface: &str,
        proc_path: &Path,
        command: impl FnMut(&str, &[&str]) -> Result<String, &'static str>,
    ) {
        if !linked {
            self.value = None;
        } else if now >= self.refresh_at {
            self.refresh_at = now + Duration::from_secs(3);
            self.value = read_signal_with(interface, proc_path, command);
        }
    }
}

// Do not infer a safe transmit channel from a configured frequency. Read regulatory flags.
pub fn permitted(info: &str, freq: u32) -> bool {
    info.lines().any(|l| {
        l.contains(&format!("{freq} MHz"))
            && !["disabled", "no IR", "radar", "passive"]
                .iter()
                .any(|s| l.contains(s))
    })
}
pub fn capabilities() -> Result<String, &'static str> {
    output("iw", &["list"])
}

pub fn subnet_conflict(routes: &str) -> bool {
    routes.lines().any(|line| {
        if line.contains(" dev wlan1") {
            return false;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let Some(prefix) = (if ["local", "broadcast", "unreachable", "blackhole"].contains(&first) {
            words.next()
        } else {
            Some(first)
        }) else {
            return false;
        };
        if prefix == "default" {
            return false;
        }
        let (ip, bits) = prefix.split_once('/').unwrap_or((prefix, "32"));
        let Ok(ip) = ip.parse::<std::net::Ipv4Addr>() else {
            return false;
        };
        let Ok(bits) = bits.parse::<u32>() else {
            return true;
        };
        if bits > 32 {
            return true;
        }
        let mask = if bits == 0 {
            0
        } else {
            u32::MAX << (32 - bits.min(24))
        };
        (u32::from(ip) & mask) == (u32::from(std::net::Ipv4Addr::new(10, 42, 0, 0)) & mask)
    })
}

struct HomeLink {
    status: String,
    dhcp: Result<(), &'static str>,
    has_ip: bool,
}

pub struct Home {
    pub interface: Interface,
    pub enabled: bool,
    pub error: &'static str,
    profiles: Vec<Network>,
    next: usize,
    deadline: Instant,
    retry: Instant,
    connected: bool,
    requested: bool,
    pub active_ssid: String,
    signal: SignalCache,
    signal_command: fn(&str, &[&str]) -> Result<String, &'static str>,
}
impl Home {
    pub fn new(run: &Path) -> Self {
        let interface = Interface::new("wlan0", run);
        let connected = connected(&interface.status()) && interface.has_ip();
        let active_ssid = system::field(&interface.status(), "ssid")
            .unwrap_or("")
            .to_owned();
        Self {
            interface,
            enabled: false,
            error: "",
            profiles: Vec::new(),
            next: 0,
            deadline: Instant::now() + Duration::from_secs(25),
            retry: Instant::now(),
            active_ssid,
            requested: false,
            connected,
            signal: SignalCache::new(),
            signal_command: |program, args| {
                system::output_timeout(program, args, Duration::from_millis(300))
            },
        }
    }
    pub fn enable(&mut self, enabled: bool, root: &Path) {
        if enabled && self.enabled {
            return;
        }
        self.enabled = enabled;
        if !enabled {
            self.interface.stop();
            self.connected = false;
            self.reset_signal();
        } else {
            self.reload(root);
        }
    }
    /// Stop the supplicant without disabling Home: `tick` brings it back once nothing holds
    /// the radio. For a link that needs the radio while Home has nothing associated.
    pub fn pause(&mut self) {
        if self.interface.wpa.is_some() {
            self.interface.stop();
            self.connected = false;
            self.reset_signal();
        }
    }
    pub fn reload(&mut self, root: &Path) {
        match config::read(root) {
            Ok(p) => {
                self.requested = false;
                if self.profiles != p && !self.profiles.is_empty() || p.is_empty() {
                    self.interface.stop();
                    self.connected = false;
                    self.reset_signal();
                }
                if self.profiles.is_empty() && self.interface.wpa.is_some() && !p.is_empty() {
                    let conf = fs::read_to_string(self.interface.run.join("wlan0.conf"))
                        .unwrap_or_default();
                    if let Some(network) = p.iter().find(|n| {
                        conf == config::supplicant(n, &self.interface.ctrl(), None, false)
                    }) {
                        self.active_ssid = network.ssid.clone();
                    } else {
                        self.interface.stop();
                        self.connected = false;
                        self.reset_signal();
                    }
                }
                self.profiles = p;
                self.next = 0;
                self.retry = Instant::now();
                self.error = "";
            }
            Err(e) => {
                self.error = e;
                eprintln!("slot-services: {e}");
            }
        }
    }
    pub fn connect(&mut self, root: &Path, ssid_hex: &str) -> Result<(), &'static str> {
        if !self.enabled {
            return Err("HOME_DISABLED");
        }
        let profiles = config::read(root)?;
        let profile = profiles
            .into_iter()
            .find(|n| config::hex(&n.ssid) == ssid_hex)
            .ok_or("NETWORK_NOT_SAVED")?;
        self.interface.stop();
        self.connected = false;
        self.active_ssid.clear();
        self.reset_signal();
        self.profiles = vec![profile];
        self.next = 0;
        self.requested = true;
        self.error = "";
        self.retry = Instant::now();
        Ok(())
    }

    fn reset_signal(&mut self) {
        self.signal.value = None;
        self.signal.refresh_at = Instant::now();
    }

    fn refresh_signal(&mut self, linked: bool) {
        let path = std::env::var_os("SLOT_PROC_WIRELESS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/proc/net/wireless"));
        self.signal.refresh_with(
            Instant::now(),
            linked,
            self.interface.name,
            &path,
            self.signal_command,
        );
    }

    pub fn status(&mut self) -> WifiStatus {
        self.status_with(|interface| {
            let supplicant = interface.status();
            let ip = connected(&supplicant).then(|| interface.ipv4()).flatten();
            (supplicant, ip)
        })
    }

    fn status_with(
        &mut self,
        observe: impl FnOnce(&Interface) -> (String, Option<std::net::Ipv4Addr>),
    ) -> WifiStatus {
        let (supplicant, ip) = if self.enabled {
            observe(&self.interface)
        } else {
            (String::new(), None)
        };
        let linked = self.enabled && connected(&supplicant);
        self.refresh_signal(linked);
        let phase = if self.enabled {
            connection_phase(
                &supplicant,
                ip.is_some(),
                self.interface.wpa.is_some() || self.requested,
                self.error,
            )
        } else {
            WifiPhase::Idle
        };
        WifiStatus {
            enabled: Some(self.enabled),
            ssid: linked.then(|| self.active_ssid.clone()),
            ip: ip.map(|ip| ip.to_string()),
            error: self.error.into(),
            phase: Some(phase),
            signal: linked.then_some(self.signal.value).flatten(),
        }
    }

    pub fn prepare_scan(
        &self,
        link_busy: bool,
        ours: &[u32],
        powered_down: &mut bool,
    ) -> Result<(), &'static str> {
        if !self.enabled {
            return Err("HOME_DISABLED");
        }
        if link_busy {
            return Err("LINK_BUSY");
        }
        if system::external_radio_owner(ours) {
            return Err("EXTERNAL_OWNER");
        }
        if !net_exists("wlan0") {
            return Err("RADIO_UNAVAILABLE");
        }
        prepare_scan_with(powered_down, output)
    }

    pub fn tick(
        &mut self,
        root: &Path,
        link_busy: bool,
        link_freq: Option<u32>,
        ours: &[u32],
        powered_down: &mut bool,
    ) {
        self.tick_with(
            root,
            link_busy,
            link_freq,
            (ours, powered_down),
            |interface| {
                interface.wpa.as_ref()?;
                let status = interface.status();
                let (dhcp, has_ip) = if connected(&status) {
                    (interface.dhcp(), interface.has_ip())
                } else {
                    (Ok(()), false)
                };
                Some(HomeLink {
                    status,
                    dhcp,
                    has_ip,
                })
            },
        );
    }

    fn tick_with(
        &mut self,
        root: &Path,
        link_busy: bool,
        link_freq: Option<u32>,
        radio: (&[u32], &mut bool),
        poll: impl FnOnce(&mut Interface) -> Option<HomeLink>,
    ) {
        let (ours, powered_down) = radio;
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if let Some(HomeLink {
            status,
            dhcp,
            has_ip,
        }) = poll(&mut self.interface)
        {
            if connected(&status) {
                // Also reap/restart a renewal worker that died after the first lease.
                if let Err(e) = dhcp {
                    self.error = e;
                }
                if has_ip {
                    self.connected = true;
                    self.error = "";
                    return;
                }
            } else {
                self.reset_signal();
                if self.connected {
                    self.deadline = now; // Lost a working association: select again, with backoff.
                }
            }
            if now < self.deadline {
                return;
            }
            let auth_log = auth_log_tail(&self.interface.run.join("wlan0.log"));
            self.error = connection_error(&status, &auth_log);
            self.interface.stop();
            self.connected = false;
            self.reset_signal();
            self.retry = now + Duration::from_secs(2);
        } else {
            self.reset_signal();
        }
        if now < self.retry {
            return;
        }
        // Conservatively defer new scans/associations for the whole Link session. Existing
        // home associations continue. No off-channel background scanning during a game.
        if link_busy || link_freq.is_some() {
            self.retry = now + Duration::from_secs(5);
            return;
        }
        if system::external_radio_owner(ours) {
            self.error = "EXTERNAL_OWNER";
            self.retry = now + Duration::from_secs(10);
            return;
        }
        if self.next >= self.profiles.len() {
            if self.requested {
                self.next = 0;
                self.retry = now + Duration::from_secs(15);
                return;
            }
            self.reload(root);
            self.retry = now + Duration::from_secs(15);
            if self.profiles.is_empty() {
                if self.error.is_empty() {
                    self.error = "NO_NETWORKS";
                }
                return;
            }
        }
        let Some(profile) = self.profiles.get(self.next) else {
            return;
        };
        // BaseOS initializes the radio asynchronously. Waiting for the interface
        // must not consume a profile and skip the preferred network at boot.
        if !net_exists("wlan0") {
            self.error = "RADIO_UNAVAILABLE";
            self.retry = now + Duration::from_secs(5);
            return;
        }
        *powered_down = false;
        if let Err(e) = output("rfkill", &["unblock", "wifi"]) {
            self.error = e;
            self.retry = now + Duration::from_secs(3);
            return;
        }
        match self.interface.start(profile, None, false) {
            Ok(()) => {
                self.active_ssid = profile.ssid.clone();
                self.next += 1;
                self.error = "";
                self.deadline = now + Duration::from_secs(25);
                self.reset_signal();
            }
            Err(e) => {
                self.error = e;
                self.retry = now + Duration::from_secs(3);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IW_LINK: &str = "Connected to 00:11:22:33:44:55 (on wlan0)\n\tSSID: Home\n\tfreq: 2412\n\tRX: 26010 bytes (158 packets)\n\tsignal: -54 dBm\n\ttx bitrate: 72.2 MBit/s MCS 7 short GI\n";
    const WIRELESS: &str = "Inter-| sta-|   Quality        |   Discarded packets               | Missed | WE\n face | tus | link level noise |  nwid  crypt   frag  retry   misc | beacon | 22\n wlan1: 0000   20.  -80.  -256        0      0      0      0      0        0\n wlan0: 0000   50.  -54.  -256        0      0      0      0      0        0\n";

    fn linked_observation(_: &Interface) -> (String, Option<std::net::Ipv4Addr>) {
        ("wpa_state=COMPLETED".into(), Some([192, 168, 1, 2].into()))
    }

    fn signal_fixture(program: &str, args: &[&str]) -> Result<String, &'static str> {
        assert_eq!(program, "iw");
        assert_eq!(args, ["dev", "wlan0", "link"]);
        Ok(IW_LINK.into())
    }

    #[test]
    fn linked_ticks_never_sample_signal_but_status_does() {
        let root = tempfile::tempdir().unwrap();
        let mut home = Home::new(root.path());
        home.enabled = true;
        home.signal_command = |_, _| panic!("tick must not sample signal");
        let mut powered_down = false;
        for has_ip in [false, true] {
            home.tick_with(root.path(), false, None, (&[], &mut powered_down), |_| {
                Some(HomeLink {
                    status: "wpa_state=COMPLETED".into(),
                    dhcp: Ok(()),
                    has_ip,
                })
            });
            assert_eq!(home.signal.value, None);
        }
        home.signal_command = signal_fixture;
        assert_eq!(
            home.status_with(linked_observation).signal,
            Some(WifiSignal::Dbm(-54))
        );
        home.signal_command = |_, _| panic!("status sample must be rate limited");
        assert_eq!(
            home.status_with(linked_observation).signal,
            Some(WifiSignal::Dbm(-54))
        );
    }

    #[test]
    fn a_signal_reset_clears_the_value_and_allows_an_immediate_status_sample() {
        let root = tempfile::tempdir().unwrap();
        let mut home = Home::new(root.path());
        home.enabled = true;
        home.signal_command = signal_fixture;
        home.status_with(linked_observation);
        let before_reset = Instant::now();
        assert!(home.signal.refresh_at > before_reset);
        home.reset_signal();
        assert_eq!(home.signal.value, None);
        assert!(home.signal.refresh_at >= before_reset);
        assert!(home.signal.refresh_at <= Instant::now());
        home.signal_command = |_, _| Ok(IW_LINK.replace("-54", "-65"));
        assert_eq!(
            home.status_with(linked_observation).signal,
            Some(WifiSignal::Dbm(-65))
        );
    }

    #[test]
    fn reloading_changed_profiles_resets_the_signal_before_the_next_status() {
        let root = tempfile::tempdir().unwrap();
        let mut home = Home::new(root.path());
        home.enabled = true;
        home.profiles = vec![Network {
            ssid: "Old".into(),
            password: None,
        }];
        slot_store::write_wifi(
            root.path(),
            &[Network {
                ssid: "New".into(),
                password: None,
            }],
        )
        .unwrap();
        home.signal_command = signal_fixture;
        home.status_with(linked_observation);
        assert_eq!(home.signal.value, Some(WifiSignal::Dbm(-54)));
        home.reload(root.path());
        assert_eq!(home.signal.value, None);
        assert!(home.signal.refresh_at <= Instant::now());
        home.signal_command = |_, _| Ok(IW_LINK.replace("-54", "-65"));
        assert_eq!(
            home.status_with(linked_observation).signal,
            Some(WifiSignal::Dbm(-65))
        );
    }

    #[test]
    fn a_tick_with_a_down_link_clears_signal_without_sampling() {
        let root = tempfile::tempdir().unwrap();
        let mut home = Home::new(root.path());
        home.enabled = true;
        home.signal.value = Some(WifiSignal::Dbm(-54));
        home.signal.refresh_at = Instant::now() + Duration::from_secs(3);
        home.signal_command = |_, _| panic!("down link must not sample signal");
        let mut powered_down = false;
        home.tick_with(root.path(), false, None, (&[], &mut powered_down), |_| {
            Some(HomeLink {
                status: "wpa_state=SCANNING".into(),
                dhcp: Ok(()),
                has_ip: false,
            })
        });
        assert_eq!(home.signal.value, None);
        assert!(home.signal.refresh_at <= Instant::now());
    }

    #[test]
    fn phases_follow_supplicant_lease_and_service_errors() {
        for state in ["SCANNING", "ASSOCIATING", "ASSOCIATED"] {
            assert_eq!(
                connection_phase(&format!("wpa_state={state}"), false, true, ""),
                WifiPhase::Associating
            );
        }
        for state in ["AUTHENTICATING", "4WAY_HANDSHAKE", "GROUP_HANDSHAKE"] {
            assert_eq!(
                connection_phase(&format!("wpa_state={state}"), false, true, ""),
                WifiPhase::Authenticating
            );
        }
        for state in [
            "DISCONNECTED",
            "INACTIVE",
            "INTERFACE_DISABLED",
            "UNKNOWN",
            "",
        ] {
            let status = format!("wpa_state={state}");
            assert_eq!(connection_phase(&status, false, false, ""), WifiPhase::Idle);
            assert_eq!(
                connection_phase(&status, false, true, ""),
                WifiPhase::Associating
            );
        }
        assert_eq!(
            connection_phase("wpa_state=COMPLETED", false, true, ""),
            WifiPhase::ObtainingAddress
        );
        assert_eq!(
            connection_phase("wpa_state=COMPLETED", true, true, ""),
            WifiPhase::Connected
        );
        assert_eq!(
            connection_phase("wpa_state=COMPLETED", true, true, "CONFIG_PARSE"),
            WifiPhase::Connected
        );
        assert_eq!(
            connection_phase("", false, false, "NO_NETWORKS"),
            WifiPhase::Idle
        );
        for (error, reason) in [
            ("WRONG_PASSWORD", WifiFailure::WrongPassword),
            ("NETWORK_NOT_FOUND", WifiFailure::NetworkNotFound),
            ("NO_DHCP_LEASE", WifiFailure::NoAddress),
            ("HOME_CONNECT_FAILED", WifiFailure::Other),
            ("RADIO_UNAVAILABLE", WifiFailure::Other),
            ("COMMAND_FAILED", WifiFailure::Other),
        ] {
            assert_eq!(
                connection_phase("", false, false, error),
                WifiPhase::Failed(reason)
            );
        }
    }

    #[test]
    fn link_signal_parsing_is_bounded_and_tolerates_missing_output() {
        assert_eq!(iw_signal(IW_LINK), Some(-54));
        for text in [
            "Not connected.",
            "garbage",
            "",
            "signal: -54 dBm",
            "Connected to aa\n signal: NaN dBm",
            "Connected to aa\n signal: -999 dBm",
            "Connected to aa\n signal: -54 unknown",
        ] {
            assert_eq!(iw_signal(text), None, "{text}");
        }
        assert_eq!(wireless_quality(WIRELESS, "wlan0"), Some(71));
        assert_eq!(wireless_quality(WIRELESS, "wlan1"), Some(28));
        assert_eq!(wireless_quality(WIRELESS, "wlan2"), None);
        assert_eq!(wireless_quality("wlan0: 0000 garbage", "wlan0"), None);
        assert_eq!(wireless_quality("wlan0: 0000 100.", "wlan0"), Some(100));
    }

    #[test]
    fn signal_reader_uses_injected_commands_and_proc_fixture() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wireless");
        fs::write(&path, WIRELESS).unwrap();
        assert_eq!(
            read_signal_with("wlan0", &path, |program, args| {
                assert_eq!(program, "iw");
                assert_eq!(args, ["dev", "wlan0", "link"]);
                Ok(IW_LINK.into())
            }),
            Some(WifiSignal::Dbm(-54))
        );
        for result in [Ok("garbage".into()), Err("TOOL_UNAVAILABLE")] {
            assert_eq!(
                read_signal_with("wlan0", &path, |_, _| result.clone()),
                Some(WifiSignal::Quality(71))
            );
        }
        assert_eq!(
            read_signal_with("wlan0", &path, |_, _| Ok("Not connected.\n".into())),
            None
        );
        fs::write(&path, "garbage").unwrap();
        assert_eq!(
            read_signal_with("wlan0", &path, |_, _| Err("COMMAND_FAILED")),
            None
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            read_signal_with("wlan0", &path, |_, _| Err("COMMAND_FAILED")),
            None
        );
    }

    #[test]
    fn signal_refresh_is_limited_to_three_seconds_and_only_an_up_link() {
        let mut cache = SignalCache::new();
        let now = cache.refresh_at;
        let path = Path::new("/missing-fixture");
        cache.refresh_with(now, false, "wlan0", path, |_, _| {
            panic!("disconnected poll")
        });
        cache.refresh_with(now, true, "wlan0", path, |_, _| Ok(IW_LINK.into()));
        assert_eq!(cache.value, Some(WifiSignal::Dbm(-54)));
        cache.refresh_with(
            now + Duration::from_millis(2999),
            true,
            "wlan0",
            path,
            |_, _| panic!("early poll"),
        );
        cache.refresh_with(now + Duration::from_secs(3), true, "wlan0", path, |_, _| {
            Ok(IW_LINK.replace("-54", "-65"))
        });
        assert_eq!(cache.value, Some(WifiSignal::Dbm(-65)));
        cache.refresh_with(
            now + Duration::from_secs(4),
            false,
            "wlan0",
            path,
            |_, _| panic!("disconnected poll"),
        );
        assert_eq!(cache.value, None);
        cache.refresh_with(now + Duration::from_secs(4), true, "wlan0", path, |_, _| {
            panic!("reconnect early poll")
        });
        cache.refresh_with(now + Duration::from_secs(6), true, "wlan0", path, |_, _| {
            Ok("garbage".into())
        });
        assert_eq!(cache.value, None);
    }
    #[test]
    fn scan_preparation_clears_power_down_even_when_a_command_fails() {
        for fail_at in [0, 1, 2] {
            let mut powered_down = true;
            let mut calls = Vec::new();
            let result = prepare_scan_with(&mut powered_down, |program, _| {
                calls.push(program.to_owned());
                if calls.len() == fail_at {
                    Err("COMMAND_FAILED")
                } else {
                    Ok(String::new())
                }
            });
            assert!(!powered_down);
            assert_eq!(result.is_err(), fail_at != 0);
            assert_eq!(calls[0], "rfkill");
            if fail_at != 1 {
                assert_eq!(calls[1], "ip");
            }
        }
    }

    #[test]
    fn file_logging_requires_the_compiled_help_option() {
        let without = "usage:\n  wpa_supplicant [-BddhKLqqtvW] [-P<pid file>] \\\n        -i<ifname> -c<config file> [-D<driver>] [-e<entropy file>]\noptions:\n  -e = entropy file\n  -g = global ctrl_interface\n  -h = show this help text\n";
        let with = without.replace("[-e<entropy file>]", "[-e<entropy file>] [-f<debug file>]");
        assert!(!supplicant_file_logging(without));
        assert!(supplicant_file_logging(&with));
        assert!(supplicant_file_logging(
            "options:\n  -f = log output to debug file instead of stdout\n"
        ));
        assert!(!supplicant_file_logging(""));
        assert!(!supplicant_file_logging("  -freq = frequency\n"));
    }

    fn busy_error() -> system::CommandError {
        system::CommandError {
            code: "COMMAND_FAILED",
            stderr: "command failed: Device or resource busy (-16)\n".into(),
        }
    }

    #[test]
    fn a_busy_scan_retries_once_and_uses_cached_results() {
        let mut calls = Vec::new();
        let scan = scan_with(
            Instant::now() + Duration::from_secs(10),
            |cached, timeout| {
                calls.push(cached);
                assert!(timeout <= Duration::from_secs(10));
                if cached {
                    Ok("BSS cached\n".into())
                } else {
                    Err(busy_error())
                }
            },
        );
        assert_eq!(scan, Ok("BSS cached\n".into()));
        assert_eq!(calls, [false, false, true]);
    }

    #[test]
    fn a_busy_scan_can_succeed_on_its_retry() {
        let mut calls = Vec::new();
        let scan = scan_with(Instant::now() + Duration::from_secs(10), |cached, _| {
            calls.push(cached);
            if calls.len() == 1 {
                Err(busy_error())
            } else {
                Ok("BSS fresh\n".into())
            }
        });
        assert_eq!(scan, Ok("BSS fresh\n".into()));
        assert_eq!(calls, [false, false]);
    }

    #[test]
    fn scan_failures_are_reported_after_the_cache_also_fails() {
        let mut calls = Vec::new();
        assert_eq!(
            scan_with(Instant::now() + Duration::from_secs(10), |cached, _| {
                calls.push(cached);
                Err(busy_error())
            }),
            Err("COMMAND_FAILED")
        );
        assert_eq!(calls, [false, false, true]);
        assert_eq!(
            scan_with(Instant::now() + Duration::from_secs(10), |_, _| Err(
                system::CommandError {
                    code: "COMMAND_FAILED",
                    stderr: "command failed: Operation not permitted (-1)\n".into(),
                }
            )),
            Err("COMMAND_FAILED")
        );
    }
    #[test]
    fn routes_conflict_without_mistaking_default_or_our_interface() {
        assert!(subnet_conflict("10.0.0.0/8 dev wlan0"));
        assert!(subnet_conflict("10.42.0.2 dev usb0"));
        assert!(!subnet_conflict(
            "default via 192.168.1.1 dev wlan0\n192.168.1.0/24 dev wlan0\n10.42.0.0/24 dev wlan1"
        ));
    }
    #[test]
    fn frequency_falls_back_to_the_supplicant_only_once_connected() {
        assert_eq!(
            supplicant_frequency("wpa_state=COMPLETED\nfreq=5745\nmode=AP"),
            Some(5745)
        );
        assert_eq!(supplicant_frequency("wpa_state=SCANNING\nfreq=5745"), None);
        assert_eq!(supplicant_frequency("wpa_state=COMPLETED\nfreq=0"), None);
        assert_eq!(supplicant_frequency("wpa_state=COMPLETED"), None);
        assert_eq!(supplicant_frequency(""), None);
    }
    #[test]
    fn the_first_inet_address_is_read_without_its_prefix() {
        assert_eq!(
            first_ipv4("9: wlan0    inet 192.168.34.81/24 brd 192.168.34.255 scope global wlan0"),
            Some(std::net::Ipv4Addr::new(192, 168, 34, 81))
        );
        assert_eq!(first_ipv4("9: wlan0    inet6 fe80::1/64 scope link"), None);
        assert_eq!(first_ipv4("9: wlan0    inet garbage/24"), None);
        assert_eq!(first_ipv4(""), None);
    }
    #[test]
    fn capabilities_fail_closed() {
        assert!(!permitted(
            "* 5580 MHz [116] (20.0 dBm) (radar detection)",
            5580
        ));
        assert!(!permitted("* 5745 MHz [149] (disabled)", 5745));
        assert!(permitted("* 2412 MHz [1] (20.0 dBm)", 2412));
    }
}

fn auth_log_tail(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or_default();
    if file
        .seek(SeekFrom::Start(len.saturating_sub(65536)))
        .is_err()
    {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.take(65536).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// An association without a lease differs from a rejected WPA key.
pub fn connection_error(status: &str, log: &str) -> &'static str {
    if connected(status) {
        "NO_DHCP_LEASE"
    } else if log.contains("reason=WRONG_KEY") || log.contains("pre-shared key may be incorrect") {
        "WRONG_PASSWORD"
    } else if field(status, "wpa_state") == Some("SCANNING")
        || log.contains("CTRL-EVENT-NETWORK-NOT-FOUND")
    {
        "NETWORK_NOT_FOUND"
    } else {
        "HOME_CONNECT_FAILED"
    }
}

#[cfg(test)]
mod home_error_tests {
    use super::*;
    #[test]
    fn choosing_a_saved_network_targets_it_and_reload_restores_all_profiles() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        fs::create_dir(&run).unwrap();
        let profiles = vec![
            Network {
                ssid: "Home".into(),
                password: Some("password".into()),
            },
            Network {
                ssid: "Guest \"".into(),
                password: None,
            },
        ];
        slot_store::write_wifi(root.path(), &profiles).unwrap();
        let mut home = Home::new(&run);
        assert_eq!(
            home.connect(root.path(), &config::hex("Home")),
            Err("HOME_DISABLED")
        );
        home.enabled = true;
        home.connect(root.path(), &config::hex("Guest \"")).unwrap();
        assert_eq!(home.profiles, vec![profiles[1].clone()]);
        assert!(home.requested);
        assert_eq!(
            home.connect(root.path(), &config::hex("Missing")),
            Err("NETWORK_NOT_SAVED")
        );
        fs::write(root.path().join("Config/wifi.toml"), "broken='").unwrap();
        home.reload(root.path());
        assert!(home.requested);
        assert_eq!(home.profiles, vec![profiles[1].clone()]);
        slot_store::write_wifi(root.path(), &profiles).unwrap();
        home.reload(root.path());
        assert!(!home.requested);
        assert_eq!(home.profiles, profiles);
    }

    #[test]
    fn authentication_log_reads_only_a_bounded_tail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("wlan0.log");
        fs::write(&path, format!("{}reason=WRONG_KEY", "x".repeat(100000))).unwrap();
        let log = auth_log_tail(&path);
        assert_eq!(log.len(), 65536);
        assert!(log.ends_with("reason=WRONG_KEY"));
    }

    #[test]
    fn reports_authentication_and_dhcp_failures_separately() {
        assert_eq!(
            connection_error("wpa_state=COMPLETED\n", ""),
            "NO_DHCP_LEASE"
        );
        assert_eq!(
            connection_error(
                "wpa_state=DISCONNECTED\n",
                "CTRL-EVENT-SSID-TEMP-DISABLED reason=WRONG_KEY"
            ),
            "WRONG_PASSWORD"
        );
        assert_eq!(
            connection_error("wpa_state=SCANNING\n", ""),
            "NETWORK_NOT_FOUND"
        );
        assert_eq!(
            connection_error("wpa_state=DISCONNECTED\n", ""),
            "HOME_CONNECT_FAILED"
        );
        assert_eq!(
            connection_error("wpa_state=DISCONNECTED\n", "CTRL-EVENT-NETWORK-NOT-FOUND"),
            "NETWORK_NOT_FOUND"
        );
    }
}
