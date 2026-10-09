/* Copyright  (C) 2010-2020 The RetroArch team
 *
 * ---------------------------------------------------------------------------------------
 * Port of libretro-common audio DSP and filter helpers (eq.c, fft.c, filters.h,
 * reverb.c, iir.c, panning.c).
 * ---------------------------------------------------------------------------------------
 *
 * Permission is hereby granted, free of charge,
 * to any person obtaining a copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation the rights to
 * use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software,
 * and to permit persons to whom the Software is furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED,
 * INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
 * IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
 * WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

use super::fft::{kaiser, Complex, Fft};
use super::look::AudioLookError;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub struct DspConfig {
    filters: Vec<FilterConfig>,
}
#[derive(Debug, Clone, PartialEq)]
enum FilterConfig {
    Eq {
        frequencies: Vec<f32>,
        gains: Vec<f32>,
        log2: u32,
        beta: f32,
    },
    Reverb {
        dry: f32,
        wet: f32,
        damping: f32,
        width: f32,
        room: f32,
    },
    Iir {
        kind: String,
        frequency: f32,
        quality: f32,
        gain: f32,
    },
    Panning {
        left: [f32; 2],
        right: [f32; 2],
    },
}

pub(super) fn config_entries(text: &str) -> Result<BTreeMap<String, String>, AudioLookError> {
    let mut map = BTreeMap::new();
    for (line_no, line) in text.lines().enumerate() {
        let mut quoted = false;
        let end = line
            .char_indices()
            .find_map(|(i, c)| {
                if c == '"' {
                    quoted = !quoted;
                }
                (c == '#' && !quoted).then_some(i)
            })
            .unwrap_or(line.len());
        let line = line[..end].trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=').ok_or_else(|| {
            AudioLookError(format!("DSP line {}: expected key = value", line_no + 1))
        })?;
        let value = value.trim();
        let value = if value.starts_with('"') {
            value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or_else(|| {
                    AudioLookError(format!("DSP line {}: unterminated quote", line_no + 1))
                })?
        } else {
            value
        };
        if key.trim().is_empty() {
            return Err(AudioLookError(format!(
                "DSP line {}: empty key",
                line_no + 1
            )));
        }
        map.insert(key.trim().to_owned(), value.to_owned());
    }
    Ok(map)
}
fn floats(value: &str, key: &str) -> Result<Vec<f32>, AudioLookError> {
    value
        .split_whitespace()
        .map(|s| super::look::finite(s, key))
        .collect()
}
impl DspConfig {
    pub fn parse(text: &str) -> Result<Self, AudioLookError> {
        let entries = config_entries(text)?;
        let count = entries.get("filters").map_or(Ok(0), |s| {
            s.parse::<usize>()
                .map_err(|_| AudioLookError("DSP filters must be an integer".into()))
        })?;
        if count > 32 {
            return Err(AudioLookError("DSP supports at most 32 filters".into()));
        }
        let mut filters = Vec::with_capacity(count);
        for i in 0..count {
            let key = format!("filter{i}");
            let kind = entries
                .get(&key)
                .ok_or_else(|| AudioLookError(format!("DSP missing {key}")))?;
            // RetroArch resolves numbered instance keys before shared type keys.
            let get = |name: &str| {
                entries
                    .get(&format!("filter{i}_{name}"))
                    .or_else(|| entries.get(&format!("{kind}_{name}")))
            };
            let number = |name: &str, default: f32| {
                get(name).map_or(Ok(default), |v| {
                    super::look::finite(v, &format!("{kind}_{name}"))
                })
            };
            let array = |name: &str, defaults: &[f32]| {
                get(name).map_or_else(|| Ok(defaults.to_vec()), |v| floats(v, name))
            };
            filters.push(match kind.as_str() {
                "eq" => {
                    let log2 = get("block_size_log2")
                        .map_or(Ok(8), |v| {
                            v.parse::<i32>().map_err(|_| {
                                AudioLookError("eq block_size_log2 must be an integer".into())
                            })
                        })?
                        .clamp(4, 16) as u32;
                    FilterConfig::Eq {
                        frequencies: array("frequencies", &[0.0, f32::MAX])?,
                        gains: array("gains", &[0.0, 0.0])?,
                        log2,
                        beta: number("window_beta", 4.0)?.clamp(0.0, 32.0),
                    }
                }
                "reverb" => FilterConfig::Reverb {
                    dry: number("drytime", 0.43)?.clamp(0.0, 1.0),
                    wet: number("wettime", 0.4)?.clamp(0.0, 1.0),
                    damping: number("damping", 0.8)?.clamp(0.0, 1.0),
                    width: number("roomwidth", 0.56)?.clamp(0.0, 1.0),
                    room: number("roomsize", 0.56)?.clamp(0.0, 1.0),
                },
                "iir" => {
                    let kind = get("type").map_or("LPF", String::as_str);
                    if ![
                        "LPF",
                        "HPF",
                        "APF",
                        "BPCSGF",
                        "BPZPGF",
                        "NOTCH",
                        "PEQ",
                        "BBOOST",
                        "LSH",
                        "HSH",
                        "RIAA_CD",
                        "RIAA_phono",
                    ]
                    .contains(&kind)
                    {
                        return Err(AudioLookError(format!("unsupported iir_type {kind:?}")));
                    }
                    FilterConfig::Iir {
                        kind: kind.to_owned(),
                        frequency: number("frequency", 1024.0)?,
                        quality: number("quality", 0.707)?.clamp(0.01, 100.0),
                        gain: number("gain", 0.0)?.clamp(-60.0, 60.0),
                    }
                }
                "panning" => {
                    let mix = |name, default: [f32; 2]| -> Result<[f32; 2], AudioLookError> {
                        let values = array(name, &default)?;
                        Ok(if values.len() == 2 {
                            [values[0].clamp(-16.0, 16.0), values[1].clamp(-16.0, 16.0)]
                        } else {
                            default
                        })
                    };
                    FilterConfig::Panning {
                        left: mix("left_mix", [1.0, 0.0])?,
                        right: mix("right_mix", [0.0, 1.0])?,
                    }
                }
                _ => return Err(AudioLookError(format!("unsupported DSP filter {kind:?}"))),
            });
        }
        Ok(Self { filters })
    }
    pub fn build(&self, rate: f32) -> Result<DspChain, AudioLookError> {
        if !rate.is_finite() || rate < 1.0 {
            return Err(AudioLookError("DSP input rate must be positive".into()));
        }
        let mut stages = Vec::with_capacity(self.filters.len());
        for config in &self.filters {
            stages.push(match config {
                FilterConfig::Eq {
                    frequencies,
                    gains,
                    log2,
                    beta,
                } => Stage::Eq(Eq::new(rate, frequencies, gains, *log2, *beta)),
                FilterConfig::Reverb {
                    dry,
                    wet,
                    damping,
                    width,
                    room,
                } => Stage::Reverb(Reverb::new(rate, *dry, *wet, *damping, *width, *room)),
                FilterConfig::Iir {
                    kind,
                    frequency,
                    quality,
                    gain,
                } => Stage::Iir(Iir::new(rate, kind, *frequency, *quality, *gain)?),
                FilterConfig::Panning { left, right } => Stage::Panning(*left, *right),
            });
        }
        Ok(DspChain { stages })
    }
}

pub struct DspChain {
    stages: Vec<Stage>,
}
enum Stage {
    Eq(Eq),
    Reverb(Reverb),
    Iir(Iir),
    Panning([f32; 2], [f32; 2]),
}
impl DspChain {
    pub fn empty() -> Self {
        Self { stages: Vec::new() }
    }
    pub fn process(&mut self, input: &[f32], emit: &mut dyn FnMut([f32; 2])) {
        for frame in input.chunks_exact(2) {
            cascade(&mut self.stages, [frame[0], frame[1]], emit);
        }
    }
}
fn cascade(stages: &mut [Stage], frame: [f32; 2], emit: &mut dyn FnMut([f32; 2])) {
    if let Some((first, rest)) = stages.split_first_mut() {
        match first {
            Stage::Eq(eq) => eq.process(frame, &mut |f| cascade(rest, f, emit)),
            Stage::Reverb(rev) => cascade(rest, rev.process(frame), emit),
            Stage::Iir(iir) => cascade(rest, iir.process(frame), emit),
            Stage::Panning(left, right) => cascade(
                rest,
                [
                    frame[0] * left[0] + frame[1] * left[1],
                    frame[0] * right[0] + frame[1] * right[1],
                ],
                emit,
            ),
        }
    } else {
        emit(frame);
    }
}

struct Eq {
    n: usize,
    at: usize,
    fft: Fft,
    filter: Vec<Complex>,
    work: Vec<Complex>,
    block: Vec<f32>,
    output: Vec<f32>,
    save: Vec<f32>,
}
impl Eq {
    fn new(rate: f32, frequencies: &[f32], gains: &[f32], log2: u32, beta: f32) -> Self {
        let n = 1usize << log2;
        let mut bands: Vec<(f32, f32)> = frequencies
            .iter()
            .zip(gains)
            .map(|(&f, &g)| {
                (
                    f.clamp(0.0, rate) / (0.5 * rate),
                    10.0f64.powf(g.clamp(-120.0, 60.0) as f64 / 20.0) as f32,
                )
            })
            .collect();
        if frequencies.is_empty() || gains.is_empty() {
            bands = vec![(0.0, 1.0), (2.0, 1.0)];
        }
        bands.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut response = vec![Complex::default(); n + 1];
        let (mut sf, mut sg) = (0.0f32, 1.0f32);
        let (mut ef, mut eg) = bands[0];
        let mut band = 1;
        for i in 0..=n / 2 {
            let freq = i as f32 / (n / 2) as f32;
            while freq >= ef {
                sf = ef;
                sg = eg;
                if band < bands.len() {
                    (ef, eg) = bands[band];
                    band += 1;
                } else {
                    ef = 1.0;
                    eg = 1.0;
                    break;
                }
            }
            let lerp = if ef > sf {
                (freq - sf) / (ef - sf)
            } else {
                0.5
            };
            let gain = (1.0 - lerp) * sg + lerp * eg;
            response[i].re = gain;
            response[n - i].re = gain;
        }
        let mut time = vec![0.0; n * 2 + 1];
        Fft::new(log2).inverse(&response[..n], &mut time, 1);
        for i in 0..n / 2 {
            time.swap(i, i + n / 2);
        }
        let norm = 1.0 / kaiser(0.0, beta as f64);
        for (i, t) in time[..n].iter_mut().enumerate() {
            let phase = 2.0 * (i as f64 / n as f64 - 0.5);
            *t = (*t as f64 * norm * kaiser(phase, beta as f64)) as f32;
        }
        let fft = Fft::new(log2 + 1);
        let mut filter = vec![Complex::default(); n * 2];
        fft.forward(&time[1..], &mut filter, 1);
        Self {
            n,
            at: 0,
            fft,
            filter,
            work: vec![Complex::default(); n * 2],
            block: vec![0.0; n * 4],
            output: vec![0.0; n * 4],
            save: vec![0.0; n * 2],
        }
    }
    fn process(&mut self, frame: [f32; 2], emit: &mut dyn FnMut([f32; 2])) {
        self.block[self.at * 2..self.at * 2 + 2].copy_from_slice(&frame);
        self.at += 1;
        if self.at != self.n {
            return;
        }
        for c in 0..2 {
            self.fft.forward(&self.block[c..], &mut self.work, 2);
            for (x, h) in self.work.iter_mut().zip(&self.filter) {
                *x = x.mul(*h);
            }
            self.fft.inverse(&self.work, &mut self.output[c..], 2);
        }
        for (x, s) in self.output.iter_mut().zip(&self.save) {
            *x += s;
        }
        self.save.copy_from_slice(&self.output[self.n * 2..]);
        self.at = 0;
        for f in self.output[..self.n * 2].chunks_exact(2) {
            emit([f[0], f[1]]);
        }
    }
}

struct Delay {
    buf: Vec<f32>,
    at: usize,
    store: f32,
}
impl Delay {
    fn new(rate: f32, length: usize) -> Self {
        Self {
            buf: vec![0.0; ((rate as f64 / 44100.0 * length as f64) as usize).max(1)],
            at: 0,
            store: 0.0,
        }
    }
    fn write(&mut self, value: f32) {
        self.buf[self.at] = value;
        self.at = (self.at + 1) % self.buf.len();
    }
}
struct Reverb {
    combs: [Vec<Delay>; 2],
    allpass: [Vec<Delay>; 2],
    dry: f32,
    wet: f32,
    damp: f32,
    feedback: f32,
}
impl Reverb {
    fn new(rate: f32, dry: f32, wet: f32, damp: f32, width: f32, room: f32) -> Self {
        Self {
            combs: std::array::from_fn(|_| {
                [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617]
                    .iter()
                    .map(|&n| Delay::new(rate, n))
                    .collect()
            }),
            allpass: std::array::from_fn(|_| {
                [225, 341, 441, 556]
                    .iter()
                    .map(|&n| Delay::new(rate, n))
                    .collect()
            }),
            dry: dry * 2.0,
            wet: wet * 3.0 * (width / 2.0 + 0.5),
            damp: damp * 0.4,
            feedback: room * 0.28 + 0.7,
        }
    }
    fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        std::array::from_fn(|c| {
            let input = frame[c] * 0.015;
            let mut output = 0.0;
            for comb in &mut self.combs[c] {
                let value = comb.buf[comb.at];
                comb.store = value * (1.0 - self.damp) + comb.store * self.damp;
                comb.write(input + comb.store * self.feedback);
                output += value;
            }
            for ap in &mut self.allpass[c] {
                let value = ap.buf[ap.at];
                ap.write(output + value * 0.5);
                output = -output + value;
            }
            frame[c] * self.dry + output * self.wet
        })
    }
}

struct Iir {
    b: [f32; 3],
    a: [f32; 3],
    state: [[f32; 4]; 2],
}
impl Iir {
    fn new(
        rate: f32,
        kind: &str,
        freq: f32,
        quality: f32,
        gain: f32,
    ) -> Result<Self, AudioLookError> {
        use std::f64::consts::PI;
        let freq = freq.clamp(1.0, rate * 0.49);
        let (freq, gain) = if kind == "RIAA_CD" {
            (5283.0f64, -9.477f64)
        } else {
            (freq as f64, gain as f64)
        };
        let omega = 2.0 * PI * freq / rate as f64;
        let (sn, cs) = omega.sin_cos();
        let alpha = sn / (2.0 * quality as f64);
        let aa = (10.0f64.ln() * gain / 40.0).exp();
        let beta = (aa + aa).sqrt();
        let (b, a) = match kind {
            "LPF" => (
                [(1.0 - cs) / 2.0, 1.0 - cs, (1.0 - cs) / 2.0],
                [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
            ),
            "HPF" => (
                [(1.0 + cs) / 2.0, -(1.0 + cs), (1.0 + cs) / 2.0],
                [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
            ),
            "APF" => (
                [1.0 - alpha, -2.0 * cs, 1.0 + alpha],
                [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
            ),
            "BPCSGF" => (
                [sn / 2.0, 0.0, -sn / 2.0],
                [1.0 + alpha, -2.0 * cs, 1.0 - alpha],
            ),
            "BPZPGF" => ([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cs, 1.0 - alpha]),
            "NOTCH" => ([1.0, -2.0 * cs, 1.0], [1.0 + alpha, -2.0 * cs, 1.0 - alpha]),
            "PEQ" => (
                [1.0 + alpha * aa, -2.0 * cs, 1.0 - alpha * aa],
                [1.0 + alpha / aa, -2.0 * cs, 1.0 - alpha / aa],
            ),
            "LSH" | "BBOOST" => (
                [
                    aa * ((aa + 1.0) - (aa - 1.0) * cs + beta * sn),
                    2.0 * aa * ((aa - 1.0) - (aa + 1.0) * cs),
                    aa * ((aa + 1.0) - (aa - 1.0) * cs - beta * sn),
                ],
                [
                    (aa + 1.0) + (aa - 1.0) * cs + beta * sn,
                    -2.0 * ((aa - 1.0) + (aa + 1.0) * cs),
                    (aa + 1.0) + (aa - 1.0) * cs - beta * sn,
                ],
            ),
            "HSH" | "RIAA_CD" => (
                [
                    aa * ((aa + 1.0) + (aa - 1.0) * cs + beta * sn),
                    -2.0 * aa * ((aa - 1.0) + (aa + 1.0) * cs),
                    aa * ((aa + 1.0) + (aa - 1.0) * cs - beta * sn),
                ],
                [
                    (aa + 1.0) - (aa - 1.0) * cs + beta * sn,
                    2.0 * ((aa - 1.0) - (aa + 1.0) * cs),
                    (aa + 1.0) - (aa - 1.0) * cs - beta * sn,
                ],
            ),
            "RIAA_phono" => {
                let (zeros, poles) = match rate as u32 {
                    44100 => ([-0.2014898, 0.9233820], [0.7083149, 0.9924091]),
                    48000 => ([-0.1766069, 0.9321590], [0.7396325, 0.9931330]),
                    88200 => ([-0.1168735, 0.9648312], [0.8590646, 0.9964002]),
                    96000 => ([-0.1141486, 0.9676817], [0.8699137, 0.9966946]),
                    _ => {
                        return Err(AudioLookError(format!(
                            "RIAA_phono has no reference coefficients at {rate} Hz"
                        )))
                    }
                };
                let poly = |r: [f64; 2]| {
                    let first = (-r[0]) as f32;
                    [
                        1.0,
                        (first as f64 - r[1]) as f32,
                        (-first as f64 * r[1]) as f32,
                    ]
                };
                let b = poly(zeros);
                let a = poly(poles);
                let y = 2.0 * PI * 1000.0 / rate as f64;
                let magnitude = |p: [f32; 3]| {
                    let re =
                        p[0] as f64 + p[1] as f64 * (-y).cos() + p[2] as f64 * (-2.0 * y).cos();
                    let im = p[1] as f64 * (-y).sin() + p[2] as f64 * (-2.0 * y).sin();
                    re * re + im * im
                };
                let g = 1.0 / (magnitude(b) / magnitude(a)).sqrt();
                (b.map(|v| (v as f64 * g) as f32 as f64), a.map(|v| v as f64))
            }
            _ => return Err(AudioLookError(format!("unsupported iir_type {kind:?}"))),
        };
        Ok(Self {
            b: b.map(|x| x as f32),
            a: a.map(|x| x as f32),
            state: [[0.0; 4]; 2],
        })
    }
    fn process(&mut self, frame: [f32; 2]) -> [f32; 2] {
        std::array::from_fn(|c| {
            let s = &mut self.state[c];
            let out = (self.b[0] * frame[c] + self.b[1] * s[0] + self.b[2] * s[1]
                - self.a[1] * s[2]
                - self.a[2] * s[3])
                / self.a[0];
            *s = [frame[c], s[0], out, s[2]];
            out
        })
    }
}
