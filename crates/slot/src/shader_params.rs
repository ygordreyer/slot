//! Parameter screen state, live edits, and saved non-default values.

use super::App;
use slot_gfx::preset::Parameter;
use slot_input::{Action, Btn};
use slot_ui::{cheat_window, Draw, ParameterFaces, ShaderParams, Toast, CHEAT_ROWS};
#[cfg(test)]
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct ParameterScreen {
    parameters: Vec<Parameter>,
    row: usize,
    top: usize,
    open: bool,
    generation: u64,
    changed: bool,
    faces: Vec<Option<ParameterFaces>>,
    legend: Option<[(slot_ui::TexId, u32); 4]>,
}
impl App {
    pub fn set_shader_parameters(&mut self, parameters: Vec<Parameter>) {
        self.shader_screen.parameters = parameters;
        self.shader_screen.open = false;
        self.shader_screen.changed = false;
    }
    pub fn shader_parameters(&self) -> &[Parameter] {
        &self.shader_screen.parameters
    }
    pub fn shader_params_open(&self) -> bool {
        self.shader_screen.open
    }
    pub fn shader_params_generation(&self) -> u64 {
        self.shader_screen.generation
    }
    pub fn shader_params_window(&self) -> (usize, usize) {
        (self.shader_screen.row, self.shader_screen.top)
    }
    pub fn set_parameter_face(&mut self, slot: usize, faces: ParameterFaces) {
        if let Some(row) = self.shader_screen.faces.get_mut(slot) {
            *row = Some(faces);
        }
    }
    pub fn set_parameter_legend(&mut self, faces: [(slot_ui::TexId, u32); 4]) {
        self.shader_screen.legend = Some(faces);
    }
    pub fn take_parameter_changes(&mut self) -> Option<Vec<Parameter>> {
        std::mem::take(&mut self.shader_screen.changed)
            .then(|| self.shader_screen.parameters.clone())
    }
    pub(super) fn open_shader_params(&mut self) {
        if self.shader_screen.parameters.is_empty() {
            self.show_toast(Toast::NoShaderParams);
            return;
        }
        self.shader_screen.open = true;
        self.shader_screen.generation = self.shader_screen.generation.wrapping_add(1);
        self.shader_screen.row = 0;
        self.shader_screen.top = 0;
        self.shader_screen.faces = vec![None; CHEAT_ROWS];
    }
    pub(super) fn close_shader_params(&mut self) {
        self.shader_screen.open = false;
    }
    pub(super) fn shader_params_input(&mut self, action: Action) {
        let s = &mut self.shader_screen;
        match action {
            Action::GbaDown(Btn::Up) => s.row = s.row.saturating_sub(1),
            Action::GbaDown(Btn::Down) => {
                s.row = (s.row + 1).min(s.parameters.len().saturating_sub(1))
            }
            Action::GbaDown(Btn::Left | Btn::Right) => {
                let p = &mut s.parameters[s.row];
                if p.step.is_finite() && p.step > 0.0 {
                    let sign = if action == Action::GbaDown(Btn::Left) {
                        -1.0
                    } else {
                        1.0
                    };
                    let index = ((p.value - p.min) / p.step).round() as f64 + sign;
                    // Preserve the anchor's decimal places as well as the step's.
                    let decimals = [p.min, p.step]
                        .iter()
                        .map(|v| v.to_string().split_once('.').map_or(0, |(_, s)| s.len()))
                        .max()
                        .unwrap_or(0);
                    let scale = 10f64.powi(decimals as i32);
                    let value =
                        (((p.min as f64 + index * p.step as f64) * scale).round() / scale) as f32;
                    let value = value.clamp(p.min, p.max);
                    p.set(if (value - p.default).abs() < p.step * 0.0001 {
                        p.default
                    } else {
                        value
                    });
                    s.changed = true;
                }
            }
            Action::GbaDown(Btn::X) => {
                let p = &mut s.parameters[s.row];
                p.value = p.default;
                s.changed = true;
            }
            Action::GbaDown(Btn::Y) => {
                for p in &mut s.parameters {
                    p.value = p.default;
                }
                s.changed = true;
            }
            Action::GbaDown(Btn::B) | Action::QuickMenu => s.open = false,
            _ => {}
        }
        s.top = cheat_window(s.top, s.row, s.parameters.len());
    }
    pub(super) fn draw_shader_params(&self, out: &mut Vec<Draw>) {
        let s = &self.shader_screen;
        if s.open {
            ShaderParams {
                row: s.row,
                top: s.top,
                len: s.parameters.len(),
                faces: &s.faces,
                legend: s.legend,
            }
            .draw(out);
        }
    }
}

