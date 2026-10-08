use crate::config::{self, Network};
use crate::system::{self, field, output, Process};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
        self.wpa = Some(Process::spawn(
            "wpa_supplicant",
            &[
                "-i",
                self.name,
                "-c",
                conf.to_str().unwrap(),
                "-Dnl80211",
                "-f",
                log.to_str().unwrap(),
            ],
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
        }
    }
    pub fn reload(&mut self, root: &Path) {
        match config::read(root) {
            Ok(p) => {
                self.requested = false;
                if self.profiles != p && !self.profiles.is_empty() || p.is_empty() {
                    self.interface.stop();
                    self.connected = false;
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
        self.profiles = vec![profile];
        self.next = 0;
        self.requested = true;
        self.error = "";
        self.retry = Instant::now();
        Ok(())
    }

    pub fn scan(&mut self, link_busy: bool, ours: &[u32]) -> Result<String, &'static str> {
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
        output("rfkill", &["unblock", "wifi"])?;
        output("ip", &["link", "set", "wlan0", "up"])?;
        system::output_timeout("iw", &["dev", "wlan0", "scan"], Duration::from_secs(10))
    }

    pub fn tick(&mut self, root: &Path, link_busy: bool, link_freq: Option<u32>, ours: &[u32]) {
        if !self.enabled {
            return;
        }
        let now = Instant::now();
        if self.interface.wpa.is_some() {
            let status = self.interface.status();
            if connected(&status) {
                // Also reap/restart a renewal worker that died after the first lease.
                if let Err(e) = self.interface.dhcp() {
                    self.error = e;
                }
                if self.interface.has_ip() {
                    self.connected = true;
                    self.error = "";
                    return;
                }
            } else if self.connected {
                self.deadline = now; // Lost a working association: select again, with backoff.
            }
            if now < self.deadline {
                return;
            }
            let auth_log = auth_log_tail(&self.interface.run.join("wlan0.log"));
            self.error = connection_error(&status, &auth_log);
            self.interface.stop();
            self.connected = false;
            self.retry = now + Duration::from_secs(2);
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
            "HOME_CONNECT_FAILED"
        );
    }
}
