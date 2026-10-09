/* Copyright  (C) 2010-2020 The RetroArch team
 *
 * ---------------------------------------------------------------------------------------
 * Port of libretro-common sinc_resampler.c and filters.h.
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

use super::fft::bessel;
use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SincQuality {
    Lowest,
    Lower,
    #[default]
    Normal,
    Higher,
    Highest,
}
impl SincQuality {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "LOWEST" => Some(Self::Lowest),
            "LOWER" => Some(Self::Lower),
            "NORMAL" | "DONTCARE" => Some(Self::Normal),
            "HIGHER" => Some(Self::Higher),
            "HIGHEST" => Some(Self::Highest),
            _ => None,
        }
    }
}

struct Kernel {
    table: std::sync::Arc<[f32]>,
    history: [Vec<f32>; 2],
    taps: usize,
    ptr: usize,
    time: u32,
    phase_bits: u32,
    subphase_bits: u32,
    interpolate: bool,
    nominal_ratio: f64,
    step: u32,
}
fn sinc(x: f64) -> f64 {
    if x.abs() < 0.00001 {
        1.0
    } else {
        x.sin() / x
    }
}
impl Kernel {
    fn new(src_hz: f64, dst_hz: f64, quality: SincQuality) -> Self {
        assert!(src_hz.is_finite() && src_hz > 0.0 && dst_hz.is_finite() && dst_hz > 0.0);
        let ratio = dst_hz / src_hz;
        let (mut cutoff, sidelobes, phase_bits, subphase_bits, beta): (f64, usize, u32, u32, f64) =
            match quality {
                SincQuality::Lowest => (0.98, 2, 12, 10, 0.0),
                SincQuality::Lower => (0.98, 4, 12, 10, 0.0),
                SincQuality::Normal => (0.825, 8, 8, 16, 5.5),
                SincQuality::Higher => (0.90, 32, 10, 14, 10.5),
                SincQuality::Highest => (0.962, 128, 10, 14, 14.5),
            };
        let mut taps = sidelobes * 2;
        if ratio < 1.0 {
            cutoff *= ratio;
            taps = (taps as f64 / ratio).ceil() as usize;
        }
        taps = (taps + 7) & !7;
        let phases = 1usize << phase_bits;
        let interpolate = beta != 0.0;
        let stride = if interpolate { 2 } else { 1 };
        let mut table = vec![0.0; phases * taps * stride];
        let norm = if interpolate { bessel(beta) } else { sinc(0.0) };
        let coefficient = |p: usize, j: usize| {
            let w = 2.0 * (j * phases + p) as f64 / (phases * taps) as f64 - 1.0;
            let window = if interpolate {
                bessel(beta * (1.0 - w * w).max(0.0).sqrt())
            } else {
                sinc(PI * w)
            };
            (cutoff * sinc(PI * (taps as f64 / 2.0) * w * cutoff) * window / norm) as f32
        };
        for p in 0..phases {
            for j in 0..taps {
                table[p * stride * taps + j] = coefficient(p, j);
            }
        }
        if interpolate {
            for p in 0..phases {
                for j in 0..taps {
                    let next = if p + 1 == phases {
                        coefficient(p + 1, j)
                    } else {
                        table[(p + 1) * stride * taps + j]
                    };
                    table[(p * stride + 1) * taps + j] = next - table[p * stride * taps + j];
                }
            }
        }
        let mut result = Self {
            table: table.into(),
            history: [vec![0.0; taps * 2], vec![0.0; taps * 2]],
            taps,
            ptr: 0,
            time: 0,
            phase_bits,
            subphase_bits,
            interpolate,
            nominal_ratio: ratio,
            step: 1,
        };
        result.set_ratio(1.0);
        result
    }
    fn set_ratio(&mut self, correction: f64) {
        let phases = (1u32 << (self.phase_bits + self.subphase_bits)) as f64;
        let step = phases / (self.nominal_ratio * correction);
        if step.is_finite() && step >= 1.0 && step <= u32::MAX as f64 - phases {
            self.step = step as u32;
        }
    }
    fn process(&mut self, input: &[f32], emit: &mut dyn FnMut([f32; 2])) {
        let phases = 1u32 << (self.phase_bits + self.subphase_bits);
        let mut frames = input.chunks_exact(2);
        let mut remaining = frames.len();
        while remaining > 0 {
            while remaining > 0 && self.time >= phases {
                if self.ptr == 0 {
                    self.ptr = self.taps;
                }
                self.ptr -= 1;
                let frame = frames.next().unwrap();
                for (c, &value) in frame.iter().enumerate() {
                    self.history[c][self.ptr] = value;
                    self.history[c][self.ptr + self.taps] = value;
                }
                self.time -= phases;
                remaining -= 1;
            }
            while self.time < phases {
                let phase = (self.time >> self.subphase_bits) as usize;
                let stride = if self.interpolate { 2 } else { 1 };
                let table = &self.table[phase * self.taps * stride..];
                let frac = (self.time & ((1 << self.subphase_bits) - 1)) as f32
                    / (1 << self.subphase_bits) as f32;
                let mut sum = [0.0; 2];
                // Match the scalar reference's four-product accumulation order.
                for j in (0..self.taps).step_by(4) {
                    let coeff: [f32; 4] = std::array::from_fn(|k| {
                        table[j + k]
                            + if self.interpolate {
                                table[self.taps + j + k] * frac
                            } else {
                                0.0
                            }
                    });
                    for (c, acc) in sum.iter_mut().enumerate() {
                        let h = &self.history[c][self.ptr + j..];
                        *acc +=
                            h[0] * coeff[0] + h[1] * coeff[1] + h[2] * coeff[2] + h[3] * coeff[3];
                    }
                }
                emit(sum);
                self.time += self.step;
            }
        }
    }
}

// Four halving stages match the reference and cover the worker's maximum 6x
// fast-forward, including nominal downsampling and rate-control correction.
const DEC_STAGES: usize = 4;
const DEC_BLOCK: usize = 256;

struct Scratch {
    data: [f32; DEC_BLOCK * 2],
    output: [f32; DEC_BLOCK * 2],
    previous: [f32; DEC_BLOCK * 2],
    stage_output: [f32; DEC_BLOCK * 2],
    pending: [Pending; 2],
}

struct Pending {
    frames: [[f32; 2]; DEC_BLOCK * 2],
    head: usize,
    len: usize,
}
impl Pending {
    fn new() -> Self {
        Self {
            frames: [[0.0; 2]; DEC_BLOCK * 2],
            head: 0,
            len: 0,
        }
    }
    fn push(&mut self, input: &[f32]) {
        for frame in input.chunks_exact(2) {
            assert!(self.len < self.frames.len());
            self.frames[(self.head + self.len) % self.frames.len()] = [frame[0], frame[1]];
            self.len += 1;
        }
    }
    fn pop(&mut self) -> [f32; 2] {
        assert!(self.len > 0);
        let frame = self.frames[self.head];
        self.head = (self.head + 1) % self.frames.len();
        self.len -= 1;
        frame
    }
}

pub struct SincResampler {
    scratch: Box<Scratch>,
    kernel: Kernel,
    shadow: Kernel,
    stages: [Kernel; DEC_STAGES],
    nominal_ratio: f64,
    design_ratio: f64,
    ratio: f64,
    dec_stages: usize,
    fade_stages: usize,
    warm_left: usize,
    fade_left: usize,
    fade_len: usize,
}
impl Kernel {
    fn restart(&mut self) {
        for h in &mut self.history {
            h.fill(0.0);
        }
        self.ptr = 0;
        self.time = 1 << (self.phase_bits + self.subphase_bits);
    }
    fn copy_state(&mut self, source: &Self) {
        for c in 0..2 {
            self.history[c].copy_from_slice(&source.history[c]);
        }
        self.ptr = source.ptr;
        self.time = source.time;
    }
    fn sibling(&self) -> Self {
        Self {
            table: self.table.clone(),
            history: [vec![0.0; self.taps * 2], vec![0.0; self.taps * 2]],
            taps: self.taps,
            ptr: 0,
            time: 0,
            phase_bits: self.phase_bits,
            subphase_bits: self.subphase_bits,
            interpolate: self.interpolate,
            nominal_ratio: self.nominal_ratio,
            step: self.step,
        }
    }
}
impl SincResampler {
    pub fn new(src_hz: f64, dst_hz: f64, quality: SincQuality) -> Self {
        let kernel = Kernel::new(src_hz, dst_hz, quality);
        let (cutoff, beta, taps): (f64, f64, f64) = match quality {
            SincQuality::Higher => (0.90, 10.5, 128.0),
            SincQuality::Highest => (0.962, 14.5, 512.0),
            _ => (0.825, 5.5, 32.0),
        };
        let taps = (taps / 0.9).ceil() as usize;
        let taps = (taps + 7) & !7;
        let cutoff = cutoff * 0.45;
        let norm = bessel(beta);
        let table: std::sync::Arc<[f32]> = (0..taps)
            .map(|j| {
                let w = 2.0 * j as f64 / taps as f64 - 1.0;
                (cutoff
                    * sinc(PI * (taps as f64 / 2.0) * w * cutoff)
                    * bessel(beta * (1.0 - w * w).max(0.0).sqrt())
                    / norm) as f32
            })
            .collect::<Vec<_>>()
            .into();
        let stages = std::array::from_fn(|_| Kernel {
            table: table.clone(),
            history: [vec![0.0; taps * 2], vec![0.0; taps * 2]],
            taps,
            ptr: 0,
            time: 0,
            phase_bits: 0,
            subphase_bits: 1,
            interpolate: false,
            nominal_ratio: 0.5,
            step: 4,
        });
        let ratio = dst_hz / src_hz;
        Self {
            scratch: Box::new(Scratch {
                data: [0.0; DEC_BLOCK * 2],
                output: [0.0; DEC_BLOCK * 2],
                previous: [0.0; DEC_BLOCK * 2],
                stage_output: [0.0; DEC_BLOCK * 2],
                pending: [Pending::new(), Pending::new()],
            }),
            shadow: kernel.sibling(),
            kernel,
            stages,
            nominal_ratio: ratio,
            design_ratio: ratio.min(1.0),
            ratio,
            dec_stages: 0,
            fade_stages: 0,
            warm_left: 0,
            fade_left: 0,
            fade_len: 0,
        }
    }
    pub fn set_ratio(&mut self, correction: f64) {
        let ratio = self.nominal_ratio * correction;
        let phases = (1u32 << (self.kernel.phase_bits + self.kernel.subphase_bits)) as f64;
        let step = phases / ratio;
        if step.is_finite() && step >= 1.0 && step <= u32::MAX as f64 - phases {
            self.ratio = ratio;
        }
    }
    fn switch(&mut self, stages: usize) {
        for pending in &mut self.scratch.pending {
            pending.len = 0;
        }
        let mut warm = self.kernel.taps << stages;
        self.shadow.copy_state(&self.kernel);
        self.kernel.restart();
        for s in self.dec_stages..stages {
            self.stages[s].restart();
            warm += self.stages[s].taps << s;
        }
        // Filter support is measured in source frames, regardless of later ratios.
        self.warm_left = warm;
        self.fade_len = (warm as f64 * self.ratio).min(8192.0) as usize + 1;
        self.fade_len = self.fade_len.max(256);
        self.fade_left = self.fade_len;
        self.fade_stages = self.dec_stages;
        self.dec_stages = stages;
    }
    fn chain(
        stages: &mut [Kernel],
        data: &mut [f32; DEC_BLOCK * 2],
        out: &mut [f32; DEC_BLOCK * 2],
        mut frames: usize,
    ) -> usize {
        for stage in stages {
            let mut at = 0;
            stage.process(&data[..frames * 2], &mut |f| {
                out[at..at + 2].copy_from_slice(&f);
                at += 2;
            });
            frames = at / 2;
            data[..at].copy_from_slice(&out[..at]);
        }
        frames
    }
    fn render(
        kernel: &mut Kernel,
        input: &[f32],
        ratio: f64,
        out: &mut [f32; DEC_BLOCK * 2],
    ) -> usize {
        kernel.set_ratio(ratio / kernel.nominal_ratio);
        let mut at = 0;
        kernel.process(input, &mut |f| {
            out[at..at + 2].copy_from_slice(&f);
            at += 2;
        });
        at / 2
    }
    fn blend(
        pending: &mut [Pending; 2],
        warm_left: usize,
        fade_left: &mut usize,
        fade_len: usize,
        emit: &mut dyn FnMut([f32; 2]),
    ) {
        let [old, new] = pending;
        while *fade_left > 0 && old.len > 0 && new.len > 0 {
            let a = old.pop();
            let b = new.pop();
            let w = if warm_left > 0 {
                0.0
            } else {
                1.0 - *fade_left as f32 / fade_len as f32
            };
            emit(std::array::from_fn(|c| a[c] + (b[c] - a[c]) * w));
            if warm_left == 0 {
                *fade_left -= 1;
            }
        }
        if *fade_left == 0 {
            old.len = 0;
            while new.len > 0 {
                emit(new.pop());
            }
        }
    }
    pub fn process(&mut self, input: &[f32], emit: &mut dyn FnMut([f32; 2])) {
        if self.dec_stages == 0 && self.fade_left == 0 && self.ratio >= self.design_ratio * 0.97 {
            self.kernel.set_ratio(self.ratio / self.nominal_ratio);
            self.kernel.process(input, emit);
            return;
        }
        let mut input = &input[..input.len() / 2 * 2];
        while !input.is_empty() {
            if self.fade_left == 0 {
                let mut stages = self.dec_stages;
                while stages < DEC_STAGES
                    && self.ratio * ((1 << stages) as f64)
                        < self.design_ratio * if stages == 0 { 0.97 } else { 0.9 }
                {
                    stages += 1;
                }
                while stages > 0
                    && self.ratio * ((1 << (stages - 1)) as f64)
                        >= self.design_ratio * if stages == 1 { 0.975 } else { 1.25 }
                {
                    stages -= 1;
                }
                if stages != self.dec_stages {
                    self.switch(stages);
                }
                if self.dec_stages == 0 && self.fade_left == 0 {
                    self.kernel.set_ratio(self.ratio / self.nominal_ratio);
                    self.kernel.process(input, emit);
                    return;
                }
            }
            // A fade can finish and require another switch on any source frame.
            let step = if self.fade_left > 0 { 1 } else { DEC_BLOCK };
            let (chunk, rest) = input.split_at(input.len().min(step * 2));
            input = rest;
            let Scratch {
                data,
                output: out,
                previous,
                stage_output,
                pending,
            } = self.scratch.as_mut();
            data[..chunk.len()].copy_from_slice(chunk);
            let mut n = chunk.len() / 2;
            let now = self.dec_stages;
            let got;
            if self.fade_left == 0 {
                n = Self::chain(&mut self.stages[..now], data, stage_output, n);
                got = Self::render(
                    &mut self.kernel,
                    &data[..n * 2],
                    self.ratio * (1 << now) as f64,
                    out,
                );
            } else {
                let old = self.fade_stages;
                let common = now.min(old);
                n = Self::chain(&mut self.stages[..common], data, stage_output, n);
                let old_frames;
                if now == common {
                    got = Self::render(
                        &mut self.kernel,
                        &data[..n * 2],
                        self.ratio * (1 << now) as f64,
                        out,
                    );
                    n = Self::chain(&mut self.stages[common..old], data, stage_output, n);
                    old_frames = Self::render(
                        &mut self.shadow,
                        &data[..n * 2],
                        self.ratio * (1 << old) as f64,
                        previous,
                    );
                } else {
                    old_frames = Self::render(
                        &mut self.shadow,
                        &data[..n * 2],
                        self.ratio * (1 << old) as f64,
                        previous,
                    );
                    n = Self::chain(&mut self.stages[common..now], data, stage_output, n);
                    got = Self::render(
                        &mut self.kernel,
                        &data[..n * 2],
                        self.ratio * (1 << now) as f64,
                        out,
                    );
                }
                // Halving stages can emit on different calls. Pair by stream position,
                // never by call-local index or by repeating the old call's last frame.
                pending[0].push(&previous[..old_frames * 2]);
                pending[1].push(&out[..got * 2]);
                self.warm_left = self.warm_left.saturating_sub(chunk.len() / 2);
                Self::blend(
                    pending,
                    self.warm_left,
                    &mut self.fade_left,
                    self.fade_len,
                    emit,
                );
                continue;
            }
            for frame in out[..got * 2].chunks_exact(2) {
                emit([frame[0], frame[1]]);
            }
        }
    }
}
