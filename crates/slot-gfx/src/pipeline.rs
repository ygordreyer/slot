//! Game texture passes for built-in looks and custom shaders.

use crate::lcd3x::mask_texture_rgba8;
use crate::power::{screen_brightness, screen_rect};
use crate::quad::Quad;
use crate::retroshader::RetroShader;
use crate::shaders::{GAME_FRAG, RECT_VERT};
use crate::surface::{GfxError, OUT_H, OUT_W};

pub const SCALE: u32 = 3;
pub const SRC_W: u32 = OUT_W / SCALE;
pub const SRC_H: u32 = OUT_H / SCALE;

pub const WHOLE_TEXTURE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// What the game layer is drawn through. The first two share slot's own program and differ
/// only in the mask it multiplies by.
pub enum Look {
    /// LCD3x collapsed to its 3x3 table. What slot has always shipped, and the default.
    Lcd,
    /// The frame alone, nearest neighbour at 3x: the mask is a single white texel.
    Plain,
    /// A RetroArch shader off the card.
    Retro(Box<RetroShader>),
}

pub struct GamePass {
    prog: gl::types::GLuint,
    game: gl::types::GLuint,
    /// The frame uploaded before `game`, for shaders that blend the two. The two names swap on
    /// every upload rather than copying pixels.
    prev: gl::types::GLuint,
    mask: gl::types::GLuint,
    /// One white texel, which turns `GAME_FRAG`'s multiply into a no-op for `Look::Plain`.
    white: gl::types::GLuint,
    look: Look,
    shader_error: Option<String>,
    /// Frames uploaded, for a shader's `FrameCount`.
    frames: u32,
    u_rect: gl::types::GLint,
    u_bright: gl::types::GLint,
    u_uv: gl::types::GLint,
    power: f32,
    src: [f32; 4],
    shader_src: [f32; 4],
    shader_actual: bool,
}

impl GamePass {
    pub fn new() -> Result<Self, GfxError> {
        let prog = crate::shaders::program(RECT_VERT, GAME_FRAG)?;
        // Both textures start black because the first upload swaps game into prev.
        let black = vec![0u8; (SRC_W * SRC_H * 4) as usize];
        let game = crate::gl::texture(
            SRC_W,
            SRC_H,
            gl::NEAREST,
            gl::CLAMP_TO_EDGE,
            gl::BGRA,
            Some(&black),
        );
        let prev = crate::gl::texture(
            SRC_W,
            SRC_H,
            gl::NEAREST,
            gl::CLAMP_TO_EDGE,
            gl::BGRA,
            Some(&black),
        );
        let white = crate::gl::texture(1, 1, gl::NEAREST, gl::REPEAT, gl::RGBA, Some(&[255u8; 4]));
        let mask = crate::gl::texture(
            3,
            3,
            gl::NEAREST,
            gl::REPEAT,
            gl::RGBA,
            Some(&mask_texture_rgba8()),
        );
        let (u_rect, u_bright, u_uv);
        unsafe {
            gl::UseProgram(prog);
            gl::Uniform1i(crate::gl::uniform_location(prog, "u_game"), 0);
            gl::Uniform1i(crate::gl::uniform_location(prog, "u_mask"), 1);
            gl::Uniform2f(
                crate::gl::uniform_location(prog, "u_src"),
                SRC_W as f32,
                SRC_H as f32,
            );
            gl::Uniform2f(
                crate::gl::uniform_location(prog, "u_target"),
                OUT_W as f32,
                OUT_H as f32,
            );
            u_rect = crate::gl::uniform_location(prog, "u_rect");
            u_bright = crate::gl::uniform_location(prog, "u_bright");
            u_uv = crate::gl::uniform_location(prog, "u_uv");
        }
        Ok(GamePass {
            prog,
            game,
            prev,
            mask,
            white,
            look: Look::Lcd,
            shader_error: None,
            frames: 0,
            u_rect,
            u_bright,
            u_uv,
            power: 1.0,
            src: WHOLE_TEXTURE,
            shader_src: WHOLE_TEXTURE,
            shader_actual: false,
        })
    }

    pub fn set_power(&mut self, t: f32) {
        self.power = t.clamp(0.0, 1.0);
    }

    pub fn set_source_rect(&mut self, rect: [f32; 4]) {
        self.src = rect;
        self.shader_src = rect;
        self.shader_actual = false;
    }

    /// Actual-size games occupy the source rectangle's fraction of the panel as well.
    pub fn set_shader_source(&mut self, rect: [f32; 4], actual: bool) {
        self.shader_src = rect;
        self.shader_actual = actual;
    }

