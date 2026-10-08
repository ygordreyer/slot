//! Supplicant configuration derived from shared saved profiles.

#[cfg(test)]
use slot_store::parse_wifi as parse;
pub use slot_store::{read_wifi as read, WifiNetwork as Network};
use std::path::Path;

pub fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

pub fn supplicant(n: &Network, ctrl: &Path, freq: Option<u32>, ap: bool) -> String {
    let mut s = format!(
        "ctrl_interface={}\nupdate_config=0\nnetwork={{\nssid={}\nscan_ssid=1\n",
        ctrl.display(),
        hex(&n.ssid)
    );
    match &n.password {
        Some(p) => {
            // A raw PSK avoids supplicant's passphrase quoting ambiguities. Passwords
            // containing quotes/backslashes retain their exact TOML-decoded bytes.
            let mut key = [0u8; 32];
            pbkdf2::pbkdf2_hmac::<sha1::Sha1>(p.as_bytes(), n.ssid.as_bytes(), 4096, &mut key);
            let psk: String = key.iter().map(|b| format!("{b:02x}")).collect();
            s.push_str(&format!(
                "key_mgmt=WPA-PSK\nproto=RSN\npairwise=CCMP\npsk={psk}\n"
            ));
        }
        None => s.push_str("key_mgmt=NONE\n"),
    }
    if ap {
        s.push_str("mode=2\n");
    }
    if let Some(f) = freq {
        if ap {
            s.push_str(&format!("frequency={f}\n"));
        } else {
            s.push_str(&format!("scan_freq={f}\nfreq_list={f}\n"));
        }
    }
    s.push_str("}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordered_profiles_and_injection_safe_serialization() {
        let n = parse("[[networks]]\nssid='Home\" }'\npassword='quotes\"\\here'\n[[networks]]\nssid='Guest'\nsecurity='open'\n").unwrap();
        assert_eq!(n[0].ssid, "Home\" }");
        assert!(n[1].password.is_none());
        let conf = supplicant(&n[0], Path::new("/run/private"), Some(2412), false);
        assert!(conf.contains("ssid=486f6d6522207d\n"));
        assert!(!conf.contains("quotes"));
        assert_eq!(
            conf.lines().find(|s| s.starts_with("psk=")).unwrap().len(),
            68
        );
        assert!(conf.contains("freq_list=2412"));
    }
    #[test]
    fn wpa_psk_matches_known_vector() {
        let n = Network {
            ssid: "IEEE".into(),
            password: Some("password".into()),
        };
        let conf = supplicant(&n, Path::new("/run/test"), None, false);
        assert!(
            conf.contains("psk=f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
        );
    }
    #[test]
    fn errors_do_not_contain_credentials_and_never_downgrade_to_open() {
        for s in [
            "[[networks]]\nssid='Home'",
            "[[networks]]\nssid='Home'\npassword='secret'",
            "password='topsecret",
            "networks=1",
        ] {
            let e = parse(s).err().unwrap();
            assert!(e.starts_with("CONFIG_"));
            assert!(!e.contains("secret"));
        }
        assert!(parse("").unwrap().is_empty());
    }
}
