//! RetroArch legacy GLSL shaders and multi-pass `.glslp` presets over the game layer.
//!
//! Both stages live in each source, selected by VERTEX/FRAGMENT. Version lines are blanked:
//! GLES 2 uses its default 100, desktop uses 330 core. PARAMETER_UNIFORM enables on-device
//! tuning. Single files retain the slot_filter linear pragma and cropped source coordinates.
//!
//! Presets supply per-pass filtering, wrapping, scales, aliases, frame modulus, LUTs and
//! parameter overrides. Intermediate outputs use FBOs; the final quad uses the game layer's
//! MVPMatrix, including power animations. Explicit final scales get a final copy pass.
//! Original/history coordinates cover only the source rectangle inside the padded 240x160
//! texture; intermediate coordinates cover their complete texture. Passes receive legacy
//! Texture, Orig, PassN, PassPrevN, alias and Prev through Prev6 names, sizes and coordinates.
//! History textures are allocated only when linked programs use history names.
//! GLES 2 NPOT repeat/mipmaps and float/sRGB framebuffer support are checked at load.
//! Missing resources, unsupported sampler budgets and incomplete FBOs return errors to the
//! caller, which restores LCD. GL bindings and sampling state are restored after each draw.

use crate::pipeline::{SRC_H, SRC_W};
use crate::preset::{self, Parameter, PassSpec, Preset, Wrap};
use crate::surface::{GfxError, OUT_H, OUT_W};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
const VERTEX_LOC: u32 = 0;
const TEXCOORD_LOC: u32 = 1;
const VERTS: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
const LINEAR_PRAGMA: &str = "#pragma slot_filter linear";

#[derive(Copy, Clone)]
struct Uniform {
    loc: i32,
    float: bool,
}
impl Uniform {
    const ABSENT: Self = Self {
        loc: -1,
        float: false,
    };
    unsafe fn set_count(self, n: u32) {
        if self.loc >= 0 {
            if self.float {
                gl::Uniform1f(self.loc, n as f32)
            } else {
                gl::Uniform1i(self.loc, n as i32)
            }
        }
    }
    unsafe fn set_direction(self, rewinding: bool) {
        if self.loc >= 0 {
            let n = frame_direction(rewinding);
            if self.float {
                gl::Uniform1f(self.loc, n as f32)
            } else {
                gl::Uniform1i(self.loc, n)
            }
        }
    }
    unsafe fn set_vec2(self, x: f32, y: f32) {
        if self.loc >= 0 {
            gl::Uniform2f(self.loc, x, y)
        }
    }
}
pub fn stage_source(src: &str, stage: &str, es: bool) -> String {
    let body = src
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            // Mali GLES tokenizes quoted parameter labels; keep their lines for diagnostics.
            if trimmed.starts_with("#version") || trimmed.starts_with("#pragma parameter") {
                String::new()
            } else if trimmed.starts_with("//") {
                l.replace('"', " ")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let version = if es { "" } else { "#version 330 core\n" };
    let compatibility = if es {
        ""
    } else if stage == "VERTEX" {
        "#define varying out\n"
    } else {
        "#define varying in\n"
    };
    format!("{version}#define {stage}\n#define PARAMETER_UNIFORM\n{compatibility}#line 1\n{body}\n")
}
pub fn wants_linear(src: &str) -> bool {
    src.lines().any(|l| l.trim() == LINEAR_PRAGMA)
}
fn error(e: impl std::fmt::Display) -> GfxError {
    GfxError::Shader(e.to_string())
}

struct Program {
    id: u32,
    uniforms: HashMap<String, Uniform>,
    samplers: Vec<String>,
    coords: HashMap<String, u32>,
    mvp: i32,
    colour: i32,
}
impl Program {
    fn new(src: &str) -> Result<Self, GfxError> {
        let id = crate::gl::program_with(
            &stage_source(src, "VERTEX", crate::gl::es()),
            &stage_source(src, "FRAGMENT", crate::gl::es()),
            &[("VertexCoord", VERTEX_LOC), ("TexCoord", TEXCOORD_LOC)],
        )?;
        let kinds = unsafe { uniform_kinds(id) };
        let mut samplers = kinds
            .iter()
            .filter(|(_, k)| **k == gl::SAMPLER_2D)
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>();
        samplers.sort();
        let uniforms = kinds
            .iter()
            .map(|(n, k)| {
                (
                    n.clone(),
                    Uniform {
                        loc: crate::gl::uniform_location(id, n),
                        float: *k == gl::FLOAT,
                    },
                )
            })
            .collect();
        let mut coords = HashMap::new();
        unsafe {
            let mut count = 0;
            gl::GetProgramiv(id, gl::ACTIVE_ATTRIBUTES, &mut count);
            let mut name = [0u8; 256];
            for i in 0..count {
                let (mut len, mut size, mut kind) = (0, 0, 0);
                gl::GetActiveAttrib(
                    id,
                    i as u32,
                    256,
                    &mut len,
                    &mut size,
                    &mut kind,
                    name.as_mut_ptr().cast(),
                );
                let n = String::from_utf8_lossy(&name[..len as usize]).into_owned();
                if n.ends_with("TexCoord") {
                    let c = std::ffi::CString::new(n.as_str()).map_err(error)?;
                    let loc = gl::GetAttribLocation(id, c.as_ptr());
                    if loc >= 0 {
                        coords.insert(n, loc as u32);
                    }
                }
            }
        }
        Ok(Self {
            id,
            uniforms,
            samplers,
            coords,
            mvp: crate::gl::uniform_location(id, "MVPMatrix"),
            colour: unsafe { gl::GetAttribLocation(id, c"COLOR".as_ptr()) },
        })
    }
    fn uniform(&self, name: &str) -> Uniform {
        self.uniforms.get(name).copied().unwrap_or(Uniform::ABSENT)
    }
}
impl Drop for Program {
    fn drop(&mut self) {
        unsafe { gl::DeleteProgram(self.id) }
    }
}

#[derive(Clone, Copy)]
struct Texture {
    id: u32,
    size: [u32; 2],
    input: [u32; 2],
    uv: [f32; 4],
    floating: bool,
}
impl Texture {
    fn full(id: u32, size: [u32; 2]) -> Self {
        Self {
            id,
            size,
            input: size,
            uv: [0.0, 0.0, 1.0, 1.0],
            floating: false,
        }
    }
    fn uniform_sizes(self) -> ([f32; 2], [f32; 2]) {
        (self.input.map(|n| n as f32), self.size.map(|n| n as f32))
    }
}

fn frame_count(frame: u32, modulus: u32) -> u32 {
    if modulus == 0 {
        frame
    } else {
        frame % modulus
    }
}

fn frame_direction(rewinding: bool) -> i32 {
    if rewinding {
        -1
    } else {
        1
    }
}

struct SourceHistory<'a> {
    textures: &'a [Texture],
    next: usize,
    ready: bool,
}

