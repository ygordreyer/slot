use crate::draw::{Draw, Sprites, TexId};
use crate::grade::blue_light_gain;
use crate::pipeline::{GamePass, Look};
use crate::quad::Quad;
use crate::retroshader::RetroShader;
use crate::shaders::{BLIT_FRAG, BLIT_VERT};
use crate::surface::{blit_rect, GfxError, Surface, OUT_H, OUT_W};

pub const BACKDROP: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// How the game layer is drawn, as a caller outside this crate names it.
#[derive(Copy, Clone, Debug)]
pub enum ShaderChoice<'a> {
    /// The built-in LCD3x mask. The default.
    Lcd,
    /// No filter at all: the frame at 3x, nearest neighbour.
    Plain,
    /// The text of a single-pass RetroArch `.glsl` file.
    RetroArch(&'a str),
    Preset(
        &'a std::path::Path,
        &'a std::collections::BTreeMap<String, f32>,
    ),
}

pub struct Compositor {
    fbo: gl::types::GLuint,
    tex: gl::types::GLuint,
    blit: gl::types::GLuint,
    u_gain: gl::types::GLint,
    gain: [f32; 3],
    shake: f32,
    quad: Quad,
    game: GamePass,
    sprites: Sprites,
    profile: Option<crate::preset::Profile>,
    overlay: Option<(TexId, f32)>,
    overlay_error: Option<String>,
}

impl Compositor {
    pub fn new(surface: &dyn Surface) -> Result<Self, GfxError> {
        crate::gl::load(surface);
        let blit = crate::shaders::program(BLIT_VERT, BLIT_FRAG)?;
        let tex = crate::gl::texture(OUT_W, OUT_H, gl::NEAREST, gl::CLAMP_TO_EDGE, gl::RGBA, None);
        unsafe {
            let mut fbo = 0;
            gl::GenFramebuffers(1, &mut fbo);
            gl::BindFramebuffer(gl::FRAMEBUFFER, fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                tex,
                0,
            );
            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            if status != gl::FRAMEBUFFER_COMPLETE {
                gl::DeleteFramebuffers(1, &fbo);
                gl::DeleteTextures(1, &tex);
                gl::DeleteProgram(blit);
                return Err(GfxError::Framebuffer(status));
            }
            gl::UseProgram(blit);
            gl::Uniform1i(crate::gl::uniform_location(blit, "u_tex"), 0);
            let u_gain = crate::gl::uniform_location(blit, "u_gain");

            Ok(Compositor {
                fbo,
                tex,
                blit,
                u_gain,
                gain: blue_light_gain(0),
                shake: 0.0,
                quad: Quad::new(),
                game: GamePass::new()?,
                sprites: Sprites::new()?,
                profile: None,
                overlay: None,
                overlay_error: None,
            })
        }
    }

    /// Changes how the game layer is drawn from the next frame on. A RetroArch shader that will
    /// not compile or link puts the built-in LCD look back, so a bad file on the card never
    /// leaves the panel black, and hands the driver's log back for whoever wants to show it.
    pub fn set_shader(&mut self, choice: ShaderChoice) -> Result<(), GfxError> {
        self.clear_profile();
        let look = match choice {
            ShaderChoice::Lcd => Look::Lcd,
            ShaderChoice::Plain => Look::Plain,
            ShaderChoice::Preset(path, saved) => match RetroShader::load(path, saved) {
                Ok(shader) => {
                    self.install_profile(shader.profile.clone());
                    Look::Retro(Box::new(shader))
                }
                Err(e) => {
                    self.game.set_look(Look::Lcd);
                    return Err(e);
                }
            },
            ShaderChoice::RetroArch(src) => match RetroShader::new(src) {
                Ok(shader) => Look::Retro(Box::new(shader)),
                Err(e) => {
                    self.game.set_look(Look::Lcd);
                    return Err(e);
                }
            },
        };
        self.game.set_look(look);
        Ok(())
    }

    fn clear_profile(&mut self) {
        if let Some((tex, _)) = self.overlay.take() {
            self.sprites.remove_texture(tex);
        }
        self.profile = None;
        self.overlay_error = None;
    }

    fn install_profile(&mut self, profile: crate::preset::Profile) {
        if let Some(path) = &profile.overlay {
            match crate::png_image::overlay(path) {
                Ok(image) => {
                    let tex =
                        self.sprites
                            .create_texture_nearest(image.width, image.height, &image.rgba);
                    self.overlay = Some((tex, profile.overlay_opacity));
                }
                Err(e) => self.overlay_error = Some(e),
            }
        }
        self.profile = Some(profile);
    }

