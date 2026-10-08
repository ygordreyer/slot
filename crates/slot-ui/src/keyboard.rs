//! Reusable ASCII text entry with button-only editing and a separate raster view.

use zeroize::Zeroize;

use crate::plate::{hint_face, UndoFace, HINT_H, LEGEND_GAP};
use crate::power_menu::{MENU_INK, MENU_PX};
use crate::text::{self, Layout};
use crate::{OUT_H, OUT_W};

pub const KEY_COLS: usize = 10;
pub const KEY_ROWS: usize = 4;
const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789 ";
const SYMBOLS: &[u8] = b" !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardInput {
    Up,
    Down,
    Left,
    Right,
    Type,
    Delete,
    Caps,
    Symbols,
    CursorLeft,
    CursorRight,
    Confirm,
    Reveal,
}

#[derive(Clone, PartialEq, Eq)]
pub enum KeyboardResult {
    Editing,
    Confirmed(String),
    Cancelled,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Keyboard {
    secure: bool,
    pub title: String,
    text: String,
    cursor: usize,
    focus: usize,
    caps: bool,
    symbols: bool,
    password: bool,
    masked: bool,
    min: usize,
    allow_empty: bool,
    max: usize,
    pub hint: String,
}

impl std::fmt::Debug for KeyboardResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Editing => f.write_str("Editing"),
            Self::Confirmed(text) => f
                .debug_struct("Confirmed")
                .field("length", &text.len())
                .finish(),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

impl std::fmt::Debug for Keyboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Keyboard");
        debug.field("title", &self.title);
        if self.secure {
            debug.field("text_length", &self.text.len());
        } else {
            debug.field("text", &self.text);
        }
        debug
            .field("cursor", &self.cursor)
            .field("focus", &self.focus)
            .field("caps", &self.caps)
            .field("symbols", &self.symbols)
            .field("password", &self.password)
            .field("masked", &self.masked)
            .field("min", &self.min)
            .field("allow_empty", &self.allow_empty)
            .field("max", &self.max)
            .field("hint", &self.hint)
            .finish()
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.clear_secret();
    }
}

impl Keyboard {
    pub fn new(title: impl Into<String>, min: usize, max: usize, password: bool) -> Self {
        assert!(min <= max);
        Self {
            secure: false,
            title: title.into(),
            text: String::new(),
            cursor: 0,
            focus: 0,
            caps: false,
            symbols: false,
            password,
            masked: password,
            min,
            allow_empty: false,
            max,
            hint: String::new(),
        }
    }

    pub fn secret(title: impl Into<String>, min: usize, max: usize) -> Self {
        let mut keyboard = Self::new(title, min, max, true);
        keyboard.secure = true;
        keyboard.text.reserve(max);
        keyboard
    }

    pub fn clear_secret(&mut self) {
        if self.secure {
            self.text.zeroize();
            self.cursor = 0;
        }
    }

    /// An empty password can describe an open network, while partial WPA keys remain invalid.
    pub fn allow_empty(mut self) -> Self {
        self.allow_empty = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn focus(&self) -> usize {
        self.focus
    }
    pub fn masked(&self) -> bool {
        self.masked
    }
    pub fn key(&self, index: usize) -> char {
        let page = if self.symbols { SYMBOLS } else { LETTERS };
        let c = page.get(index).copied().unwrap_or(b' ') as char;
        if self.caps {
            c.to_ascii_uppercase()
        } else {
            c
        }
    }

    pub fn input(&mut self, input: KeyboardInput) -> KeyboardResult {
        let row = self.focus / KEY_COLS;
        let col = self.focus % KEY_COLS;
        match input {
            KeyboardInput::Up => self.focus = ((row + KEY_ROWS - 1) % KEY_ROWS) * KEY_COLS + col,
            KeyboardInput::Down => self.focus = ((row + 1) % KEY_ROWS) * KEY_COLS + col,
            KeyboardInput::Left => self.focus = row * KEY_COLS + (col + KEY_COLS - 1) % KEY_COLS,
            KeyboardInput::Right => self.focus = row * KEY_COLS + (col + 1) % KEY_COLS,
            KeyboardInput::Type if self.text.len() < self.max => {
                self.text.insert(self.cursor, self.key(self.focus));
                self.cursor += 1;
                self.hint.clear();
            }
            KeyboardInput::Type => self.hint = format!("Maximum {} characters", self.max),
            KeyboardInput::Delete if self.text.is_empty() => return KeyboardResult::Cancelled,
            KeyboardInput::Delete if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
                self.hint.clear();
            }
            KeyboardInput::Delete => {}
            KeyboardInput::Caps => self.caps = !self.caps,
            KeyboardInput::Symbols => self.symbols = !self.symbols,
            KeyboardInput::CursorLeft => self.cursor = self.cursor.saturating_sub(1),
            KeyboardInput::CursorRight => self.cursor = (self.cursor + 1).min(self.text.len()),
            KeyboardInput::Reveal if self.password => self.masked = !self.masked,
            KeyboardInput::Reveal => {}
            KeyboardInput::Confirm
                if self.text.len() >= self.min || (self.allow_empty && self.text.is_empty()) =>
            {
                return KeyboardResult::Confirmed(if self.secure {
                    self.cursor = 0;
                    std::mem::take(&mut self.text)
                } else {
                    self.text.clone()
                })
            }
            KeyboardInput::Confirm => self.hint = format!("Enter at least {} characters", self.min),
        }
        KeyboardResult::Editing
    }

