use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use super::dsp::{DspChain, DspConfig};
use super::sinc::{SincQuality, SincResampler};
use super::GBA_HZ;

pub const BASELINE_LATENCY_MS: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResamplerKind {
    #[default]
    Linear,
    Sinc,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioLook {
    pub rate: Option<u32>,
    pub latency_ms: Option<u32>,
    pub resampler: Option<ResamplerKind>,
    pub quality: Option<SincQuality>,
    pub sync: Option<bool>,
    pub gain_db: Option<f32>,
    pub dsp: Option<DspConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioLookError(pub String);
impl fmt::Display for AudioLookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AudioLookError {}

pub(super) fn finite(value: &str, key: &str) -> Result<f32, AudioLookError> {
    value
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| AudioLookError(format!("{key}: expected a finite number, got {value:?}")))
}

impl AudioLook {
    pub fn from_entries(
        entries: &BTreeMap<String, String>,
        base_dir: &Path,
        card_root: &Path,
    ) -> Result<AudioLook, AudioLookError> {
        let integer = |key: &str, min: u32, max: u32| {
            entries
                .get(key)
                .map(|v| {
                    v.parse::<u32>()
                        .ok()
                        .filter(|n| (min..=max).contains(n))
                        .ok_or_else(|| {
                            AudioLookError(format!(
                                "{key}: expected an integer from {min} to {max}, got {v:?}"
                            ))
                        })
                })
                .transpose()
        };
        let rate = integer("slot_audio_rate", 8000, 192000)?;
        let latency_ms = integer("slot_audio_latency_ms", 1, 10000)?;
        let resampler = entries
            .get("slot_audio_resampler")
            .map(|v| match v.as_str() {
                "linear" => Ok(ResamplerKind::Linear),
                "sinc" => Ok(ResamplerKind::Sinc),
                _ => Err(AudioLookError(format!(
                    "slot_audio_resampler: unsupported value {v:?}"
                ))),
            })
            .transpose()?;
        let quality = entries
            .get("slot_audio_resampler_quality")
            .map(|v| {
                SincQuality::parse(v).ok_or_else(|| {
                    AudioLookError(format!(
                        "slot_audio_resampler_quality: unsupported value {v:?}"
                    ))
                })
            })
            .transpose()?;
        let sync = entries
            .get("slot_audio_sync")
            .map(|v| match v.as_str() {
                "true" => Ok(true),
                "false" => Err(AudioLookError(
                    "slot_audio_sync=false is unsupported".into(),
                )),
                _ => Err(AudioLookError(format!(
                    "slot_audio_sync: unsupported value {v:?}"
                ))),
            })
            .transpose()?;
        let gain_db = entries
            .get("slot_audio_gain_db")
            .map(|v| {
                finite(v, "slot_audio_gain_db").and_then(|db| {
                    if (-120.0..=60.0).contains(&db) {
                        Ok(db)
                    } else {
                        Err(AudioLookError(
                            "slot_audio_gain_db: supported range is -120 to 60 dB".into(),
                        ))
                    }
                })
            })
            .transpose()?;
        let dsp = entries
            .get("slot_audio_dsp")
            .map(|v| {
                if Path::new(v).is_absolute() {
                    return Err(AudioLookError(
                        "slot_audio_dsp: absolute path is forbidden".into(),
                    ));
                }
                let path = base_dir.join(v);
                if v.is_empty() {
                    return Err(AudioLookError("slot_audio_dsp: empty path".into()));
                }
                let root = card_root
                    .canonicalize()
                    .map_err(|e| AudioLookError(format!("slot_audio_dsp card root: {e}")))?;
                let path = path.canonicalize().map_err(|e| {
                    AudioLookError(format!("slot_audio_dsp {}: {e}", path.display()))
                })?;
                if !path.starts_with(&root) {
                    return Err(AudioLookError(
                        "slot_audio_dsp: path escapes card root".into(),
                    ));
                }
                let text = std::fs::read_to_string(&path).map_err(|e| {
                    AudioLookError(format!("slot_audio_dsp {}: {e}", path.display()))
                })?;
                DspConfig::parse(&text)
            })
            .transpose()?;
        Ok(Self {
            rate,
            latency_ms,
            resampler,
            quality,
            sync,
            gain_db,
            dsp,
        })
    }

    pub fn output_rate(&self) -> u32 {
        self.rate.unwrap_or(GBA_HZ)
    }
    pub fn output_latency_ms(&self) -> u32 {
        self.latency_ms.unwrap_or(BASELINE_LATENCY_MS)
    }
    pub fn gain(&self) -> f32 {
        gain_db(self.gain_db.unwrap_or(0.0))
    }
}

pub fn gain_db(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

// This retains linear interpolation's baseline phase convention without i16 intermediates.
struct FloatLinear {
    step: f64,
    scaled: f64,
    pos: f64,
    prev: [f32; 2],
}
impl FloatLinear {
    fn new(src: f64, dst: f64) -> Self {
        Self {
            step: src / dst,
            scaled: src / dst,
            pos: 0.0,
            prev: [0.0; 2],
        }
    }
    fn process(&mut self, frame: [f32; 2], emit: &mut dyn FnMut([f32; 2])) {
        while self.pos < 1.0 {
            emit(std::array::from_fn(|c| {
                (self.prev[c] as f64 + (frame[c] as f64 - self.prev[c] as f64) * self.pos) as f32
            }));
            self.pos += self.scaled;
        }
        self.pos -= 1.0;
        self.prev = frame;
    }
}
enum LookResampler {
    Linear(FloatLinear),
    Sinc(SincResampler),
}
impl LookResampler {
    fn process(&mut self, input: &[f32], emit: &mut dyn FnMut([f32; 2])) {
        match self {
            Self::Linear(r) => {
                for f in input.chunks_exact(2) {
                    r.process([f[0], f[1]], emit);
                }
            }
            Self::Sinc(r) => r.process(input, emit),
        }
    }
}

const LOOK_BLOCK: usize = 1024;

pub struct LookProcessor {
    dsp: DspChain,
    resampler: LookResampler,
    gain: f32,
    dsp_output: Box<[f32; LOOK_BLOCK * 4]>,
}
impl LookProcessor {
    pub fn new(
        look: &AudioLook,
        input_rate: f64,
        clock_rate: f64,
        output_rate: u32,
    ) -> Result<Self, AudioLookError> {
        if !input_rate.is_finite()
            || input_rate < 1.0
            || !clock_rate.is_finite()
            || clock_rate < 1.0
            || output_rate == 0
        {
            return Err(AudioLookError("invalid audio sample rate".into()));
        }
        let dsp = look
            .dsp
            .as_ref()
            .map_or_else(|| Ok(DspChain::empty()), |d| d.build(input_rate as f32))?;
        let resampler = match look.resampler.unwrap_or_default() {
            ResamplerKind::Linear => {
                LookResampler::Linear(FloatLinear::new(clock_rate, output_rate as f64))
            }
            ResamplerKind::Sinc => LookResampler::Sinc(SincResampler::new(
                clock_rate,
                output_rate as f64,
                look.quality.unwrap_or_default(),
            )),
        };
        Ok(Self {
            dsp,
            resampler,
            gain: look.gain(),
            dsp_output: Box::new([0.0; LOOK_BLOCK * 4]),
        })
    }
    pub fn process(
        &mut self,
        input: &[i16],
        ratio: f64,
        user_gain: f32,
        emit: &mut dyn FnMut(&[i16]),
    ) {
        match &mut self.resampler {
            LookResampler::Linear(r) => {
                if ratio.is_finite() && ratio > 0.0 {
                    r.scaled = r.step / ratio;
                }
            }
            LookResampler::Sinc(r) => r.set_ratio(ratio),
        }
        let mut output = [0i16; 1024];
        let mut at = 0;
        let mut quantize = |frame: [f32; 2]| {
            for value in frame {
                output[at] = (value * user_gain * 32768.0).round() as i16;
                at += 1;
            }
            if at == output.len() {
                emit(&output);
                at = 0;
            }
        };
        let resampler = &mut self.resampler;
        let gain = self.gain / 32768.0;
        let mut floats = [0.0; LOOK_BLOCK * 2];
        let scratch = self.dsp_output.as_mut();
        for chunk in input.chunks(LOOK_BLOCK * 2) {
            for (out, &value) in floats.iter_mut().zip(chunk) {
                *out = value as f32 * gain;
            }
            let mut len = 0;
            self.dsp.process(&floats[..chunk.len()], &mut |frame| {
                // Large configured EQ blocks can emit more than this input batch.
                if len == scratch.len() {
                    resampler.process(scratch, &mut quantize);
                    len = 0;
                }
                scratch[len..len + 2].copy_from_slice(&frame);
                len += 2;
            });
            if len > 0 {
                resampler.process(&scratch[..len], &mut quantize);
            }
        }
        if at > 0 {
            emit(&output[..at]);
        }
    }
}
