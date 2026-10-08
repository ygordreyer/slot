//! Shader parameter rows and their on-device adjustment legend.

use crate::plate::{arrows_hint_face, centred_hints, hint_face, UndoFace, HINT_H, LEGEND_GAP};
use crate::slot_chrome::{edge, opening};
use crate::{Draw, TexId, CHEAT_ROWS, OUT_H, OUT_W};

pub fn parameter_label_face(label: &str) -> UndoFace {
    crate::cheat_menu::list_label_face(label, 432)
}

pub fn parameter_value_text(value: f32, step: f32, default: f32) -> String {
    let decimals = (0..=6).find(|&decimals| {
        let scale = 10f64.powi(decimals);
        [step, value, default].into_iter().all(|number| {
            let number = number as f64;
            // Allow f32 representation error without hiding a meaningful decimal place.
            ((number * scale).round() / scale - number).abs() <= number.abs() * f32::EPSILON as f64
        })
    });
    let number = match decimals {
        Some(decimals) if value.abs() < 1_000_000.0 => format!("{value:.*}", decimals as usize),
        _ => format!("{value:.6e}"),
    };
    format!("{number}{}", if value == default { " *" } else { "" })
}

pub type ParameterFaces = [(TexId, u32, u32); 2];
pub fn parameter_legend_faces() -> [UndoFace; 4] {
    [
        hint_face("B", "Back"),
        arrows_hint_face("Change"),
        hint_face("X", "Reset"),
        hint_face("Y", "Reset all"),
    ]
}
pub struct ShaderParams<'a> {
    pub row: usize,
    pub top: usize,
    pub len: usize,
    pub faces: &'a [Option<ParameterFaces>],
    pub legend: Option<[(TexId, u32); 4]>,
}
impl ShaderParams<'_> {
    pub fn draw(&self, out: &mut Vec<Draw>) {
        out.push(Draw::Rect {
            x: 0.0,
            y: 0.0,
            w: OUT_W as f32,
            h: OUT_H as f32,
            colour: opening(),
        });
        out.push(Draw::Rect {
            x: 0.0,
            y: 58.0 + (self.row - self.top) as f32 * 52.0,
            w: OUT_W as f32,
            h: 44.0,
            colour: edge(),
        });
        for slot in 0..CHEAT_ROWS.min(self.len.saturating_sub(self.top)) {
            if let Some(Some(faces)) = self.faces.get(slot) {
                for (i, &(tex, w, h)) in faces.iter().enumerate() {
                    out.push(Draw::Tex {
                        x: if i == 0 { 24.0 } else { 696.0 - w as f32 },
                        y: 58.0 + slot as f32 * 52.0,
                        w: w as f32,
                        h: h as f32,
                        tex,
                        alpha: 1.0,
                    });
                }
            }
        }
        if let Some(legend) = self.legend {
            for (tex, w, x) in centred_hints(&legend, LEGEND_GAP) {
                out.push(Draw::Tex {
                    x,
                    y: 427.0,
                    w: w as f32,
                    h: HINT_H as f32,
                    tex,
                    alpha: 1.0,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_show_the_precision_of_the_step_current_value_and_default() {
        assert_eq!(parameter_value_text(0.333, 0.111, 0.0), "0.333");
        assert_eq!(parameter_value_text(0.25, 0.25, 0.0), "0.25");
        assert_eq!(parameter_value_text(0.35, 0.1, 0.0), "0.35");
        assert_eq!(parameter_value_text(0.3, 0.1, 0.125), "0.300");
        assert_eq!(parameter_value_text(0.25, 0.25, 0.25), "0.25 *");
    }

    #[test]
    fn labels_reserve_value_room_and_legend_fits_the_panel() {
        let face =
            parameter_label_face("A long parameter label with words that need to fit on one line");
        assert!(face.w <= 432 + 2 * crate::MENU_PAD);
        assert_eq!(parameter_value_text(0.0, 0.1, 0.0), "0.0 *");
        assert_eq!(
            parameter_value_text(0.0000001, 0.0000001, 0.0),
            "1.000000e-7"
        );
        assert!(parameter_value_text(1e-20, 1e-20, 0.0).contains("e-20"));
        let legend = parameter_legend_faces();
        let width: u32 = legend.iter().map(|f| f.w).sum();
        assert!(width as f32 + 3.0 * LEGEND_GAP <= OUT_W as f32);
    }
}