fn texture_bindings<'a>(
    orig: Texture,
    prev: Texture,
    outputs: &[Texture],
    aliases: impl Iterator<Item = &'a str>,
    history: SourceHistory<'_>,
) -> HashMap<String, Texture> {
    let index = outputs.len();
    let mut b = HashMap::new();
    b.insert("".into(), outputs.last().copied().unwrap_or(orig));
    b.insert("Orig".into(), orig);
    b.insert(format!("PassPrev{}", index + 1), orig);
    for (i, (t, alias)) in outputs.iter().zip(aliases).enumerate() {
        b.insert(format!("Pass{}", i + 1), *t);
        b.insert(format!("PassPrev{}", index - i), *t);
        if !alias.is_empty() {
            b.insert(alias.to_string(), *t);
        }
    }
    for n in 0..7 {
        let name = if n == 0 {
            "Prev".into()
        } else {
            format!("Prev{n}")
        };
        // Snapshots hand the same texture in for current and previous frames.
        let mut t = if orig.id == prev.id || (n == 0 && !history.ready) {
            prev
        } else if n < history.textures.len() {
            history.textures
                [(history.next + history.textures.len() - 1 - n) % history.textures.len()]
        } else {
            prev
        };
        t.uv = orig.uv;
        t.input = orig.input;
        b.insert(name, t);
    }
    b
}
struct Target {
    tex: Texture,
    fbo: u32,
}
impl Target {
    fn new(size: [u32; 2], spec: &PassSpec, caps: &Caps) -> Result<Self, GfxError> {
        if size.iter().any(|n| *n > caps.max_size) {
            return Err(error("pass exceeds GL_MAX_TEXTURE_SIZE"));
        }
        unsafe {
            let mut id = 0;
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            let (internal, kind) = if spec.float && caps.float {
                (
                    if caps.es { gl::RGBA } else { gl::RGBA16F },
                    if caps.es { 0x8d61 } else { gl::HALF_FLOAT },
                )
            } else if spec.srgb && caps.srgb {
                (
                    if caps.es { 0x8c42 } else { gl::SRGB8_ALPHA8 },
                    gl::UNSIGNED_BYTE,
                )
            } else {
                (
                    if caps.es { gl::RGBA } else { gl::RGBA8 },
                    gl::UNSIGNED_BYTE,
                )
            };
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                internal as i32,
                size[0] as i32,
                size[1] as i32,
                0,
                if caps.es && internal == 0x8c42 {
                    0x8c42
                } else {
                    gl::RGBA
                },
                kind,
                std::ptr::null(),
            );
            sampling(
                size,
                caps.linear(spec.linear, spec.float && caps.float),
                spec.wrap,
                false,
                caps,
            );
            let mut fbo = 0;
            gl::GenFramebuffers(1, &mut fbo);
            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                id,
                0,
            );
            let target = Self {
                tex: Texture {
                    floating: spec.float && caps.float,
                    ..Texture::full(id, size)
                },
                fbo,
            };
            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                return Err(error(format!(
                    "incomplete shader framebuffer: 0x{status:x}"
                )));
            }
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            Ok(target)
        }
    }
}
impl Drop for Target {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.tex.id);
            gl::DeleteFramebuffers(1, &self.fbo)
        }
    }
}
struct Pass {
    program: Program,
    spec: PassSpec,
    target: RefCell<Option<Target>>,
}
struct Lut {
    tex: Texture,
    spec: preset::LutSpec,
}
impl Drop for Lut {
    fn drop(&mut self) {
        unsafe { gl::DeleteTextures(1, &self.tex.id) }
    }
}
struct History {
    textures: Vec<Texture>,
    next: usize,
    frame: Option<u32>,
    pending: Vec<u8>,
}
impl Drop for History {
    fn drop(&mut self) {
        unsafe {
            for t in &self.textures {
                gl::DeleteTextures(1, &t.id)
            }
        }
    }
}