    /// Replaces how the game layer is drawn, releasing the previous custom program.
    pub fn set_look(&mut self, look: Look) {
        self.look = look;
    }

    pub fn parameters(&self) -> &[crate::preset::Parameter] {
        match &self.look {
            Look::Retro(s) => s.parameters(),
            _ => &[],
        }
    }
    pub fn set_parameter(&mut self, name: &str, value: f32) {
        if let Look::Retro(s) = &mut self.look {
            s.set_parameter(name, value);
        }
    }
    pub fn take_shader_error(&mut self) -> Option<String> {
        self.shader_error.take()
    }

    pub fn upload(&mut self, xrgb8888: &[u8]) {
        if xrgb8888.len() < (SRC_W * SRC_H * 4) as usize {
            return;
        }
        if let Look::Retro(shader) = &self.look {
            shader.capture_frame(xrgb8888, self.frames);
        }
        // Last frame becomes `prev`, and its texture takes this one.
        std::mem::swap(&mut self.game, &mut self.prev);
        self.frames = self.frames.wrapping_add(1);
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.game);
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
                xrgb8888.as_ptr() as *const std::ffi::c_void,
            );
        }
    }

    pub fn draw(&mut self, quad: &Quad) {
        self.draw_source(self.game, self.prev, quad, self.src, true);
    }

    pub fn draw_still(&mut self, tex: gl::types::GLuint, quad: &Quad) {
        self.draw_source(tex, tex, quad, WHOLE_TEXTURE, false);
    }

    fn draw_source(
        &mut self,
        tex: gl::types::GLuint,
        prev: gl::types::GLuint,
        quad: &Quad,
        src: [f32; 4],
        live: bool,
    ) {
        let rect = screen_rect(self.power);
        let mask = match &self.look {
            Look::Lcd => self.mask,
            Look::Plain => self.white,
            Look::Retro(shader) => {
                let source = if live { self.shader_src } else { WHOLE_TEXTURE };
                let output = shader_rect(rect, source, live && self.shader_actual);
                match shader.draw(tex, prev, output, source, self.frames) {
                    Ok(()) => return,
                    Err(e) => {
                        self.shader_error = Some(e.to_string());
                        self.look = Look::Lcd;
                        self.mask
                    }
                }
            }
        };
        let (x, y, w, h) = rect;
        unsafe {
            gl::UseProgram(self.prog);
            gl::Uniform4f(self.u_rect, x, y, w, h);
            gl::Uniform4f(self.u_uv, src[0], src[1], src[2], src[3]);
            gl::Uniform1f(self.u_bright, screen_brightness(self.power));
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::ActiveTexture(gl::TEXTURE1);
            gl::BindTexture(gl::TEXTURE_2D, mask);
            gl::ActiveTexture(gl::TEXTURE0);
        }
        quad.draw();
    }
}

impl Drop for GamePass {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.game);
            gl::DeleteTextures(1, &self.prev);
            gl::DeleteTextures(1, &self.mask);
            gl::DeleteTextures(1, &self.white);
            gl::DeleteProgram(self.prog);
        }
    }
}