    pub fn display(&self) -> String {
        let start = self.cursor.saturating_sub(12);
        let end = (start + 24).min(self.text.len());
        let mut display = String::with_capacity(end - start + 7);
        if start > 0 {
            display.push_str("...");
        }
        if self.masked {
            display.extend(std::iter::repeat_n('*', self.cursor - start));
        } else {
            display.push_str(&self.text[start..self.cursor]);
        }
        display.push('|');
        if self.masked {
            display.extend(std::iter::repeat_n('*', end - self.cursor));
        } else {
            display.push_str(&self.text[self.cursor..end]);
        }
        if end < self.text.len() {
            display.push_str("...");
        }
        display
    }

    pub fn face(&self) -> UndoFace {
        let mut face = panel_face();
        panel_text(&mut face, &self.title, 24, 14, 672, MENU_PX);
        panel_text_owned(&mut face, self.display(), 24, 66, 672, MENU_PX);
        let hint = if self.hint.is_empty() {
            format!(
                "{} / {}    L / R: cursor    {}",
                self.text.len(),
                self.max,
                if self.caps { "CAPS" } else { "lowercase" }
            )
        } else {
            self.hint.clone()
        };
        panel_text(&mut face, &hint, 24, 110, 672, 22.0);
        for index in 0..KEY_COLS * KEY_ROWS {
            let x = 28 + (index % KEY_COLS) as u32 * 66;
            let y = 158 + (index / KEY_COLS) as u32 * 52;
            if index == self.focus {
                panel_rect(&mut face, x, y, 62, 48, [0x4d, 0x4d, 0x57, 255]);
            }
            let key = self.key(index);
            let label = if key == ' ' {
                "SP".into()
            } else {
                key.to_string()
            };
            panel_text(&mut face, &label, x, y + 4, 62, MENU_PX);
        }
        panel_legend(
            &mut face,
            &[("A", "Type"), ("B", "Delete / Back"), ("X", "Caps")],
            394,
        );
        let mut legend = vec![("Y", "Symbols"), ("START", "Done")];
        if self.password {
            legend.push(("SELECT", "Show"));
        }
        panel_legend(&mut face, &legend, 437);
        face
    }
}

pub(crate) fn panel_face() -> UndoFace {
    UndoFace {
        rgba: [0x05, 0x05, 0x08, 255].repeat((OUT_W * OUT_H) as usize),
        w: OUT_W,
        h: OUT_H,
    }
}

pub(crate) fn panel_rect(face: &mut UndoFace, x: u32, y: u32, w: u32, h: u32, colour: [u8; 4]) {
    for y in y..(y + h).min(face.h) {
        for x in x..(x + w).min(face.w) {
            let i = ((y * face.w + x) * 4) as usize;
            face.rgba[i..i + 4].copy_from_slice(&colour);
        }
    }
}

/// Preserve case, whitespace and punctuation when showing credentials.
pub(crate) fn panel_text(face: &mut UndoFace, value: &str, x: u32, y: u32, w: u32, px: f32) {
    panel_text_owned(face, value.to_owned(), x, y, w, px);
}

fn panel_text_owned(face: &mut UndoFace, mut line: String, x: u32, y: u32, w: u32, px: f32) {
    let Some(font) = text::label_font() else {
        line.zeroize();
        return;
    };
    if text::line_width(font, &line, px, 0.0) > w as f32 {
        let ellipsis_width = text::line_width(font, "...", px, 0.0);
        while !line.is_empty() && text::line_width(font, &line, px, 0.0) + ellipsis_width > w as f32
        {
            line.pop();
        }
        line.push_str("...");
    }
    let mut layout = Layout {
        lines: vec![line],
        px,
        tracking: 0.0,
    };
    let mut rgba = vec![0; (w * 40 * 4) as usize];
    text::draw_centred(&mut rgba, w, 40, &layout, MENU_INK);
    layout.lines.zeroize();
    panel_blit(face, &UndoFace { rgba, w, h: 40 }, x, y);
}

pub(crate) fn panel_blit(face: &mut UndoFace, source: &UndoFace, x: u32, y: u32) {
    for sy in 0..source.h.min(face.h.saturating_sub(y)) {
        for sx in 0..source.w.min(face.w.saturating_sub(x)) {
            let s = ((sy * source.w + sx) * 4) as usize;
            let d = (((y + sy) * face.w + x + sx) * 4) as usize;
            let a = source.rgba[s + 3] as u32;
            for c in 0..3 {
                face.rgba[d + c] = ((source.rgba[s + c] as u32 * a
                    + face.rgba[d + c] as u32 * (255 - a))
                    / 255) as u8;
            }
        }
    }
}

