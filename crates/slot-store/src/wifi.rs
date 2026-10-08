//! Home Wi-Fi profiles shared by the frontend and service.

use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNetwork {
    pub ssid: String,
    pub password: Option<String>,
}

// Never format TOML errors: they include the source line, potentially a password.
pub fn read_wifi(root: &Path) -> Result<Vec<WifiNetwork>, &'static str> {
    let path = root.join("Config/wifi.toml");
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("CONFIG_UNREADABLE"),
    };
    let mut text = String::new();
    file.take(65537)
        .read_to_string(&mut text)
        .map_err(|_| "CONFIG_UNREADABLE")?;
    if text.len() > 65536 {
        return Err("CONFIG_TOO_LARGE");
    }
    parse_wifi(&text)
}

pub fn parse_wifi(text: &str) -> Result<Vec<WifiNetwork>, &'static str> {
    let doc = text.parse::<toml::Table>().map_err(|_| "CONFIG_SYNTAX")?;
    if doc.keys().any(|k| k != "networks") {
        return Err("CONFIG_UNKNOWN_FIELD");
    }
    let Some(value) = doc.get("networks") else {
        return Ok(Vec::new());
    };
    let entries = value.as_array().ok_or("CONFIG_NETWORKS")?;
    if entries.len() > WIFI_LIMIT {
        return Err("CONFIG_TOO_MANY_NETWORKS");
    }
    entries
        .iter()
        .map(|entry| {
            let t = entry.as_table().ok_or("CONFIG_NETWORK")?;
            if t.keys()
                .any(|k| !["ssid", "password", "security"].contains(&k.as_str()))
            {
                return Err("CONFIG_UNKNOWN_FIELD");
            }
            let ssid = t
                .get("ssid")
                .and_then(|v| v.as_str())
                .ok_or("CONFIG_SSID")?;
            if ssid.is_empty() || ssid.len() > 32 || ssid.contains('\0') {
                return Err("CONFIG_SSID");
            }
            let security = match t.get("security") {
                None => "wpa-psk",
                Some(v) => v.as_str().ok_or("CONFIG_SECURITY")?,
            };
            let password = match security {
                "open" if !t.contains_key("password") => None,
                "wpa-psk" => {
                    let p = t
                        .get("password")
                        .and_then(|v| v.as_str())
                        .ok_or("CONFIG_PASSWORD")?;
                    if !(8..=63).contains(&p.len()) || !p.bytes().all(|b| (32..=126).contains(&b)) {
                        return Err("CONFIG_PASSWORD");
                    }
                    Some(p.to_owned())
                }
                _ => return Err("CONFIG_SECURITY"),
            };
            Ok(WifiNetwork {
                ssid: ssid.into(),
                password,
            })
        })
        .collect()
}

pub const WIFI_LIMIT: usize = 32;

pub fn write_wifi(root: &Path, networks: &[WifiNetwork]) -> Result<(), &'static str> {
    let entries: Vec<toml::Value> = networks
        .iter()
        .map(|network| {
            let mut entry = toml::Table::new();
            entry.insert("ssid".into(), network.ssid.clone().into());
            if let Some(password) = &network.password {
                entry.insert("password".into(), password.clone().into());
            } else {
                entry.insert("security".into(), "open".into());
            }
            toml::Value::Table(entry)
        })
        .collect();
    let mut doc = toml::Table::new();
    doc.insert("networks".into(), entries.into());
    let text = toml::to_string(&doc).map_err(|_| "CONFIG_SERIALIZE")?;
    parse_wifi(&text)?;
    std::fs::create_dir_all(root.join("Config")).map_err(|_| "CONFIG_WRITE")?;
    crate::atomic::atomic_write_private(&root.join("Config/wifi.toml"), text.as_bytes())
        .map_err(|_| "CONFIG_WRITE")
}

/// Promote the chosen profile without losing any other saved network.
pub fn save_wifi(root: &Path, network: WifiNetwork) -> Result<(), &'static str> {
    let mut networks = read_wifi(root)?;
    networks.retain(|n| n.ssid != network.ssid);
    networks.insert(0, network);
    write_wifi(root, &networks)
}

pub fn forget_wifi(root: &Path, ssid: &str) -> Result<(), &'static str> {
    let mut networks = read_wifi(root)?;
    networks.retain(|n| n.ssid != ssid);
    write_wifi(root, &networks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_merge_preserve_password_bytes_and_existing_networks() {
        let root = tempfile::tempdir().unwrap();
        let home = WifiNetwork {
            ssid: "Home \"雪".into(),
            password: Some(" Ab\\c\"!#[]{}".into()),
        };
        let guest = WifiNetwork {
            ssid: "Guest".into(),
            password: None,
        };
        save_wifi(root.path(), home.clone()).unwrap();
        save_wifi(root.path(), guest.clone()).unwrap();
        assert_eq!(
            read_wifi(root.path()).unwrap(),
            vec![guest.clone(), home.clone()]
        );
        save_wifi(root.path(), home.clone()).unwrap();
        assert_eq!(read_wifi(root.path()).unwrap(), vec![home, guest]);
        forget_wifi(root.path(), "Guest").unwrap();
        assert_eq!(read_wifi(root.path()).unwrap().len(), 1);
        assert_eq!(
            std::fs::read_dir(root.path().join("Config"))
                .unwrap()
                .count(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join("Config/wifi.toml"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_or_full_config_is_not_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let networks: Vec<_> = (0..WIFI_LIMIT)
            .map(|i| WifiNetwork {
                ssid: i.to_string(),
                password: None,
            })
            .collect();
        write_wifi(root.path(), &networks).unwrap();
        assert_eq!(
            save_wifi(
                root.path(),
                WifiNetwork {
                    ssid: "extra".into(),
                    password: None
                }
            ),
            Err("CONFIG_TOO_MANY_NETWORKS")
        );
        assert_eq!(read_wifi(root.path()).unwrap(), networks);
        assert_eq!(
            save_wifi(
                root.path(),
                WifiNetwork {
                    ssid: "0".into(),
                    password: Some("short".into())
                }
            ),
            Err("CONFIG_PASSWORD")
        );
        assert_eq!(read_wifi(root.path()).unwrap(), networks);
        std::fs::write(root.path().join("Config/wifi.toml"), "broken='").unwrap();
        assert_eq!(
            save_wifi(
                root.path(),
                WifiNetwork {
                    ssid: "valid".into(),
                    password: None
                }
            ),
            Err("CONFIG_SYNTAX")
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("Config/wifi.toml")).unwrap(),
            "broken='"
        );
    }
}
