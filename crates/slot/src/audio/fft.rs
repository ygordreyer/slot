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

use std::f64::consts::PI;

#[derive(Clone, Copy, Default)]
pub(super) struct Complex {
    pub re: f32,
    pub im: f32,
}
impl Complex {
    pub fn mul(self, b: Self) -> Self {
        Self {
            re: self.re * b.re - self.im * b.im,
            im: self.re * b.im + self.im * b.re,
        }
    }
}

pub(super) struct Fft {
    inverse: Vec<usize>,
    phases: Vec<Complex>,
    scratch: Vec<Complex>,
}
impl Fft {
    pub fn new(log2: u32) -> Self {
        let n = 1usize << log2;
        Self {
            inverse: (0..n)
                .map(|i| i.reverse_bits() >> (usize::BITS - log2))
                .collect(),
            phases: (0..=2 * n)
                .map(|i| {
                    let p = PI * (i as f64 - n as f64) / n as f64;
                    Complex {
                        re: p.cos() as f32,
                        im: p.sin() as f32,
                    }
                })
                .collect(),
            scratch: vec![Complex::default(); n],
        }
    }
    fn butterflies(phases: &[Complex], out: &mut [Complex], dir: isize) {
        let n = out.len();
        let mut step = 1;
        while step < n {
            for i in (0..n).step_by(step * 2) {
                for j in i..i + step {
                    let phase = n as isize + n as isize * dir / step as isize * (j - i) as isize;
                    let m = phases[phase as usize].mul(out[j + step]);
                    let a = out[j];
                    out[j + step] = Complex {
                        re: a.re - m.re,
                        im: a.im - m.im,
                    };
                    out[j] = Complex {
                        re: a.re + m.re,
                        im: a.im + m.im,
                    };
                }
            }
            step *= 2;
        }
    }
    pub fn forward(&self, input: &[f32], out: &mut [Complex], stride: usize) {
        for (i, &rev) in self.inverse.iter().enumerate() {
            out[rev] = Complex {
                re: input[i * stride],
                im: 0.0,
            };
        }
        Self::butterflies(&self.phases, out, -1);
    }
    pub fn inverse(&mut self, input: &[Complex], out: &mut [f32], stride: usize) {
        for (i, &rev) in self.inverse.iter().enumerate() {
            self.scratch[rev] = input[i];
        }
        Self::butterflies(&self.phases, &mut self.scratch, 1);
        let gain = 1.0 / self.scratch.len() as f32;
        for (i, c) in self.scratch.iter().enumerate() {
            out[i * stride] = gain * c.re;
        }
    }
}

pub(super) fn bessel(x: f64) -> f64 {
    let (mut sum, mut factorial, mut power, mut two) = (0.0, 1.0, 1.0, 1.0);
    for i in 0..18 {
        sum += power * two / (factorial * factorial);
        power *= x * x;
        two *= 0.25;
        factorial *= (i + 1) as f64;
    }
    sum
}
pub(super) fn kaiser(index: f64, beta: f64) -> f64 {
    // filters.h deliberately uses sqrtf for EQ's window.
    bessel(beta * ((1.0 - index * index) as f32).sqrt() as f64)
}
