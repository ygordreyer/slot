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
    Retro(RetroShader),
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
        let game = crate::gl::texture(SRC_W, SRC_H, gl::NEAREST, gl::CLAMP_TO_EDGE, gl::BGRA, None);
        // Black to begin with, so a blending shader's first frame blends with nothing rather
        // than with whatever the driver left in fresh memory.
        let black = vec![0u8; (SRC_W * SRC_H * 4) as usize];
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

    pub fn upload(&mut self, xrgb8888: &[u8]) {
        if xrgb8888.len() < (SRC_W * SRC_H * 4) as usize {
            return;
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

    pub fn draw(&self, quad: &Quad) {
        self.draw_source(self.game, self.prev, quad, self.src, true);
    }

    pub fn draw_still(&self, tex: gl::types::GLuint, quad: &Quad) {
        self.draw_source(tex, tex, quad, WHOLE_TEXTURE, false);
    }

    fn draw_source(
        &self,
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
                shader.draw(tex, prev, output, source, self.frames);
                return;
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

    #[test]
    fn custom_shader_preserves_actual_and_stretched_game_bounds() {
        let full = (0.0, 0.0, 720.0, 480.0);
        let gb = [40.0 / 240.0, 8.0 / 160.0, 160.0 / 240.0, 144.0 / 160.0];
        assert_eq!(shader_rect(full, gb, true), (120.0, 24.0, 480.0, 432.0));
        assert_eq!(shader_rect(full, gb, false), full);
        assert_eq!(shader_rect(full, WHOLE_TEXTURE, true), full);
    }
}
