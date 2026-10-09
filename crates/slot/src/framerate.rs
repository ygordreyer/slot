use std::time::{Duration, Instant};

const SAMPLE: Duration = Duration::from_secs(1);
const LOG: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rates {
    pub display: f64,
    pub emu: f64,
}

impl Rates {
    pub fn text(self) -> String {
        format!("FPS {:.1}  EMU {:.1}", self.display, self.emu)
    }

    pub fn log(self, shader: &str) -> String {
        format!(
            "slot: fps: display={:.1} emu={:.1} shader={shader}",
            self.display, self.emu
        )
    }
}

struct Baseline {
    at: Instant,
    display: u64,
    emu: u64,
    generation: u64,
    logged: Instant,
}

#[derive(Default)]
pub(crate) struct RateWindow {
    baseline: Option<Baseline>,
    pub rates: Option<Rates>,
}

impl RateWindow {
    pub fn observe(
        &mut self,
        now: Instant,
        display: u64,
        emu: u64,
        active: bool,
        generation: u64,
    ) -> bool {
        if !active {
            self.baseline = None;
            self.rates = None;
            return false;
        }
        let reset = self
            .baseline
            .as_ref()
            .is_none_or(|b| generation != b.generation || display < b.display || emu < b.emu);
        if reset {
            self.baseline = Some(Baseline {
                at: now,
                display,
                emu,
                generation,
                logged: now,
            });
            self.rates = None;
            return false;
        }
        let b = self.baseline.as_mut().unwrap();
        let elapsed = now.duration_since(b.at);
        if elapsed >= SAMPLE {
            self.rates = Some(Rates {
                display: (display - b.display) as f64 / elapsed.as_secs_f64(),
                emu: (emu - b.emu) as f64 / elapsed.as_secs_f64(),
            });
            b.at = now;
            b.display = display;
            b.emu = emu;
        }
        if now.duration_since(b.logged) >= LOG && self.rates.is_some() {
            b.logged = now;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_actual_elapsed_time_and_counts_every_core_frame() {
        let start = Instant::now();
        let mut window = RateWindow::default();
        assert!(!window.observe(start, 100, 200, true, 1));
        assert!(!window.observe(start + SAMPLE / 2, 130, 380, true, 1));
        assert_eq!(window.rates, None);
        assert!(!window.observe(start + SAMPLE * 2, 220, 920, true, 1));
        assert_eq!(
            window.rates,
            Some(Rates {
                display: 60.0,
                emu: 360.0
            })
        );
        assert!(!window.observe(start + SAMPLE * 2 + SAMPLE / 2, 250, 1100, true, 1));
        assert_eq!(window.rates.unwrap().text(), "FPS 60.0  EMU 360.0");
    }

    #[test]
    fn logs_once_per_five_seconds_and_preserves_the_format() {
        let start = Instant::now();
        let mut window = RateWindow::default();
        window.observe(start, 0, 0, true, 1);
        for sec in 1..=10 {
            let due = window.observe(
                start + SAMPLE * sec,
                60 * sec as u64,
                60 * sec as u64,
                true,
                1,
            );
            assert_eq!(due, sec % 5 == 0);
            assert!(!window.observe(
                start + SAMPLE * sec,
                60 * sec as u64,
                60 * sec as u64,
                true,
                1
            ));
        }
        let rates = Rates {
            display: 59.7,
            emu: 60.0,
        };
        assert_eq!(rates.text(), "FPS 59.7  EMU 60.0");
        assert_eq!(
            rates.log("crt-lite"),
            "slot: fps: display=59.7 emu=60.0 shader=crt-lite"
        );
    }

    #[test]
    fn pause_disable_and_sleep_hide_rates_and_restart_both_windows() {
        let start = Instant::now();
        let mut window = RateWindow::default();
        window.observe(start, 0, 0, true, 1);
        window.observe(start + SAMPLE, 60, 60, true, 1);
        assert!(window.rates.is_some());
        assert!(!window.observe(start + SAMPLE * 2, 120, 60, false, 1));
        assert_eq!(window.rates, None);
        assert!(!window.observe(start + SAMPLE * 100, 180, 60, true, 1));
        assert_eq!(window.rates, None);
        assert!(!window.observe(start + SAMPLE * 101, 240, 120, true, 1));
        assert_eq!(
            window.rates.unwrap(),
            Rates {
                display: 60.0,
                emu: 60.0
            }
        );
        assert!(window.observe(start + SAMPLE * 105, 480, 360, true, 1));
    }

    #[test]
    fn core_reload_and_resume_generation_reset_even_when_counts_increase() {
        let start = Instant::now();
        let mut window = RateWindow::default();
        window.observe(start, 0, 1000, true, 1);
        window.observe(start + SAMPLE, 60, 1060, true, 1);
        for (sec, count, generation) in [(2, 10, 2), (3, 2000, 3)] {
            assert!(!window.observe(
                start + SAMPLE * sec,
                60 * sec as u64,
                count,
                true,
                generation
            ));
            assert_eq!(window.rates, None);
        }
        assert!(!window.observe(start + SAMPLE * 4, 240, 2060, true, 3));
        assert_eq!(window.rates.unwrap().text(), "FPS 60.0  EMU 60.0");
    }
}
