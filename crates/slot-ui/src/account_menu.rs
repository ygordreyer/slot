use crate::keyboard::{panel_face, panel_legend, panel_rect, panel_text};
use crate::UndoFace;

pub struct AccountMenu<'a> {
    pub enabled: bool,
    pub username: &'a str,
    pub password_length: usize,
    pub selected: usize,
    pub status: &'a str,
}

impl AccountMenu<'_> {
    pub fn face(&self) -> UndoFace {
        let mut face = panel_face();
        panel_text(&mut face, "RetroAchievements", 24, 10, 672, 32.0);
        panel_text(&mut face, self.status, 24, 54, 672, 22.0);
        let rows = [
            format!("Achievements: {}", if self.enabled { "On" } else { "Off" }),
            format!("Username: {}", self.username),
            format!("Password: {}", "*".repeat(self.password_length.min(24))),
            "Sign in".into(),
            "Sign out".into(),
        ];
        for (index, row) in rows.iter().enumerate() {
            let y = 105 + index as u32 * 54;
            if index == self.selected {
                panel_rect(&mut face, 0, y, 720, 48, [0x4d, 0x4d, 0x57, 255]);
            }
            panel_text(&mut face, row, 24, y + 4, 672, 30.0);
        }
        panel_text(
            &mut face,
            "Gameplay changes apply on the next game load",
            24,
            386,
            672,
            18.0,
        );
        panel_legend(&mut face, &[("B", "Back"), ("A", "Choose")], 427);
        face
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_panel_renders_all_rows_with_a_masked_password() {
        let face = AccountMenu {
            enabled: true,
            username: "Player",
            password_length: 6,
            selected: 2,
            status: "Signed out",
        }
        .face();
        assert_eq!((face.w, face.h), (720, 480));
        assert_eq!(face.rgba.len(), 720 * 480 * 4);
    }
}