fn shader_rect(rect: (f32, f32, f32, f32), src: [f32; 4], actual: bool) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = rect;
    if actual {
        (x + w * src[0], y + h * src[1], w * src[2], h * src[3])
    } else {
        rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod mock_gl {
        use gl::types::*;
        use std::cell::RefCell;
        use std::collections::BTreeMap;
        use std::ffi::c_void;

        #[derive(Default)]
        struct Textures {
            next: GLuint,
            bound: GLuint,
            pixels: BTreeMap<GLuint, Vec<u8>>,
        }
        thread_local! {
            static TEXTURES: RefCell<Textures> = RefCell::new(Textures::default());
        }

        macro_rules! stub {
            ($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty, $value:expr)?) => {
                unsafe extern "system" fn $name($($arg: $ty),*) $(-> $ret)? {
                    $($value)?
                }
            };
        }
        stub!(create_shader(_kind: GLenum) -> GLuint, 1);
        stub!(create_program() -> GLuint, 1);
        stub!(shader_source(_id: GLuint, _n: GLsizei, _text: *const *const GLchar, _len: *const GLint));
        stub!(one_id(_id: GLuint));
        stub!(attach_shader(_program: GLuint, _shader: GLuint));
        stub!(bind_attrib(_program: GLuint, _index: GLuint, _name: *const GLchar));
        stub!(uniform_location(_program: GLuint, _name: *const GLchar) -> GLint, 0);
        stub!(uniform1i(_location: GLint, _value: GLint));
        stub!(uniform2f(_location: GLint, _x: GLfloat, _y: GLfloat));
        stub!(pixel_store(_key: GLenum, _value: GLint));
        stub!(tex_parameter(_target: GLenum, _key: GLenum, _value: GLint));
        stub!(delete_textures(_n: GLsizei, _ids: *const GLuint));

        unsafe extern "system" fn status(_id: GLuint, _key: GLenum, value: *mut GLint) {
            *value = 1;
        }
        unsafe extern "system" fn gen_textures(n: GLsizei, ids: *mut GLuint) {
            TEXTURES.with_borrow_mut(|textures| {
                for i in 0..n as usize {
                    textures.next += 1;
                    *ids.add(i) = textures.next;
                }
            });
        }
        unsafe extern "system" fn bind_texture(_target: GLenum, id: GLuint) {
            TEXTURES.with_borrow_mut(|textures| textures.bound = id);
        }
        #[allow(clippy::too_many_arguments)]
        unsafe extern "system" fn tex_image(
            _target: GLenum,
            _level: GLint,
            _internal: GLint,
            w: GLsizei,
            h: GLsizei,
            _border: GLint,
            _format: GLenum,
            _kind: GLenum,
            pixels: *const c_void,
        ) {
            // Poison unspecified allocations so zero-filled driver memory cannot hide the bug.
            let len = (w * h * 4) as usize;
            let data = if pixels.is_null() {
                vec![0xcd; len]
            } else {
                std::slice::from_raw_parts(pixels.cast::<u8>(), len).to_vec()
            };
            TEXTURES.with_borrow_mut(|textures| textures.pixels.insert(textures.bound, data));
        }
        #[allow(clippy::too_many_arguments)]
        unsafe extern "system" fn tex_sub_image(
            _target: GLenum,
            _level: GLint,
            _x: GLint,
            _y: GLint,
            w: GLsizei,
            h: GLsizei,
            _format: GLenum,
            _kind: GLenum,
            pixels: *const c_void,
        ) {
            let data =
                std::slice::from_raw_parts(pixels.cast::<u8>(), (w * h * 4) as usize).to_vec();
            TEXTURES.with_borrow_mut(|textures| textures.pixels.insert(textures.bound, data));
        }

        pub fn load() {
            gl::load_with(|name| match name {
                "glCreateShader" => create_shader as *const c_void,
                "glCreateProgram" => create_program as *const c_void,
                "glShaderSource" => shader_source as *const c_void,
                "glCompileShader" | "glDeleteShader" | "glLinkProgram" | "glUseProgram"
                | "glDeleteProgram" => one_id as *const c_void,
                "glGetShaderiv" | "glGetProgramiv" => status as *const c_void,
                "glAttachShader" => attach_shader as *const c_void,
                "glBindAttribLocation" => bind_attrib as *const c_void,
                "glGetUniformLocation" => uniform_location as *const c_void,
                "glUniform1i" => uniform1i as *const c_void,
                "glUniform2f" => uniform2f as *const c_void,
                "glGenTextures" => gen_textures as *const c_void,
                "glBindTexture" => bind_texture as *const c_void,
                "glPixelStorei" => pixel_store as *const c_void,
                "glTexImage2D" => tex_image as *const c_void,
                "glTexSubImage2D" => tex_sub_image as *const c_void,
                "glTexParameteri" => tex_parameter as *const c_void,
                "glDeleteTextures" => delete_textures as *const c_void,
                _ => std::ptr::null(),
            });
        }
        pub fn pixels(id: GLuint) -> Vec<u8> {
            TEXTURES.with_borrow(|textures| textures.pixels[&id].clone())
        }
    }

    #[test]
    fn first_upload_leaves_previous_texture_black() {
        mock_gl::load();
        let mut pass = GamePass::new().unwrap();
        let frame = vec![128; (SRC_W * SRC_H * 4) as usize];
        pass.upload(&frame);
        assert_eq!(mock_gl::pixels(pass.game), frame);
        assert!(mock_gl::pixels(pass.prev).iter().all(|&byte| byte == 0));
        let next = vec![64; frame.len()];
        pass.upload(&next);
        assert_eq!(mock_gl::pixels(pass.prev), frame);
        assert_eq!(mock_gl::pixels(pass.game), next);
    }

    #[test]
    fn custom_shader_preserves_actual_and_stretched_game_bounds() {
        let full = (0.0, 0.0, 720.0, 480.0);
        let gb = [40.0 / 240.0, 8.0 / 160.0, 160.0 / 240.0, 144.0 / 160.0];
        assert_eq!(shader_rect(full, gb, true), (120.0, 24.0, 480.0, 432.0));
        assert_eq!(shader_rect(full, gb, false), full);
        assert_eq!(shader_rect(full, WHOLE_TEXTURE, true), full);
    }
}