    pub fn shader_profile(&self) -> Option<&crate::preset::Profile> {
        self.profile.as_ref()
    }

    pub fn take_overlay_error(&mut self) -> Option<String> {
        self.overlay_error.take()
    }

    fn draw_overlay(&self) {
        if let Some((tex, alpha)) = self.overlay {
            self.sprites.draw(
                &[Draw::Tex {
                    x: 0.0,
                    y: 0.0,
                    w: OUT_W as f32,
                    h: OUT_H as f32,
                    tex,
                    alpha,
                }],
                &self.quad,
            );
        }
    }

    pub fn shader_parameters(&self) -> &[crate::preset::Parameter] {
        self.game.parameters()
    }
    pub fn set_shader_parameter(&mut self, name: &str, value: f32) {
        self.game.set_parameter(name, value);
    }
    pub fn set_game_rewinding(&mut self, rewinding: bool) {
        self.game.set_rewinding(rewinding);
    }
    pub fn take_shader_error(&mut self) -> Option<String> {
        let error = self.game.take_shader_error();
        if error.is_some() {
            self.clear_profile();
        }
        error
    }

    pub fn upload_game(&mut self, xrgb8888: &[u8]) {
        self.game.upload(xrgb8888);
    }

    pub fn draw_game(&mut self) {
        self.game.draw(&self.quad);
        self.draw_overlay();
    }

    pub fn draw_list(&mut self, items: &[Draw]) {
        let mut from = 0;
        for (i, item) in items.iter().enumerate() {
            match *item {
                Draw::Game => {
                    self.sprites.draw(&items[from..i], &self.quad);
                    self.draw_game();
                }
                Draw::Shot { tex } => {
                    self.sprites.draw(&items[from..i], &self.quad);
                    if let Some(tex) = self.sprites.source(tex) {
                        self.game.draw_still(tex, &self.quad);
                        self.draw_overlay();
                    }
                }
                _ => continue,
            }
            from = i + 1;
        }
        self.sprites.draw(&items[from..], &self.quad);
    }

    pub fn create_texture(&mut self, w: u32, h: u32, rgba: &[u8]) -> TexId {
        self.sprites.create_texture(w, h, rgba)
    }

    pub fn create_texture_nearest(&mut self, w: u32, h: u32, rgba: &[u8]) -> TexId {
        self.sprites.create_texture_nearest(w, h, rgba)
    }

    pub fn update_texture(&mut self, id: TexId, w: u32, h: u32, rgba: &[u8]) {
        self.sprites.update_texture(id, w, h, rgba);
    }

    pub fn set_blue_light(&mut self, step: u8) {
        self.gain = blue_light_gain(step);
    }

    pub fn set_screen_power(&mut self, t: f32) {
        self.game.set_power(t);
    }

    pub fn set_game_source_rect(&mut self, rect: [f32; 4]) {
        self.game.set_source_rect(rect);
    }

    pub fn set_game_shader_source(&mut self, rect: [f32; 4], actual: bool) {
        self.game.set_shader_source(rect, actual);
    }

    pub fn set_shake(&mut self, dx: f32) {
        self.shake = dx;
    }

    pub fn read_frame(&self) -> Vec<u8> {
        let stride = OUT_W as usize * 4;
        let mut buf = vec![0u8; stride * OUT_H as usize];
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::PixelStorei(gl::PACK_ALIGNMENT, 1);
            gl::ReadPixels(
                0,
                0,
                OUT_W as i32,
                OUT_H as i32,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                buf.as_mut_ptr() as *mut std::ffi::c_void,
            );
        }
        let mut top_down = Vec::with_capacity(buf.len());
        for row in buf.chunks_exact(stride).rev() {
            top_down.extend_from_slice(row);
        }
        top_down
    }

    pub fn begin_frame(&mut self) {
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::Viewport(0, 0, OUT_W as i32, OUT_H as i32);
            gl::ClearColor(BACKDROP[0], BACKDROP[1], BACKDROP[2], BACKDROP[3]);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }

    pub fn end_frame(&mut self, window: (u32, u32)) {
        let (x, y, w, h) = blit_rect(window, self.shake);
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::Viewport(0, 0, window.0 as i32, window.1 as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            gl::Viewport(x, y, w, h);
            gl::UseProgram(self.blit);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.tex);
            gl::Uniform3f(self.u_gain, self.gain[0], self.gain[1], self.gain[2]);
        }
        self.quad.draw();
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteFramebuffers(1, &self.fbo);
            gl::DeleteTextures(1, &self.tex);
            gl::DeleteProgram(self.blit);
        }
    }
}
