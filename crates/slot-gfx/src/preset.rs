//! Legacy RetroArch presets, scale rules, and shared parameter declarations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Border,
    Edge,
    Repeat,
    Mirror,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleType {
    Source,
    Viewport,
    Absolute,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub kind: ScaleType,
    pub value: f32,
}
#[derive(Clone, Debug)]
pub struct PassSpec {
    pub path: PathBuf,
    pub linear: bool,
    pub wrap: Wrap,
    pub scale: Option<[Axis; 2]>,
    pub frame_mod: u32,
    pub alias: String,
    pub float: bool,
    pub srgb: bool,
    pub mipmap: bool,
}
#[derive(Clone, Debug)]
pub struct LutSpec {
    pub name: String,
    pub path: PathBuf,
    pub linear: bool,
    pub wrap: Wrap,
    pub mipmap: bool,
}
#[derive(Clone, Debug, Default)]
pub struct Preset {
    pub passes: Vec<PassSpec>,
    pub textures: Vec<LutSpec>,
    pub overrides: BTreeMap<String, f32>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub label: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub step: f32,
    pub value: f32,
}
impl Parameter {
    pub fn set(&mut self, value: f32) {
        if value.is_finite() {
            self.value = value.clamp(self.min, self.max);
        }
    }
}

pub fn fields(line: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for c in line.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && c == '#' {
            break;
        } else if !quoted && c.is_whitespace() {
            if !word.is_empty() {
                result.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if quoted {
        return Err("unterminated quoted value".into());
    }
    if !word.is_empty() {
        result.push(word);
    }
    Ok(result)
}

fn entries(text: &str, path: &Path) -> Result<BTreeMap<String, (String, PathBuf)>, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let tokens = fields(value)?;
            out.insert(
                key.trim().to_string(),
                (
                    tokens.join(" "),
                    path.parent().unwrap_or(Path::new("")).to_path_buf(),
                ),
            );
        }
    }
    Ok(out)
}
fn wrap(v: &str) -> Result<Wrap, String> {
    match v {
        "clamp_to_border" => Ok(Wrap::Border),
        "clamp_to_edge" => Ok(Wrap::Edge),
        "repeat" => Ok(Wrap::Repeat),
        "mirrored_repeat" => Ok(Wrap::Mirror),
        _ => Err(format!("unknown wrap mode {v}")),
    }
}
fn kind(v: &str) -> Result<ScaleType, String> {
    match v {
        "source" => Ok(ScaleType::Source),
        "viewport" => Ok(ScaleType::Viewport),
        "absolute" => Ok(ScaleType::Absolute),
        _ => Err(format!("unknown scale type {v}")),
    }
}
fn from_entries(kv: BTreeMap<String, (String, PathBuf)>) -> Result<Preset, String> {
    let get = |key: &str| kv.get(key).map(|(v, _)| v.as_str());
    let flag = |key: &str| -> Result<bool, String> {
        match get(key) {
            None | Some("false" | "0") => Ok(false),
            Some("true" | "1") => Ok(true),
            Some(v) => Err(format!("invalid boolean {key}={v}")),
        }
    };
    let number = |key: &str, default: f32| -> Result<f32, String> {
        let v = match get(key) {
            None => default,
            Some(v) => v.parse().map_err(|_| format!("invalid number {key}={v}"))?,
        };
        if v.is_finite() && v > 0.0 {
            Ok(v)
        } else {
            Err(format!("invalid positive scale {key}"))
        }
    };
    let count: usize = get("shaders")
        .ok_or("missing shaders count")?
        .parse()
        .map_err(|_| "invalid shaders count")?;
    if count == 0 || count > 64 {
        return Err("shaders count must be 1 to 64".into());
    }
    let mut preset = Preset::default();
    for n in 0..count {
        let key = format!("shader{n}");
        let (file, base) = kv.get(&key).ok_or_else(|| format!("missing {key}"))?;
        let mut axes = [Axis {
            kind: ScaleType::Source,
            value: 1.0,
        }; 2];
        let mut explicit = false;
        for (axis, suffix) in ["x", "y"].iter().enumerate() {
            let t =
                get(&format!("scale_type_{suffix}{n}")).or_else(|| get(&format!("scale_type{n}")));
            let s = format!("scale_{suffix}{n}");
            let all = format!("scale{n}");
            explicit |= t.is_some() || get(&s).is_some() || get(&all).is_some();
            axes[axis] = Axis {
                kind: kind(t.unwrap_or("source"))?,
                value: number(if get(&s).is_some() { &s } else { &all }, 1.0)?,
            };
        }
        let frame_mod = get(&format!("frame_count_mod{n}"))
            .unwrap_or("0")
            .parse()
            .map_err(|_| "invalid frame count modulus")?;
        preset.passes.push(PassSpec {
            path: base.join(file),
            linear: flag(&format!("filter_linear{n}"))?,
            wrap: wrap(get(&format!("wrap_mode{n}")).unwrap_or("clamp_to_border"))?,
            scale: explicit.then_some(axes),
            frame_mod,
            alias: get(&format!("alias{n}")).unwrap_or("").to_string(),
            float: flag(&format!("float_framebuffer{n}"))?,
            srgb: flag(&format!("srgb_framebuffer{n}"))?,
            mipmap: flag(&format!("mipmap_input{n}"))?,
        });
    }
    for name in get("textures")
        .unwrap_or("")
        .split(';')
        .filter(|s| !s.is_empty())
    {
        let (file, base) = kv.get(name).ok_or_else(|| format!("missing LUT {name}"))?;
        preset.textures.push(LutSpec {
            name: name.into(),
            path: base.join(file),
            linear: flag(&format!("{name}_linear"))?,
            wrap: wrap(get(&format!("{name}_wrap_mode")).unwrap_or("clamp_to_border"))?,
            mipmap: flag(&format!("{name}_mipmap"))?,
        });
    }
    for name in get("parameters")
        .unwrap_or("")
        .split(';')
        .filter(|s| !s.is_empty())
    {
        if let Some(v) = get(name) {
            let v: f32 = v.parse().map_err(|_| format!("invalid parameter {name}"))?;
            if !v.is_finite() {
                return Err(format!("invalid parameter {name}"));
            }
            preset.overrides.insert(name.into(), v);
        }
    }
    Ok(preset)
}