struct Caps {
    es: bool,
    npot: bool,
    float: bool,
    float_linear: bool,
    srgb: bool,
    units: usize,
    max_size: u32,
}
impl Caps {
    fn detect() -> Self {
        let es = crate::gl::es();
        let extensions = if es {
            unsafe {
                let p = gl::GetString(gl::EXTENSIONS);
                if p.is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(p.cast())
                        .to_string_lossy()
                        .into_owned()
                }
            }
        } else {
            String::new()
        };
        let has = |s: &str| extensions.split_whitespace().any(|e| e == s);
        let (mut units, mut max_size) = (0, 0);
        unsafe {
            gl::GetIntegerv(gl::MAX_TEXTURE_IMAGE_UNITS, &mut units);
            gl::GetIntegerv(gl::MAX_TEXTURE_SIZE, &mut max_size);
        }
        Self {
            es,
            npot: !es || has("GL_OES_texture_npot"),
            float: !es
                || (has("GL_OES_texture_half_float") && has("GL_EXT_color_buffer_half_float")),
            float_linear: !es || has("GL_OES_texture_half_float_linear"),
            srgb: !es || has("GL_EXT_sRGB"),
            units: units.max(0) as usize,
            max_size: max_size.max(0) as u32,
        }
    }
    fn linear(&self, requested: bool, floating: bool) -> bool {
        requested && (!floating || self.float_linear)
    }
    fn mipmap(&self, size: [u32; 2]) -> bool {
        self.npot || size.iter().all(|n| n.is_power_of_two())
    }
    fn wrap(&self, mode: Wrap, size: [u32; 2]) -> u32 {
        match mode {
            Wrap::Border if !self.es => gl::CLAMP_TO_BORDER,
            Wrap::Repeat if self.mipmap(size) => gl::REPEAT,
            Wrap::Mirror if self.mipmap(size) => gl::MIRRORED_REPEAT,
            _ => gl::CLAMP_TO_EDGE,
        }
    }
}
unsafe fn sampling(size: [u32; 2], linear: bool, wrap: Wrap, mipmap: bool, caps: &Caps) {
    let mipmap = mipmap && caps.mipmap(size);
    let filter = if linear { gl::LINEAR } else { gl::NEAREST };
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, filter as i32);
    gl::TexParameteri(
        gl::TEXTURE_2D,
        gl::TEXTURE_MIN_FILTER,
        if mipmap {
            if linear {
                gl::LINEAR_MIPMAP_LINEAR
            } else {
                gl::NEAREST_MIPMAP_NEAREST
            }
        } else {
            filter
        } as i32,
    );
    let wrap = caps.wrap(wrap, size);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, wrap as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, wrap as i32);
    if mipmap {
        gl::GenerateMipmap(gl::TEXTURE_2D);
    }
}

