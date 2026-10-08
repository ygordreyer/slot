//! Raster view of the home network list, independent of radio and storage work.

use crate::keyboard::{panel_blit, panel_face, panel_legend, panel_rect, panel_text};
use crate::plate::UndoFace;
use crate::power_menu::MENU_PX;
use slot_store::wifi_status::{WifiFailure, WifiPhase, WifiStatus};

pub const WIFI_ROWS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiMenuRow {
    pub ssid: String,
    pub signal: Option<i32>,
    pub secured: bool,
    pub saved: bool,
    pub connected: bool,
}

pub struct WifiMenu<'a> {
    pub rows: &'a [WifiMenuRow],
    pub selected: usize,
    pub status: &'a str,
    pub enabled: bool,
    pub forget: Option<&'a str>,
}

impl WifiMenu<'_> {
    pub fn status_text(status: &WifiStatus) -> Option<String> {
        let phase = status.phase.or_else(|| {
            (status.ssid.is_some() && status.ip.is_some()).then_some(WifiPhase::Connected)
        })?;
        Some(match phase {
            WifiPhase::Idle => return None,
            WifiPhase::Associating => "Associating...".into(),
            WifiPhase::Authenticating => "Authenticating...".into(),
            WifiPhase::ObtainingAddress => "Obtaining an address...".into(),
            WifiPhase::Failed(reason) => match reason {
                WifiFailure::WrongPassword => "Wrong password",
                WifiFailure::NetworkNotFound => "Network not found",
                WifiFailure::NoAddress => "No DHCP lease",
                WifiFailure::Other => "Could not connect to network",
            }
            .into(),
            WifiPhase::Connected => match (status.signal, &status.ip, &status.ssid) {
                (Some(signal), Some(ip), _) => format!("Connected: {signal} ({ip})"),
                (Some(signal), _, _) => format!("Connected: {signal}"),
                (None, Some(ip), Some(ssid)) => format!("Connected: {ip} ({ssid})"),
                _ => "Connected".into(),
            },
        })
    }

    pub fn face(&self) -> UndoFace {
        let mut face = panel_face();
        panel_text(&mut face, "Wi-Fi Networks", 24, 10, 672, MENU_PX);
        panel_text(&mut face, self.status, 24, 54, 672, 22.0);
        if let Some(ssid) = self.forget {
            panel_text(&mut face, "Forget saved network?", 24, 164, 672, MENU_PX);
            panel_text(&mut face, ssid, 24, 218, 672, MENU_PX);
            panel_legend(&mut face, &[("B", "Cancel"), ("A", "Forget")], 427);
            return face;
        }
        if !self.enabled {
            panel_text(&mut face, "Home Wi-Fi is Off", 24, 180, 672, MENU_PX);
            panel_legend(&mut face, &[("B", "Back"), ("A", "Turn On")], 427);
            return face;
        }
        let top = self
            .selected
            .saturating_sub(WIFI_ROWS / 2)
            .min(self.rows.len().saturating_sub(WIFI_ROWS));
        for (slot, title) in self.rows.iter().skip(top).take(WIFI_ROWS).enumerate() {
            let y = 103 + slot as u32 * 49;
            if top + slot == self.selected {
                panel_rect(&mut face, 0, y, 720, 44, [0x4d, 0x4d, 0x57, 255]);
            }
            let mark = format!(
                "{}{}",
                if title.connected { "+" } else { " " },
                if title.saved { "*" } else { " " }
            );
            panel_text(&mut face, &mark, 16, y + 2, 38, MENU_PX);
            panel_text(&mut face, &title.ssid, 62, y + 2, 438, MENU_PX);
            if title.secured {
                lock(&mut face, 516, y + 10);
            }
            if let Some(signal) = title.signal {
                panel_text(&mut face, &format!("{signal} dBm"), 554, y + 2, 142, 26.0);
            } else if title.saved {
                panel_text(&mut face, "Away", 554, y + 2, 142, 26.0);
            }
        }
        panel_text(&mut face, "* Saved   + Connected", 24, 392, 672, 18.0);
        panel_legend(
            &mut face,
            &[("B", "Back"), ("A", "Join"), ("X", "Forget"), ("Y", "Scan")],
            437,
        );
        face
    }
}

fn lock(face: &mut UndoFace, x: u32, y: u32) {
    let Some(font) = crate::icon::symbols_font() else {
        return;
    };
    let (metrics, coverage) = font.rasterize('\u{f023}', 24.0);
    let rgba: Vec<_> = coverage
        .into_iter()
        .flat_map(|a| [0xf6, 0xf4, 0xef, a])
        .collect();
    panel_blit(
        face,
        &UndoFace {
            rgba,
            w: metrics.width as u32,
            h: metrics.height as u32,
        },
        x,
        y,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use slot_store::wifi_status::WifiSignal;

    #[test]
    fn phase_and_live_signal_status_text() {
        for (phase, signal, expected) in [
            (WifiPhase::Associating, None, "Associating..."),
            (WifiPhase::Authenticating, None, "Authenticating..."),
            (WifiPhase::ObtainingAddress, None, "Obtaining an address..."),
            (
                WifiPhase::Failed(WifiFailure::WrongPassword),
                None,
                "Wrong password",
            ),
            (
                WifiPhase::Failed(WifiFailure::NetworkNotFound),
                None,
                "Network not found",
            ),
            (
                WifiPhase::Failed(WifiFailure::NoAddress),
                None,
                "No DHCP lease",
            ),
            (
                WifiPhase::Failed(WifiFailure::Other),
                None,
                "Could not connect to network",
            ),
            (
                WifiPhase::Connected,
                Some(WifiSignal::Dbm(-54)),
                "Connected: -54 dBm (192.168.1.2)",
            ),
            (
                WifiPhase::Connected,
                Some(WifiSignal::Quality(71)),
                "Connected: 71% (192.168.1.2)",
            ),
        ] {
            let status = WifiStatus {
                phase: Some(phase),
                signal,
                ip: Some("192.168.1.2".into()),
                ..Default::default()
            };
            assert_eq!(WifiMenu::status_text(&status).as_deref(), Some(expected));
        }
    }
}