pub fn parse_preset(text: &str, path: &Path) -> Result<Preset, String> {
    from_entries(entries(text, path)?)
}

/// The reader is injected so reference traversal is testable without a filesystem or GL.
pub fn resolve_preset(
    path: &Path,
    read: &impl Fn(&Path) -> Result<String, String>,
) -> Result<Preset, String> {
    fn visit(
        path: &Path,
        depth: usize,
        read: &impl Fn(&Path) -> Result<String, String>,
    ) -> Result<BTreeMap<String, (String, PathBuf)>, String> {
        if depth >= 16 {
            return Err("preset reference depth exceeds 16 (possible cycle)".into());
        }
        let text = read(path)?;
        let mut kv = BTreeMap::new();
        for line in text.lines() {
            if let Some(reference) = line.trim().strip_prefix("#reference") {
                let names = fields(reference)?;
                if names.len() != 1 {
                    return Err("invalid #reference".into());
                }
                kv.extend(visit(
                    &path.parent().unwrap_or(Path::new("")).join(&names[0]),
                    depth + 1,
                    read,
                )?);
            }
        }
        kv.extend(entries(&text, path)?);
        Ok(kv)
    }
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("glsl"))
    {
        let mut preset = Preset::default();
        preset.passes.push(PassSpec {
            path: path.into(),
            linear: false,
            wrap: Wrap::Edge,
            scale: None,
            frame_mod: 0,
            alias: String::new(),
            float: false,
            srgb: false,
            mipmap: false,
        });
        Ok(preset)
    } else {
        from_entries(visit(path, 0, read)?)
    }
}

pub fn output_size(
    scale: Option<[Axis; 2]>,
    source: [u32; 2],
    viewport: [u32; 2],
    last: bool,
) -> Result<[u32; 2], String> {
    let Some(axes) = scale else {
        return Ok(if last { viewport } else { source });
    };
    let mut out = [0; 2];
    for i in 0..2 {
        let size = axes[i].value
            * match axes[i].kind {
                ScaleType::Source => source[i] as f32,
                ScaleType::Viewport => viewport[i] as f32,
                ScaleType::Absolute => 1.0,
            };
        if !size.is_finite() || size <= 0.0 || size > 16384.0 {
            return Err(format!("invalid pass dimension {size}"));
        }
        out[i] = size.round().max(1.0) as u32;
    }
    Ok(out)
}