#[cfg(test)]
fn saved_values(parameters: &[Parameter]) -> BTreeMap<String, f32> {
    parameters
        .iter()
        .filter(|p| p.value != p.default)
        .map(|p| (p.name.clone(), p.value))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parameter(value: f32, min: f32, max: f32, step: f32, default: f32) -> Parameter {
        Parameter {
            name: "A".into(),
            label: "Amount".into(),
            value,
            min,
            max,
            step,
            default,
        }
    }

    #[test]
    fn doze_closes_parameters_without_losing_pending_edits() {
        for doze in [Action::LidClose, Action::PowerTap] {
            let mut app = App::new(Vec::new());
            app.set_shader_parameters(vec![parameter(0.5, 0.0, 1.0, 0.1, 0.5)]);
            app.open_shader_params();
            app.apply(Action::GbaDown(Btn::Right));
            let edited = app.shader_parameters()[0].value;
            assert_eq!(edited, 0.6);

            app.apply(doze);
            assert!(matches!(app.phase(), super::super::Phase::Doze { .. }));
            assert!(!app.shader_params_open());
            app.apply(Action::GbaDown(Btn::Right));
            assert_eq!(app.shader_parameters()[0].value, edited);
            assert_eq!(app.take_parameter_changes().unwrap()[0].value, edited);
            assert!(app.take_parameter_changes().is_none());
        }
    }

    #[test]
    fn nine_right_presses_persist_a_clean_decimal() {
        let mut app = App::new(Vec::new());
        app.set_shader_parameters(vec![parameter(0.0, 0.0, 1.0, 0.1, 0.0)]);
        app.open_shader_params();
        for _ in 0..9 {
            app.shader_params_input(Action::GbaDown(Btn::Right));
        }
        assert_eq!(app.shader_parameters()[0].value, 0.9);
        let root = tempfile::tempdir().unwrap();
        slot_store::write_shader_params(
            root.path(),
            "test.glslp",
            &saved_values(app.shader_parameters()),
        )
        .unwrap();
        let path = slot_store::params_path(root.path(), "test.glslp").unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "A = \"0.9\"\n");
    }

    #[test]
    fn adjustments_snap_to_the_minimum_grid_and_preserve_bounds_and_default() {
        for (value, min, max, step, default, button, expected) in [
            (0.35, 0.0, 1.0, 0.1, 0.0, Btn::Right, 0.5),
            (0.35, 0.0, 1.0, 0.1, 0.0, Btn::Left, 0.3),
            (0.4, 0.05, 1.0, 0.1, 0.05, Btn::Right, 0.55),
            (0.9, 0.0, 0.95, 0.1, 0.0, Btn::Right, 0.95),
            (0.05, 0.05, 1.0, 0.1, 0.05, Btn::Left, 0.05),
            (0.2, 0.0, 1.0, 0.1, 0.300001, Btn::Right, 0.300001),
            (0.111, 0.0, 1.0, 0.111, 0.0, Btn::Right, 0.222),
        ] {
            let mut app = App::new(Vec::new());
            app.set_shader_parameters(vec![parameter(value, min, max, step, default)]);
            app.open_shader_params();
            app.shader_params_input(Action::GbaDown(button));
            assert_eq!(
                app.shader_parameters()[0].value,
                expected,
                "value {value}, minimum {min}, step {step}, button {button:?}"
            );
        }
    }

    #[test]
    fn nonpositive_steps_do_not_adjust_or_mark_changes() {
        for step in [0.0, -0.1] {
            let mut app = App::new(Vec::new());
            app.set_shader_parameters(vec![parameter(0.35, 0.0, 1.0, step, 0.0)]);
            app.open_shader_params();
            for button in [Btn::Left, Btn::Right] {
                app.shader_params_input(Action::GbaDown(button));
                assert_eq!(app.shader_parameters()[0].value, 0.35);
                assert!(app.take_parameter_changes().is_none());
            }
        }
    }

    #[test]
    fn edits_clamp_reset_and_return_to_quick_menu() {
        let mut app = App::new(Vec::new());
        let p = Parameter {
            name: "A".into(),
            label: "Amount".into(),
            default: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.5,
            value: 0.5,
        };
        app.set_shader_parameters(vec![p.clone(); 10]);
        app.open_shader_params();
        for _ in 0..3 {
            app.shader_params_input(Action::GbaDown(Btn::Right));
        }
        assert_eq!(app.shader_parameters()[0].value, 1.0);
        app.shader_params_input(Action::GbaDown(Btn::X));
        assert_eq!(app.shader_parameters()[0].value, 0.5);
        for _ in 0..8 {
            app.shader_params_input(Action::GbaDown(Btn::Down));
        }
        assert_eq!(app.shader_params_window(), (8, 2));
        app.shader_params_input(Action::GbaDown(Btn::Left));
        assert_eq!(app.shader_parameters()[8].value, 0.0);
        assert!(saved_values(app.shader_parameters()).contains_key("A"));
        app.shader_params_input(Action::GbaDown(Btn::Y));
        assert!(saved_values(app.shader_parameters()).is_empty());
        assert!(app.take_parameter_changes().is_some());
        assert!(app.take_parameter_changes().is_none());
        app.shader_params_input(Action::GbaDown(Btn::B));
        assert!(!app.shader_params_open());
        app.set_shader_parameters(Vec::new());
        app.open_shader_params();
        assert!(!app.shader_params_open());
    }
}