pub struct RetroShader {
    pub profile: preset::Profile,
    passes: Vec<Pass>,
    luts: Vec<Lut>,
    parameters: Vec<Parameter>,
    caps: Caps,
    vbo: u32,
    vao: u32,
    copy: Program,
    history: RefCell<History>,
    rewinding: bool,
}
const COPY: &str = r#"
#if defined(VERTEX)
#if __VERSION__ >= 130
#define attribute in
#define varying out
#endif
attribute vec4 VertexCoord; attribute vec4 TexCoord; varying vec2 uv; uniform mat4 MVPMatrix;
void main(){ gl_Position=MVPMatrix*VertexCoord;uv=TexCoord.xy; }
#elif defined(FRAGMENT)
#if __VERSION__ >= 130
#define varying in
#define texture2D texture
out vec4 colour;
#define gl_FragColor colour
#endif
#ifdef GL_ES
precision mediump float;
#endif
varying vec2 uv; uniform sampler2D Texture;
void main(){gl_FragColor=texture2D(Texture,uv);}
#endif
"#;
impl RetroShader {
    pub fn new(src: &str) -> Result<Self, GfxError> {
        let spec = PassSpec {
            path: Default::default(),
            linear: wants_linear(src),
            wrap: Wrap::Edge,
            scale: None,
            frame_mod: 0,
            alias: String::new(),
            float: false,
            srgb: false,
            mipmap: false,
        };
        Self::build(
            Preset {
                passes: vec![spec],
                ..Default::default()
            },
            vec![src.into()],
            &BTreeMap::new(),
        )
    }
    pub fn load(path: &Path, saved: &BTreeMap<String, f32>) -> Result<Self, GfxError> {
        let read =
            |p: &Path| std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
        let mut preset = preset::resolve_preset(path, &read).map_err(error)?;
        let sources = preset
            .passes
            .iter()
            .map(|p| read(&p.path))
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("glsl"))
        {
            preset.passes[0].linear = wants_linear(&sources[0]);
        }
        Self::build(preset, sources, saved)
    }
    fn build(
        preset: Preset,
        sources: Vec<String>,
        saved: &BTreeMap<String, f32>,
    ) -> Result<Self, GfxError> {
        let caps = Caps::detect();
        let _state = unsafe { State::save(caps.units) };
        let parameters = preset::collect_parameters(
            sources.iter().map(String::as_str),
            &preset.overrides,
            saved,
        )
        .map_err(error)?;
        let mut passes = Vec::new();
        for (spec, src) in preset.passes.into_iter().zip(&sources) {
            if spec.float && !caps.float {
                eprintln!(
                    "slot: shader: {}: float framebuffer unsupported, using RGBA8",
                    spec.path.display()
                );
            }
            if spec.srgb && !caps.srgb {
                eprintln!(
                    "slot: shader: {}: sRGB framebuffer unsupported, using RGBA8",
                    spec.path.display()
                );
            }
            let program =
                Program::new(src).map_err(|e| error(format!("{}: {e}", spec.path.display())))?;
            passes.push(Pass {
                program,
                spec,
                target: RefCell::new(None),
            });
        }
        let mut luts = Vec::new();
        for spec in preset.textures {
            let image = crate::png_image::load(&spec.path).map_err(error)?;
            let (width, height) = (image.width, image.height);
            if width > caps.max_size || height > caps.max_size {
                return Err(error("LUT exceeds GL_MAX_TEXTURE_SIZE"));
            }
            let id = crate::gl::texture(
                width,
                height,
                gl::NEAREST,
                gl::CLAMP_TO_EDGE,
                gl::RGBA,
                Some(&image.rgba),
            );
            unsafe {
                sampling([width, height], spec.linear, spec.wrap, spec.mipmap, &caps);
            }
            luts.push(Lut {
                tex: Texture::full(id, [width, height]),
                spec,
            });
        }
        let mut history_len = 0;
        for pass in &passes {
            for name in pass
                .program
                .uniforms
                .keys()
                .chain(pass.program.coords.keys())
            {
                for n in 0..7 {
                    let prefix = if n == 0 {
                        "Prev".into()
                    } else {
                        format!("Prev{n}")
                    };
                    if name.starts_with(&prefix) {
                        history_len = history_len.max(n + 1);
                    }
                }
            }
        }
        let black = vec![0; (SRC_W * SRC_H * 4) as usize];
        let mut history = History {
            textures: Vec::new(),
            next: 0,
            frame: None,
            pending: Vec::new(),
        };
        if history_len > 0 {
            for _ in 0..history_len {
                let id = crate::gl::texture(
                    SRC_W,
                    SRC_H,
                    gl::NEAREST,
                    gl::CLAMP_TO_EDGE,
                    gl::BGRA,
                    Some(&black),
                );
                history.textures.push(Texture::full(id, [SRC_W, SRC_H]));
            }
        }
        let copy = Program::new(COPY)?;
        let (mut vbo, mut vao) = (0, 0);
        unsafe {
            gl::GenBuffers(1, &mut vbo);
            if !caps.es {
                gl::GenVertexArrays(1, &mut vao);
            }
        }
        let shader = Self {
            profile: preset.profile,
            passes,
            luts,
            parameters,
            caps,
            vbo,
            vao,
            copy,
            history: RefCell::new(history),
            rewinding: false,
        };
        shader.resize([SRC_W, SRC_H], [OUT_W, OUT_H])?;
        shader.validate_bindings()?;
        Ok(shader)
    }
    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }
    pub fn set_rewinding(&mut self, rewinding: bool) {
        self.rewinding = rewinding;
    }
    pub fn set_parameter(&mut self, name: &str, value: f32) {
        if let Some(p) = self.parameters.iter_mut().find(|p| p.name == name) {
            p.set(value);
        }
    }
    fn resize(&self, source: [u32; 2], viewport: [u32; 2]) -> Result<(), GfxError> {
        let mut input = source;
        for (i, pass) in self.passes.iter().enumerate() {
            let last = i + 1 == self.passes.len();
            let size =
                preset::output_size(pass.spec.scale, input, viewport, last).map_err(error)?;
            if !last || pass.spec.scale.is_some() {
                let mut target = pass.target.borrow_mut();
                if target.as_ref().is_none_or(|t| t.tex.size != size) {
                    *target = Some(Target::new(size, &pass.spec, &self.caps)?);
                }
            }
            input = size;
        }
        Ok(())
    }
    fn bindings(
        &self,
        index: usize,
        orig: Texture,
        prev: Texture,
        outputs: &[Texture],
    ) -> HashMap<String, Texture> {
        debug_assert_eq!(index, outputs.len());
        let history = self.history.borrow();
        texture_bindings(
            orig,
            prev,
            outputs,
            self.passes.iter().map(|p| p.spec.alias.as_str()),
            SourceHistory {
                textures: &history.textures,
                next: history.next,
                ready: history.frame.is_some(),
            },
        )
    }
    fn validate_bindings(&self) -> Result<(), GfxError> {
        let orig = Texture::full(u32::MAX, [SRC_W, SRC_H]);
        let mut outputs = Vec::new();
        for (index, p) in self.passes.iter().enumerate() {
            let b = self.bindings(
                index,
                orig,
                Texture::full(u32::MAX - 1, [SRC_W, SRC_H]),
                &outputs,
            );
            let mut textures = std::collections::HashSet::new();
            for name in &p.program.samplers {
                let texture = self
                    .luts
                    .iter()
                    .find(|l| l.spec.name == *name)
                    .map(|l| l.tex)
                    .or_else(|| b.get(name.strip_suffix("Texture").unwrap_or(name)).copied())
                    .ok_or_else(|| {
                        error(format!(
                            "unsupported or forward sampler {name} in {}",
                            p.spec.path.display()
                        ))
                    })?;
                textures.insert(texture.id);
            }
            if textures.len() > self.caps.units {
                return Err(error(format!(
                    "{} needs {} texture units, driver supports {}",
                    p.spec.path.display(),
                    textures.len(),
                    self.caps.units
                )));
            }
            outputs.push(p.target.borrow().as_ref().map(|t| t.tex).unwrap_or(orig));
        }
        Ok(())
    }
    pub fn draw(
        &self,
        tex: u32,
        prev: u32,
        rect: (f32, f32, f32, f32),
        src: [f32; 4],
        frame: u32,
    ) -> Result<(), GfxError> {
        let mut state = unsafe { State::save(self.caps.units) };
        let viewport = [
            rect.2.round().max(1.0) as u32,
            rect.3.round().max(1.0) as u32,
        ];
        let input = [
            (src[2] * SRC_W as f32).round() as u32,
            (src[3] * SRC_H as f32).round() as u32,
        ];
        self.resize(input, viewport)?;
        let orig = Texture {
            id: tex,
            size: [SRC_W, SRC_H],
            input,
            uv: src,
            floating: false,
        };
        let previous = Texture { id: prev, ..orig };
        let mut outputs = Vec::new();
        unsafe {
            gl::Disable(gl::BLEND);
            if !self.caps.es {
                gl::Disable(gl::FRAMEBUFFER_SRGB);
            }
        }
        for (index, pass) in self.passes.iter().enumerate() {
            let bindings = self.bindings(index, orig, previous, &outputs);
            let target = pass.target.borrow();
            let (output, mvp) = if let Some(t) = target.as_ref() {
                unsafe {
                    gl::BindFramebuffer(gl::FRAMEBUFFER, t.fbo);
                    gl::Viewport(0, 0, t.tex.size[0] as i32, t.tex.size[1] as i32);
                }
                (
                    t.tex.size.map(|n| n as f32),
                    matrix(
                        (0.0, 0.0, t.tex.size[0] as f32, t.tex.size[1] as f32),
                        t.tex.size,
                        true,
                    ),
                )
            } else {
                unsafe {
                    gl::BindFramebuffer(gl::FRAMEBUFFER, state.fbo as u32);
                    gl::Viewport(
                        state.viewport[0],
                        state.viewport[1],
                        state.viewport[2],
                        state.viewport[3],
                    );
                }
                ([rect.2, rect.3], matrix(rect, [OUT_W, OUT_H], false))
            };
            unsafe {
                if !self.caps.es {
                    if pass.spec.srgb && self.caps.srgb {
                        gl::Enable(gl::FRAMEBUFFER_SRGB)
                    } else {
                        gl::Disable(gl::FRAMEBUFFER_SRGB)
                    }
                }
            }
            self.render(
                &pass.program,
                &pass.spec,
                &bindings,
                output,
                &mvp,
                frame,
                &mut state,
            )?;
            if let Some(t) = target.as_ref() {
                outputs.push(t.tex);
            }
        }
        if self.passes.last().is_some_and(|p| p.spec.scale.is_some()) {
            unsafe {
                gl::BindFramebuffer(gl::FRAMEBUFFER, state.fbo as u32);
                gl::Viewport(
                    state.viewport[0],
                    state.viewport[1],
                    state.viewport[2],
                    state.viewport[3],
                );
                if !self.caps.es {
                    gl::Disable(gl::FRAMEBUFFER_SRGB);
                }
            }
            let t = *outputs
                .last()
                .ok_or_else(|| error("missing final shader output"))?;
            let spec = &self
                .passes
                .last()
                .ok_or_else(|| error("empty shader"))?
                .spec;
            self.render(
                &self.copy,
                spec,
                &HashMap::from([("".into(), t)]),
                [rect.2, rect.3],
                &matrix(rect, [OUT_W, OUT_H], false),
                frame,
                &mut state,
            )?;
        }
        Ok(())
    }
    pub fn capture_frame(&self, bytes: &[u8], frame: u32) {
        let len = (SRC_W * SRC_H * 4) as usize;
        if bytes.len() < len || self.history.borrow().textures.is_empty() {
            return;
        }
        let _state = unsafe { State::save(self.caps.units) };
        let mut history = self.history.borrow_mut();
        // Keep the current upload until it becomes a previous source frame.
        if !history.pending.is_empty() {
            unsafe {
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, history.textures[history.next].id);
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    0,
                    0,
                    SRC_W as i32,
                    SRC_H as i32,
                    gl::BGRA,
                    gl::UNSIGNED_BYTE,
                    history.pending.as_ptr().cast(),
                );
            }
            history.next = (history.next + 1) % history.textures.len();
            history.frame = Some(frame);
        }
        history.pending.clear();
        history.pending.extend_from_slice(&bytes[..len]);
    }
    #[allow(clippy::too_many_arguments)]
    fn render(
        &self,
        p: &Program,
        spec: &PassSpec,
        bindings: &HashMap<String, Texture>,
        output: [f32; 2],
        mvp: &[f32; 16],
        frame: u32,
        state: &mut State,
    ) -> Result<(), GfxError> {
        unsafe {
            gl::UseProgram(p.id);
            if p.mvp >= 0 {
                gl::UniformMatrix4fv(p.mvp, 1, gl::FALSE, mvp.as_ptr());
            }
            p.uniform("OutputSize").set_vec2(output[0], output[1]);
            p.uniform("FrameCount")
                .set_count(frame_count(frame, spec.frame_mod));
            p.uniform("FrameDirection").set_direction(self.rewinding);
            for param in &self.parameters {
                let u = p.uniform(&param.name);
                if u.loc >= 0 {
                    gl::Uniform1f(u.loc, param.value);
                }
            }
            for (prefix, t) in bindings {
                let (input, texture) = t.uniform_sizes();
                p.uniform(&format!("{prefix}InputSize"))
                    .set_vec2(input[0], input[1]);
                p.uniform(&format!("{prefix}TextureSize"))
                    .set_vec2(texture[0], texture[1]);
            }
            let mut units = HashMap::new();
            for name in &p.samplers {
                let lut = self.luts.iter().find(|l| l.spec.name == *name);
                let t = if let Some(l) = lut {
                    l.tex
                } else {
                    *bindings
                        .get(name.strip_suffix("Texture").unwrap_or(name))
                        .ok_or_else(|| error(format!("missing sampler {name}")))?
                };
                let next = units.len();
                let unit = *units.entry(t.id).or_insert(next);
                if unit >= self.caps.units {
                    return Err(error("shader texture unit budget exceeded"));
                }
                gl::ActiveTexture(gl::TEXTURE0 + unit as u32);
                gl::BindTexture(gl::TEXTURE_2D, t.id);
                state.remember_texture(t.id);
                if let Some(l) = lut {
                    sampling(
                        t.size,
                        l.spec.linear,
                        l.spec.wrap,
                        l.spec.mipmap,
                        &self.caps,
                    )
                } else {
                    sampling(
                        t.size,
                        self.caps.linear(spec.linear, t.floating),
                        spec.wrap,
                        spec.mipmap,
                        &self.caps,
                    )
                };
                gl::Uniform1i(p.uniform(name).loc, unit as i32);
            }
            if !self.caps.es {
                gl::BindVertexArray(self.vao);
            }
            if p.colour >= 0 {
                let loc = p.colour as u32;
                state.remember_attribute(loc);
                gl::DisableVertexAttribArray(loc);
                gl::VertexAttrib4f(loc, 1.0, 1.0, 1.0, 1.0);
            }
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            let mut data = VERTS.to_vec();
            let mut arrays = vec![(0u32, 0usize)];
            for (name, loc) in &p.coords {
                let prefix = if name == "TexCoord" {
                    ""
                } else {
                    name.strip_suffix("TexCoord").unwrap_or("")
                };
                let t = bindings.get(prefix).or_else(|| bindings.get(""));
                if let Some(t) = t {
                    arrays.push((*loc, data.len() * 4));
                    data.extend(texture_coords(t.uv));
                }
            }
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (data.len() * 4) as isize,
                data.as_ptr().cast(),
                gl::DYNAMIC_DRAW,
            );
            for (loc, offset) in &arrays {
                gl::EnableVertexAttribArray(*loc);
                gl::VertexAttribPointer(
                    *loc,
                    2,
                    gl::FLOAT,
                    gl::FALSE,
                    0,
                    *offset as *const std::ffi::c_void,
                );
            }
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
            if self.caps.es {
                for (loc, _) in arrays {
                    gl::DisableVertexAttribArray(loc);
                }
            }
        }
        Ok(())
    }
}
impl Drop for RetroShader {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            if self.vao != 0 {
                gl::DeleteVertexArrays(1, &self.vao);
            }
        }
    }
}
fn matrix(rect: (f32, f32, f32, f32), size: [u32; 2], fbo: bool) -> [f32; 16] {
    let (x, y, w, h) = rect;
    let sx = 2.0 * w / size[0] as f32;
    let sy = 2.0 * h / size[1] as f32;
    let ox = 2.0 * x / size[0] as f32 - 1.0;
    let oy = 2.0 * y / size[1] as f32 - 1.0;
    // FBO row zero is the source's top row. The final panel mapping flips it once.
    [
        sx,
        0.0,
        0.0,
        0.0,
        0.0,
        if fbo { sy } else { -sy },
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        ox,
        if fbo { oy } else { -oy },
        0.0,
        1.0,
    ]
}
fn texture_coords(src: [f32; 4]) -> [f32; 8] {
    let [x, y, w, h] = src;
    [x, y, x + w, y, x, y + h, x + w, y + h]
}