pub fn collect_parameters<'a>(
    sources: impl IntoIterator<Item = &'a str>,
    overrides: &BTreeMap<String, f32>,
    saved: &BTreeMap<String, f32>,
) -> Result<Vec<Parameter>, String> {
    let mut out: Vec<Parameter> = Vec::new();
    for src in sources {
        for line in src.lines() {
            let Some(rest) = line.trim().strip_prefix("#pragma parameter ") else {
                continue;
            };
            let words = fields(rest)?;
            if words.len() < 5 {
                return Err(format!("invalid parameter declaration: {line}"));
            }
            if out.iter().any(|p| p.name == words[0]) {
                continue;
            }
            let value = |i: usize| -> Result<f32, String> {
                let v: f32 = words[i]
                    .parse()
                    .map_err(|_| format!("invalid parameter: {line}"))?;
                if v.is_finite() {
                    Ok(v)
                } else {
                    Err(format!("invalid parameter: {line}"))
                }
            };
            let (default, min, max) = (value(2)?, value(3)?, value(4)?);
            let step = if words.len() > 5 {
                value(5)?
            } else {
                (max - min) / 10.0
            };
            if min > max || step <= 0.0 {
                return Err(format!("invalid parameter range: {line}"));
            }
            let mut p = Parameter {
                name: words[0].clone(),
                label: words[1].clone(),
                default: default.clamp(min, max),
                min,
                max,
                step,
                value: default.clamp(min, max),
            };
            if let Some(v) = overrides.get(&p.name) {
                p.set(*v);
            }
            p.default = p.value;
            if let Some(v) = saved.get(&p.name) {
                p.set(*v);
            }
            out.push(p);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_bare_paths_options_and_unknown_keys() {
        let p = parse_preset("shaders = \"1\"\nshader0 = \"../passes/a.glsl\"\nfilter_linear0 = true\nwrap_mode0 = repeat\nscale_type_x0 = viewport\nscale_y0 = 2\nframe_count_mod0 = 8\nalias0 = A\nfloat_framebuffer0 = true\nsrgb_framebuffer0 = true\nmipmap_input0 = true\ntextures = L\nL = lut.png\nL_linear = true\nparameters = P\nP = \"0.4\"\nunknown = ignored", Path::new("pack/look.glslp")).unwrap();
        assert_eq!(p.passes[0].path, Path::new("pack/../passes/a.glsl"));
        assert_eq!(p.passes[0].scale.unwrap()[0].kind, ScaleType::Viewport);
        assert_eq!(p.overrides["P"], 0.4);
        assert!(p.passes[0].linear && p.passes[0].float && p.passes[0].srgb && p.passes[0].mipmap);
        assert_eq!(p.textures[0].path, Path::new("pack/lut.png"));
    }
    #[test]
    fn references_keep_base_paths_and_child_overrides_and_limit_cycles() {
        let read = |p: &Path| {
            Ok(if p == Path::new("base.glslp") {
                "shaders=1\nshader0=pass.glsl\nparameters=P\nP=1"
            } else {
                "#reference \"base.glslp\"\nP=2"
            }
            .into())
        };
        let p = resolve_preset(Path::new("child.glslp"), &read).unwrap();
        assert_eq!(p.passes[0].path, Path::new("pass.glsl"));
        assert_eq!(p.overrides["P"], 2.0);
        assert!(resolve_preset(Path::new("loop"), &|_| Ok("#reference loop".into())).is_err());
        assert!(parse_preset("shaders=2\nshader0=a", Path::new("p")).is_err());
    }
    #[test]
    fn scale_axes_and_last_default() {
        for (kind, value, expected) in [
            (ScaleType::Source, 2.0, [480, 320]),
            (ScaleType::Viewport, 0.5, [360, 240]),
            (ScaleType::Absolute, 100.0, [100, 100]),
        ] {
            assert_eq!(
                output_size(
                    Some([Axis { kind, value }; 2]),
                    [240, 160],
                    [720, 480],
                    false
                )
                .unwrap(),
                expected
            );
        }
        assert_eq!(
            output_size(None, [240, 160], [720, 480], true).unwrap(),
            [720, 480]
        );
        assert_eq!(
            output_size(
                Some(
                    [Axis {
                        kind: ScaleType::Viewport,
                        value: 0.5
                    }; 2]
                ),
                [240, 160],
                [1, 1],
                false
            )
            .unwrap(),
            [1, 1]
        );
    }
    #[test]
    fn parameters_first_declaration_then_preset_then_saved() {
        let a = "#pragma parameter P \"A label\" 1 0 4 0.5";
        let b = "#pragma parameter P \"Duplicate\" 2 0 10 1";
        let p = collect_parameters(
            [a, b],
            &BTreeMap::from([("P".into(), 2.0)]),
            &BTreeMap::from([("P".into(), 3.0)]),
        )
        .unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].label, "A label");
        assert_eq!(p[0].value, 3.0);
        assert_eq!(p[0].default, 2.0);
    }
}

#[cfg(test)]
mod bundled_tests {
    use super::*;
    #[test]
    fn bundled_presets_resolve_every_pass_and_parameter_without_gl() {
        fn walk(dir: &Path, paths: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, paths);
                } else if path.extension().is_some_and(|e| e == "glslp") {
                    paths.push(path);
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../card/Shaders");
        let mut paths = Vec::new();
        walk(&root, &mut paths);
        assert_eq!(paths.len(), 10);
        for path in paths {
            let read = |p: &Path| std::fs::read_to_string(p).map_err(|e| e.to_string());
            let preset = resolve_preset(&path, &read).unwrap();
            let sources = preset
                .passes
                .iter()
                .map(|p| read(&p.path).unwrap())
                .collect::<Vec<_>>();
            collect_parameters(
                sources.iter().map(String::as_str),
                &preset.overrides,
                &BTreeMap::new(),
            )
            .unwrap();
            let mut size = [240, 160];
            for (i, pass) in preset.passes.iter().enumerate() {
                size = output_size(pass.scale, size, [720, 480], i + 1 == preset.passes.len())
                    .unwrap();
            }
        }
    }
}
