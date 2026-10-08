//! The cheat list: every cheat in the seated cart's `.cht`, over the paused game, each with
//! whether it is on. Drawn in the quick menu's type, bar and ink, so it reads as the same
//! kind of screen.
//!
//! A file can hold hundreds of cheats, so only `CHEAT_ROWS` are on the panel at once and the
//! binary rasters just those: the list scrolls under a window, and the window's rows are what
//! the faces belong to.

use crate::draw::{Draw, TexId, OUT_H, OUT_W};
use crate::plate::{centred_hints, hint_face, UndoFace, HINT_H, LEGEND_GAP};
use crate::power_menu::{MENU_H, MENU_INK, MENU_PAD, MENU_PX};
use crate::slot_chrome::{edge, opening};
use crate::text;

/// Rows on the panel at once.
pub const CHEAT_ROWS: usize = 7;
/// The quick menu's original pitch: seven rows of it clear both the count and the legend.
pub const CHEAT_PITCH: f32 = 52.0;
pub const CHEAT_TOP: f32 = 54.0;
/// Where the count's line of type goes, above the first row.
const COUNT_Y: f32 = 16.0;
/// Labels start this far in and values end this far in, as on the quick menu.
const EDGE: f32 = 32.0;
/// Room kept at the right of every row for ON or OFF.
const VALUE_ROOM: f32 = 120.0;
const BAR_INSET: f32 = 4.0;
const TYPE_DROP: f32 = 4.0;
const LEGEND_Y: f32 = 427.0;
/// A long description shrinks to this before it is cut short.
const MIN_PX: f32 = 20.0;

/// The widest a description may be set: the panel less both edges and the value's room.
pub fn cheat_label_width() -> u32 {
    (OUT_W as f32 - 2.0 * EDGE - VALUE_ROOM) as u32
}

/// One cheat's description on a line of menu type. Shrunk toward `MIN_PX` if it needs to be,
/// and cut with an ellipsis if even that is not enough: a list has to stay one line a row.
pub fn cheat_label_face(desc: &str) -> UndoFace {
    let Some(font) = text::label_font() else {
        return UndoFace {
            rgba: Vec::new(),
            w: 0,
            h: 0,
        };
    };
    let max_w = cheat_label_width() as f32;
    let mut layout = text::fit(font, desc, max_w, 1, MENU_PX, MIN_PX);
    let whole = desc
        .to_uppercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let first = layout.lines.first().cloned().unwrap_or_default();
    if first.trim() != whole {
        let mut kept = first;
        while !kept.is_empty()
            && text::line_width(font, &format!("{}...", kept.trim_end()), layout.px, layout.tracking)
                > max_w
        {
            kept.pop();
        }
        layout.lines = vec![format!("{}...", kept.trim_end())];
    }
    let set = layout
        .lines
        .iter()
        .map(|l| text::line_width(font, l, layout.px, layout.tracking))
        .fold(0.0, f32::max);
    let w = set.ceil() as u32 + 2 * MENU_PAD;
    let mut rgba = vec![0u8; (w * MENU_H * 4) as usize];
    text::draw_centred(&mut rgba, w, MENU_H, &layout, MENU_INK);
    UndoFace { rgba, w, h: MENU_H }
}

/// B DONE and A ON / OFF, in the order `CheatMenu::legend` holds them.
pub fn cheat_legend_faces() -> [UndoFace; 2] {
    [hint_face("B", "Done"), hint_face("A", "On / Off")]
}

/// The window's first row for a list of `len` with `row` in hand, moved only as far as it has
/// to be from where it was: the bar walks to the edge of the window before the list scrolls.
pub fn cheat_window(top: usize, row: usize, len: usize) -> usize {
    let most = len.saturating_sub(CHEAT_ROWS);
    let top = if row < top {
        row
    } else if row >= top + CHEAT_ROWS {
        row + 1 - CHEAT_ROWS
    } else {
        top
    };
    top.min(most)
}

