//! Backward-tolerant Home Wi-Fi status protocol shared by the service and frontend.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiFailure {
    WrongPassword,
    NetworkNotFound,
    NoAddress,
    Other,
}

impl WifiFailure {
    fn wire(self) -> &'static str {
        match self {
            Self::WrongPassword => "WRONG_PASSWORD",
            Self::NetworkNotFound => "NETWORK_NOT_FOUND",
            Self::NoAddress => "NO_DHCP_LEASE",
            Self::Other => "HOME_CONNECT_FAILED",
        }
    }

    pub fn from_error(error: &str) -> Self {
        match error {
            "WRONG_PASSWORD" => Self::WrongPassword,
            "NETWORK_NOT_FOUND" => Self::NetworkNotFound,
            "NO_DHCP_LEASE" => Self::NoAddress,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WifiPhase {
    #[default]
    Idle,
    Associating,
    Authenticating,
    ObtainingAddress,
    Connected,
    Failed(WifiFailure),
}

impl WifiPhase {
    fn wire(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Associating => "ASSOCIATING",
            Self::Authenticating => "AUTHENTICATING",
            Self::ObtainingAddress => "OBTAINING_ADDRESS",
            Self::Connected => "CONNECTED",
            Self::Failed(_) => "FAILED",
        }
    }

    fn parse(value: &str, error: &str) -> Option<Self> {
        Some(match value {
            "IDLE" => Self::Idle,
            "ASSOCIATING" => Self::Associating,
            "AUTHENTICATING" => Self::Authenticating,
            "OBTAINING_ADDRESS" => Self::ObtainingAddress,
            "CONNECTED" => Self::Connected,
            "FAILED" => Self::Failed(WifiFailure::from_error(error)),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiSignal {
    Dbm(i32),
    Quality(u8),
}

impl std::fmt::Display for WifiSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dbm(value) => write!(f, "{value} dBm"),
            Self::Quality(value) => write!(f, "{value}%"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiStatus {
    pub enabled: Option<bool>,
    pub ssid: Option<String>,
    pub ip: Option<String>,
    pub error: String,
    pub phase: Option<WifiPhase>,
    pub signal: Option<WifiSignal>,
}

impl WifiStatus {
    pub fn parse(text: &str) -> Self {
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
        let error = value("home_error=").unwrap_or("");
        let signal = value("home_signal_dbm=")
            .and_then(|s| s.parse().ok())
            .filter(|s| (-127..=0).contains(s))
            .map(WifiSignal::Dbm)
            .or_else(|| {
                value("home_signal_quality=")
                    .and_then(|s| s.parse().ok())
                    .filter(|s| *s <= 100)
                    .map(WifiSignal::Quality)
            });
        Self {
            enabled: value("home_enabled=").and_then(|s| s.parse().ok()),
            ssid,
            ip: value("home_ip=")
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            error: error.into(),
            phase: value("home_phase=")
                .and_then(|s| WifiPhase::parse(s, value("home_failure=").unwrap_or(error))),
            signal,
        }
    }

    pub fn to_wire(&self) -> String {
        let ssid: String = self
            .ssid
            .as_deref()
            .unwrap_or("")
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mut text = format!(
            "home_enabled={} home_connected={} home_error={} home_ssid={} home_ip={}",
            self.enabled.map(|v| v.to_string()).unwrap_or_default(),
            self.enabled == Some(true) && self.ssid.is_some() && self.ip.is_some(),
            self.error,
            ssid,
            self.ip.as_deref().unwrap_or("")
        );
        if let Some(phase) = self.phase {
            text.push_str(&format!(" home_phase={}", phase.wire()));
            if let WifiPhase::Failed(reason) = phase {
                text.push_str(&format!(" home_failure={}", reason.wire()));
            }
        }
        match self.signal {
            Some(WifiSignal::Dbm(value)) => text.push_str(&format!(" home_signal_dbm={value}")),
            Some(WifiSignal::Quality(value)) => {
                text.push_str(&format!(" home_signal_quality={value}"))
            }
            None => {}
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_with_and_without_optional_fields() {
        let legacy = WifiStatus {
            enabled: Some(true),
            ssid: Some("Home café".into()),
            ip: Some("192.168.1.2".into()),
            ..Default::default()
        };
        assert_eq!(WifiStatus::parse(&legacy.to_wire()), legacy);
        for (phase, error) in [
            (WifiPhase::Idle, ""),
            (WifiPhase::Associating, ""),
            (WifiPhase::Authenticating, ""),
            (WifiPhase::ObtainingAddress, ""),
            (WifiPhase::Connected, ""),
            (
                WifiPhase::Failed(WifiFailure::WrongPassword),
                "WRONG_PASSWORD",
            ),
            (
                WifiPhase::Failed(WifiFailure::NetworkNotFound),
                "NETWORK_NOT_FOUND",
            ),
            (WifiPhase::Failed(WifiFailure::NoAddress), "NO_DHCP_LEASE"),
            (WifiPhase::Failed(WifiFailure::Other), "COMMAND_FAILED"),
        ] {
            for signal in [
                None,
                Some(WifiSignal::Dbm(-54)),
                Some(WifiSignal::Quality(71)),
            ] {
                let status = WifiStatus {
                    phase: Some(phase),
                    error: error.into(),
                    signal,
                    ..legacy.clone()
                };
                assert_eq!(
                    WifiStatus::parse(&format!("0 {} link=idle", status.to_wire())),
                    status
                );
            }
        }
        let status = WifiStatus::parse(
            "home_phase=FUTURE home_signal_dbm=garbage home_signal_quality=101 home_ssid=éé",
        );
        assert_eq!(status, WifiStatus::default());
        let failed = WifiStatus {
            phase: Some(WifiPhase::Failed(WifiFailure::WrongPassword)),
            ..Default::default()
        };
        assert_eq!(WifiStatus::parse(&failed.to_wire()), failed);
    }
}