struct Attrib {
    loc: u32,
    enabled: i32,
    size: i32,
    kind: i32,
    normalized: i32,
    stride: i32,
    buffer: i32,
    pointer: *mut std::ffi::c_void,
}
struct State {
    fbo: i32,
    viewport: [i32; 4],
    program: i32,
    active: i32,
    buffer: i32,
    vao: i32,
    blend: bool,
    srgb: bool,
    clear: [f32; 4],
    unpack: i32,
    textures: Vec<i32>,
    sampling: HashMap<u32, [i32; 4]>,
    attribs: Vec<Attrib>,
    constants: HashMap<u32, [f32; 4]>,
}
impl State {
    unsafe fn save(units: usize) -> Self {
        let mut s = Self {
            fbo: 0,
            viewport: [0; 4],
            program: 0,
            active: 0,
            buffer: 0,
            vao: 0,
            blend: gl::IsEnabled(gl::BLEND) != 0,
            srgb: false,
            clear: [0.0; 4],
            unpack: 0,
            textures: Vec::new(),
            sampling: HashMap::new(),
            attribs: Vec::new(),
            constants: HashMap::new(),
        };
        for (key, v) in [
            (gl::FRAMEBUFFER_BINDING, &mut s.fbo),
            (gl::CURRENT_PROGRAM, &mut s.program),
            (gl::ACTIVE_TEXTURE, &mut s.active),
            (gl::ARRAY_BUFFER_BINDING, &mut s.buffer),
            (gl::UNPACK_ALIGNMENT, &mut s.unpack),
        ] {
            gl::GetIntegerv(key, v);
        }
        gl::GetIntegerv(gl::VIEWPORT, s.viewport.as_mut_ptr());
        gl::GetFloatv(gl::COLOR_CLEAR_VALUE, s.clear.as_mut_ptr());
        if !crate::gl::es() {
            gl::GetIntegerv(gl::VERTEX_ARRAY_BINDING, &mut s.vao);
            s.srgb = gl::IsEnabled(gl::FRAMEBUFFER_SRGB) != 0;
        } else {
            let mut count = 0;
            gl::GetIntegerv(gl::MAX_VERTEX_ATTRIBS, &mut count);
            for loc in 0..count as u32 {
                let mut a = Attrib {
                    loc,
                    enabled: 0,
                    size: 0,
                    kind: 0,
                    normalized: 0,
                    stride: 0,
                    buffer: 0,
                    pointer: std::ptr::null_mut(),
                };
                for (key, v) in [
                    (gl::VERTEX_ATTRIB_ARRAY_ENABLED, &mut a.enabled),
                    (gl::VERTEX_ATTRIB_ARRAY_SIZE, &mut a.size),
                    (gl::VERTEX_ATTRIB_ARRAY_TYPE, &mut a.kind),
                    (gl::VERTEX_ATTRIB_ARRAY_NORMALIZED, &mut a.normalized),
                    (gl::VERTEX_ATTRIB_ARRAY_STRIDE, &mut a.stride),
                    (gl::VERTEX_ATTRIB_ARRAY_BUFFER_BINDING, &mut a.buffer),
                ] {
                    gl::GetVertexAttribiv(loc, key, v);
                }
                gl::GetVertexAttribPointerv(
                    loc,
                    gl::VERTEX_ATTRIB_ARRAY_POINTER,
                    std::ptr::addr_of_mut!(a.pointer),
                );
                s.attribs.push(a);
            }
        }
        for unit in 0..units {
            gl::ActiveTexture(gl::TEXTURE0 + unit as u32);
            let mut id = 0;
            gl::GetIntegerv(gl::TEXTURE_BINDING_2D, &mut id);
            s.textures.push(id);
        }
        gl::ActiveTexture(s.active as u32);
        s
    }
    unsafe fn remember_attribute(&mut self, loc: u32) {
        self.constants.entry(loc).or_insert_with(|| {
            let mut value = [0.0; 4];
            gl::GetVertexAttribfv(loc, gl::CURRENT_VERTEX_ATTRIB, value.as_mut_ptr());
            value
        });
    }
    unsafe fn remember_texture(&mut self, id: u32) {
        self.sampling.entry(id).or_insert_with(|| {
            let mut params = [0; 4];
            for (i, key) in [
                gl::TEXTURE_MIN_FILTER,
                gl::TEXTURE_MAG_FILTER,
                gl::TEXTURE_WRAP_S,
                gl::TEXTURE_WRAP_T,
            ]
            .iter()
            .enumerate()
            {
                gl::GetTexParameteriv(gl::TEXTURE_2D, *key, &mut params[i]);
            }
            params
        });
    }
}
impl Drop for State {
    fn drop(&mut self) {
        unsafe {
            for (id, params) in &self.sampling {
                gl::BindTexture(gl::TEXTURE_2D, *id);
                for (key, value) in [
                    gl::TEXTURE_MIN_FILTER,
                    gl::TEXTURE_MAG_FILTER,
                    gl::TEXTURE_WRAP_S,
                    gl::TEXTURE_WRAP_T,
                ]
                .iter()
                .zip(params)
                {
                    gl::TexParameteri(gl::TEXTURE_2D, *key, *value);
                }
            }
            for (unit, id) in self.textures.iter().enumerate() {
                gl::ActiveTexture(gl::TEXTURE0 + unit as u32);
                gl::BindTexture(gl::TEXTURE_2D, *id as u32);
            }
            gl::ActiveTexture(self.active as u32);
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo as u32);
            gl::Viewport(
                self.viewport[0],
                self.viewport[1],
                self.viewport[2],
                self.viewport[3],
            );
            gl::UseProgram(self.program as u32);
            if self.blend {
                gl::Enable(gl::BLEND)
            } else {
                gl::Disable(gl::BLEND)
            };
            if !crate::gl::es() {
                gl::BindVertexArray(self.vao as u32);
                if self.srgb {
                    gl::Enable(gl::FRAMEBUFFER_SRGB)
                } else {
                    gl::Disable(gl::FRAMEBUFFER_SRGB)
                }
            } else {
                for a in &self.attribs {
                    gl::BindBuffer(gl::ARRAY_BUFFER, a.buffer as u32);
                    gl::VertexAttribPointer(
                        a.loc,
                        a.size,
                        a.kind as u32,
                        a.normalized as u8,
                        a.stride,
                        a.pointer,
                    );
                    if a.enabled != 0 {
                        gl::EnableVertexAttribArray(a.loc)
                    } else {
                        gl::DisableVertexAttribArray(a.loc)
                    }
                }
            }
            for (loc, value) in &self.constants {
                gl::VertexAttrib4fv(*loc, value.as_ptr());
            }
            gl::BindBuffer(gl::ARRAY_BUFFER, self.buffer as u32);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, self.unpack);
            gl::ClearColor(self.clear[0], self.clear[1], self.clear[2], self.clear[3]);
        }
    }
}
/// Every active uniform the program kept after linking, and its type.
unsafe fn uniform_kinds(prog: gl::types::GLuint) -> HashMap<String, gl::types::GLenum> {
    let mut out = HashMap::new();
    let mut count = 0;
    gl::GetProgramiv(prog, gl::ACTIVE_UNIFORMS, &mut count);
    let mut name = vec![0u8; 256];
    for i in 0..count.max(0) as gl::types::GLuint {
        let (mut len, mut size, mut kind) = (0, 0, 0);
        gl::GetActiveUniform(
            prog,
            i,
            name.len() as gl::types::GLsizei,
            &mut len,
            &mut size,
            &mut kind,
            name.as_mut_ptr() as *mut gl::types::GLchar,
        );
        let len = (len.max(0) as usize).min(name.len());
        let key = String::from_utf8_lossy(&name[..len]).into_owned();
        // An array reports its first element; nothing set here is an array, but the name is
        // what `GetUniformLocation` would be asked for.
        let key = key.strip_suffix("[0]").map(str::to_string).unwrap_or(key);
        out.insert(key, kind);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_uniforms_preserve_the_modulus_and_signed_rewind_direction() {
        assert_eq!(frame_count(19, 0), 19);
        assert_eq!(frame_count(19, 8), 3);
        assert_eq!(frame_count(u32::MAX, 2), 1);
        assert_eq!(frame_direction(false), 1);
        assert_eq!(frame_direction(true), -1);
    }

    #[test]
    fn every_pass_keeps_original_sizes_and_relative_pass_bindings() {
        for (input, uv) in [
            ([240, 160], [0.0, 0.0, 1.0, 1.0]),
            (
                [160, 144],
                [40.0 / 240.0, 8.0 / 160.0, 160.0 / 240.0, 144.0 / 160.0],
            ),
        ] {
            let orig = Texture {
                input,
                uv,
                ..Texture::full(1, [240, 160])
            };
            let prev = Texture { id: 2, ..orig };
            let outputs = [Texture::full(3, [720, 480]), Texture::full(4, [720, 480])];
            for index in 0..=outputs.len() {
                let b = texture_bindings(
                    orig,
                    prev,
                    &outputs[..index],
                    ["Mask", "Colour"].into_iter(),
                    SourceHistory {
                        textures: &[],
                        next: 0,
                        ready: false,
                    },
                );
                let source = b[""];
                assert_eq!(
                    source.id,
                    if index == 0 {
                        orig.id
                    } else {
                        outputs[index - 1].id
                    }
                );
                assert_eq!(
                    source.uniform_sizes(),
                    if index == 0 {
                        orig.uniform_sizes()
                    } else {
                        ([720.0, 480.0], [720.0, 480.0])
                    }
                );
                assert_eq!(
                    b["Orig"].uniform_sizes(),
                    (input.map(|n| n as f32), [240.0, 160.0])
                );
                assert_eq!(b["Orig"].uv, uv);
                assert_eq!(b[&format!("PassPrev{}", index + 1)].id, orig.id);
                for (i, alias) in ["Mask", "Colour"].into_iter().take(index).enumerate() {
                    assert_eq!(b[&format!("Pass{}", i + 1)].id, outputs[i].id);
                    assert_eq!(b[&format!("PassPrev{}", index - i)].id, outputs[i].id);
                    assert_eq!(b[alias].uv, [0.0, 0.0, 1.0, 1.0]);
                }
            }
        }
    }

    #[test]
    fn history_is_previous_source_frames_even_after_an_upscaling_pass() {
        let orig = Texture::full(1, [240, 160]);
        let prev = Texture::full(2, [240, 160]);
        let history = (10..17)
            .map(|id| Texture::full(id, [240, 160]))
            .collect::<Vec<_>>();
        let outputs = [Texture::full(3, [720, 480])];
        for next in 0..7 {
            let b = texture_bindings(
                orig,
                prev,
                &outputs,
                [""].into_iter(),
                SourceHistory {
                    textures: &history,
                    next,
                    ready: true,
                },
            );
            for n in 0..7 {
                let name = if n == 0 {
                    "Prev".into()
                } else {
                    format!("Prev{n}")
                };
                assert_eq!(b[&name].id, history[(next + 6 - n) % 7].id);
                assert_eq!(b[&name].uniform_sizes(), ([240.0, 160.0], [240.0, 160.0]));
            }
        }
        let b = texture_bindings(
            orig,
            orig,
            &outputs,
            [""].into_iter(),
            SourceHistory {
                textures: &history,
                next: 0,
                ready: true,
            },
        );
        assert!((0..7).all(|n| b[&if n == 0 {
            "Prev".into()
        } else {
            format!("Prev{n}")
        }]
            .id
            == orig.id));
    }

    #[test]
    fn framebuffer_and_panel_matrices_flip_only_the_final_output() {
        let size = [720, 480];
        for fbo in [false, true] {
            let m = matrix((0.0, 0.0, 720.0, 480.0), size, fbo);
            for (x, y) in [(0.0, 0.0), (1.0, 1.0), (0.5, 0.5)] {
                assert_eq!(m[0] * x + m[12], 2.0 * x - 1.0);
                assert_eq!(
                    m[5] * y + m[13],
                    if fbo { 2.0 * y - 1.0 } else { 1.0 - 2.0 * y }
                );
            }
        }
    }

    #[test]
    fn texture_coordinates_cover_only_the_source_rectangle() {
        assert_eq!(texture_coords([0.0, 0.0, 1.0, 1.0]), VERTS);
        let crop = [40.0 / 240.0, 8.0 / 160.0, 160.0 / 240.0, 144.0 / 160.0];
        let coords = texture_coords(crop);
        assert_eq!(coords[0], crop[0]);
        assert_eq!(coords[1], crop[1]);
        assert!((coords[6] - 200.0 / 240.0).abs() < 0.00001);
        assert!((coords[7] - 152.0 / 160.0).abs() < 0.00001);
        assert_eq!(
            (crop[2] * SRC_W as f32, crop[3] * SRC_H as f32),
            (160.0, 144.0)
        );
    }

    const SHADER: &str = "// header\n#version 130\n#pragma slot_filter linear\nvoid main() {}\n";

    #[test]
    fn the_device_gets_no_version_line_at_all() {
        let s = stage_source(SHADER, "FRAGMENT", true);
        assert!(!s.contains("#version"));
        assert!(s.starts_with("#define FRAGMENT\n"));
    }

    #[test]
    fn the_host_gets_its_own_version_first_and_the_files_own_is_blanked() {
        let s = stage_source(SHADER, "VERTEX", false);
        assert!(s.starts_with("#version 330 core\n#define VERTEX\n"));
        assert_eq!(s.matches("#version").count(), 1);
    }

    #[test]
    fn parameter_pragmas_are_blanked_without_moving_source_lines() {
        let source = "// \"header\"\n#version 130\n  #pragma parameter X \"Label\" 0.5 0.0 1.0 0.1\n\t#pragma parameter Y \"Other label\" 0.5 0.0 1.0 0.1\n#pragma slot_filter linear\n#ifdef PARAMETER_UNIFORM\nuniform float X;\nuniform float Y;\n#else\n#define X 0.5\n#define Y 0.5\n#endif\nvoid main() {}\n";
        for newline in ["\n", "\r\n"] {
            for trailing_newline in [false, true] {
                let source = source.trim_end_matches('\n').replace('\n', newline)
                    + if trailing_newline { newline } else { "" };
                for es in [true, false] {
                    for stage in ["VERTEX", "FRAGMENT"] {
                        let assembled = stage_source(&source, stage, es);
                        let prefix_lines = if es { 3 } else { 5 };
                        assert_eq!(
                            assembled.lines().count(),
                            source.lines().count() + prefix_lines
                        );
                        assert!(!assembled.contains('"'));
                        let (_, body) = assembled.split_once("#line 1\n").unwrap();
                        let lines = body.lines().collect::<Vec<_>>();
                        assert_eq!(lines[0], "//  header ");
                        assert_eq!(lines[1], "");
                        assert_eq!(lines[2], "");
                        assert_eq!(lines[3], "");
                        assert_eq!(lines[4], LINEAR_PRAGMA);
                        assert_eq!(lines[6], "uniform float X;");
                        assert_eq!(lines[7], "uniform float Y;");
                        assert_eq!(lines[9], "#define X 0.5");
                        assert_eq!(lines[10], "#define Y 0.5");
                        assert_eq!(lines[12], "void main() {}");
                    }
                }
                let parameters = preset::collect_parameters(
                    [source.as_str()],
                    &BTreeMap::new(),
                    &BTreeMap::new(),
                )
                .unwrap();
                assert_eq!(parameters.len(), 2);
                assert_eq!(parameters[0].name, "X");
                assert_eq!(parameters[0].label, "Label");
                assert_eq!(parameters[0].value, 0.5);
                assert_eq!(parameters[1].name, "Y");
                assert_eq!(parameters[1].label, "Other label");
            }
        }
    }

    #[test]
    fn the_linear_pragma_is_read_and_its_absence_is_nearest() {
        assert!(wants_linear(SHADER));
        assert!(!wants_linear("void main() {}"));
    }
}

#[cfg(test)]
mod caps_tests {
    use super::*;
    #[test]
    fn gles_npot_wrapping_and_mipmaps_require_the_extension() {
        let mut caps = Caps {
            es: true,
            npot: false,
            float: false,
            float_linear: false,
            srgb: false,
            units: 8,
            max_size: 4096,
        };
        assert_eq!(caps.wrap(Wrap::Repeat, [720, 480]), gl::CLAMP_TO_EDGE);
        assert_eq!(caps.wrap(Wrap::Mirror, [240, 160]), gl::CLAMP_TO_EDGE);
        assert_eq!(caps.wrap(Wrap::Border, [256, 128]), gl::CLAMP_TO_EDGE);
        assert_eq!(caps.wrap(Wrap::Repeat, [256, 128]), gl::REPEAT);
        assert!(!caps.linear(true, true));
        assert!(caps.linear(true, false));
        assert!(!caps.mipmap([240, 160]));
        assert!(caps.mipmap([256, 128]));
        caps.npot = true;
        assert_eq!(caps.wrap(Wrap::Mirror, [720, 480]), gl::MIRRORED_REPEAT);
        assert!(caps.mipmap([720, 480]));
    }
}