/// The list as it stands this frame.
pub struct CheatMenu<'a> {
    /// The cheat in hand, as an index into the whole list.
    pub row: usize,
    /// The first cheat on the panel.
    pub top: usize,
    /// Whether each cheat in the whole list is on.
    pub enabled: &'a [bool],
    /// One per window row, top to bottom, for whichever cheat is in that row now. `None` for a
    /// row whose face has not been rastered yet.
    pub labels: &'a [Option<(TexId, u32, u32)>],
    /// The quick menu's own OFF and ON, each grey then lit.
    pub off: Option<[(TexId, u32, u32); 2]>,
    pub on: Option<[(TexId, u32, u32); 2]>,
    /// "12 OF 140", centred over the rows.
    pub count: Option<(TexId, u32)>,
    /// `cheat_legend_faces`, with their widths.
    pub legend: Option<[(TexId, u32); 2]>,
}

impl CheatMenu<'_> {
    pub fn draw(&self, out: &mut Vec<Draw>) {
        out.push(Draw::Rect {
            x: 0.0,
            y: 0.0,
            w: OUT_W as f32,
            h: OUT_H as f32,
            colour: opening(),
        });
        let len = self.enabled.len();
        if len == 0 {
            return;
        }
        if self.row >= self.top && self.row < self.top + CHEAT_ROWS {
            out.push(Draw::Rect {
                x: 0.0,
                y: row_top(self.row - self.top) + BAR_INSET,
                w: OUT_W as f32,
                h: CHEAT_PITCH - 2.0 * BAR_INSET,
                colour: edge(),
            });
        }
        if let Some((tex, w)) = self.count {
            push(out, tex, (OUT_W as f32 - w as f32) / 2.0, COUNT_Y, w, HINT_H);
        }
        let (right, pad) = (OUT_W as f32 - EDGE, MENU_PAD as f32);
        for slot in 0..CHEAT_ROWS {
            let index = self.top + slot;
            if index >= len {
                break;
            }
            let y = row_top(slot) + TYPE_DROP;
            if let Some(Some((tex, w, h))) = self.labels.get(slot) {
                push(out, *tex, EDGE - pad, y, *w, *h);
            }
            let lit = index == self.row;
            let value = if self.enabled[index] { self.on } else { self.off };
            if let Some(faces) = value {
                let (tex, w, h) = faces[lit as usize];
                push(out, tex, right + pad - w as f32, y, w, h);
            }
        }
        if let Some(legend) = self.legend {
            for (tex, w, x) in centred_hints(&legend, LEGEND_GAP) {
                push(out, tex, x, LEGEND_Y, w, HINT_H);
            }
        }
    }
}

fn row_top(slot: usize) -> f32 {
    CHEAT_TOP + CHEAT_PITCH * slot as f32
}

/// A face at its own size, on whole pixels, which is the only place it is sharp.
fn push(out: &mut Vec<Draw>, tex: TexId, x: f32, y: f32, w: u32, h: u32) {
    out.push(Draw::Tex {
        x: x.round(),
        y: y.round(),
        w: w as f32,
        h: h as f32,
        tex,
        alpha: 1.0,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_stays_put_until_the_bar_reaches_its_edge() {
        assert_eq!(cheat_window(0, 3, 100), 0);
        assert_eq!(cheat_window(0, CHEAT_ROWS, 100), 1);
        assert_eq!(cheat_window(10, 9, 100), 9);
        assert_eq!(cheat_window(0, 99, 100), 100 - CHEAT_ROWS);
    }

    #[test]
    fn a_short_list_never_scrolls() {
        assert_eq!(cheat_window(0, 4, 5), 0);
    }

    #[test]
    fn the_rows_clear_the_count_and_the_legend() {
        assert!(CHEAT_TOP > COUNT_Y + HINT_H as f32);
        assert!(row_top(CHEAT_ROWS) <= LEGEND_Y);
    }
}