pub(crate) fn panel_legend(face: &mut UndoFace, hints: &[(&str, &str)], y: u32) {
    let faces: Vec<_> = hints
        .iter()
        .map(|(key, text)| hint_face(key, text))
        .collect();
    let width = faces.iter().map(|f| f.w as f32).sum::<f32>()
        + LEGEND_GAP * faces.len().saturating_sub(1) as f32;
    debug_assert!(width <= OUT_W as f32);
    let mut x = (OUT_W as f32 - width) / 2.0;
    for hint in &faces {
        panel_blit(face, hint, x.max(0.0) as u32, y);
        x += hint.w as f32 + LEGEND_GAP;
    }
    debug_assert!(y + HINT_H <= OUT_H);
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeyboardInput::*;

    #[test]
    fn debug_redacts_secret_text_and_all_confirmed_payloads() {
        let mut keyboard = Keyboard::secret("Password", 1, 127);
        keyboard.text.push_str("secret");
        for masked in [true, false] {
            keyboard.masked = masked;
            let debug = format!("{keyboard:?}");
            assert!(!debug.contains("secret"));
            assert!(debug.contains('6'));
        }
        let debug = format!("{:?}", KeyboardResult::Confirmed("secret".into()));
        assert!(!debug.contains("secret"));
        assert!(debug.contains('6'));
    }

    #[test]
    fn navigation_wraps_on_every_edge_and_page() {
        let mut k = Keyboard::new("Name", 1, 32, false);
        k.input(Left);
        assert_eq!(k.focus(), 9);
        k.input(Right);
        assert_eq!(k.focus(), 0);
        k.input(Up);
        assert_eq!(k.focus(), 30);
        k.input(Down);
        assert_eq!(k.focus(), 0);
        k.input(Symbols);
        k.input(Up);
        assert_eq!(k.focus(), 30);
    }

    #[test]
    fn every_printable_ascii_byte_is_available() {
        let mut k = Keyboard::new("Password", 8, 63, true);
        let mut available = std::collections::BTreeSet::new();
        for input in [Caps, Caps, Symbols] {
            k.input(input);
            for i in 0..KEY_ROWS * KEY_COLS {
                available.insert(k.key(i) as u8);
            }
        }
        assert_eq!(
            available.into_iter().collect::<Vec<_>>(),
            (32..=126).collect::<Vec<_>>()
        );
    }

    #[test]
    fn cursor_edits_preserve_case_and_password_display_is_optional() {
        let mut k = Keyboard::new("Password", 2, 3, true);
        k.input(Type);
        k.input(Caps);
        k.input(Type);
        assert_eq!(k.text(), "aA");
        assert_eq!(k.display(), "**|");
        k.input(Reveal);
        assert_eq!(k.display(), "aA|");
        k.input(CursorLeft);
        k.input(Right);
        k.input(Type);
        assert_eq!(k.text(), "aBA");
        k.input(Type);
        assert_eq!(k.text(), "aBA");
        assert!(!k.hint.is_empty());
        k.input(Delete);
        assert_eq!(k.text(), "aA");
        k.input(CursorRight);
        k.input(Delete);
        k.input(Delete);
        assert_eq!(k.input(Delete), KeyboardResult::Cancelled);
    }

    #[test]
    fn secret_entry_transfers_ownership_and_explicit_clear_empties_the_buffer() {
        let mut keyboard = Keyboard::secret("Password", 1, 127);
        keyboard.input(Type);
        assert_eq!(
            keyboard.input(Confirm),
            KeyboardResult::Confirmed("a".into())
        );
        assert!(keyboard.text().is_empty());
        keyboard.input(Type);
        keyboard.clear_secret();
        assert!(keyboard.text().is_empty());
    }

    #[test]
    fn confirmation_enforces_the_callers_limits() {
        let mut k = Keyboard::new("Password", 8, 63, true);
        assert_eq!(k.input(Confirm), KeyboardResult::Editing);
        assert_eq!(k.hint, "Enter at least 8 characters");
        for _ in 0..8 {
            k.input(Type);
        }
        assert_eq!(
            k.input(Confirm),
            KeyboardResult::Confirmed("aaaaaaaa".into())
        );
        assert_eq!(
            Keyboard::new("Optional", 0, 10, false).input(Confirm),
            KeyboardResult::Confirmed(String::new())
        );
    }

    #[test]
    fn long_entry_keeps_the_cursor_in_the_visible_text() {
        let mut k = Keyboard::new("Password", 8, 63, true);
        for _ in 0..63 {
            k.input(Type);
        }
        assert!(k.display().contains('|'));
        assert!(k.display().len() < 32);
        assert_eq!(k.face().rgba.len(), (OUT_W * OUT_H * 4) as usize);
    }
}
