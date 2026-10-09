use std::collections::{BTreeSet, HashMap};

use slot_gfx::{Draw, TexId, OUT_H, OUT_W};
use slot_store::Cart;

use crate::cart::{cart_box, gb_shell_of, label_colour, label_text, CART_H, CART_W};
use crate::hud::Millis;
use crate::silhouette::GbShell;
use crate::slot_chrome::{draw_empty_slot, MOUTH_H};

const SIDE_SCALE: f32 = 0.78;
const SIDE_ALPHA: f32 = 0.55;
pub fn rest_y(h: f32) -> f32 {
    if h > CART_H as f32 {
        (OUT_H as f32 - MOUTH_H - h) / 2.0
    } else {
        (OUT_H + CART_H) as f32 / 2.0 - h
    }
}

pub fn foot_y(h: f32) -> f32 {
    rest_y(h) + h
}
const OMEGA: f32 = 16.0;
const PART: f32 = 130.0;

const SLOTS: i32 = 3;

const REPEAT_DELAY_MS: Millis = 400;
const REPEAT_MS: [Millis; 4] = [110, 85, 65, 50];

pub struct Shelf {
    pub carts: Vec<Cart>,
    pub index: usize,
    pub scroll: f32,
    keys: Vec<String>,
    fallback_colours: Vec<[u8; 3]>,
    faces: Vec<Option<TexId>>,
    favorites: BTreeSet<String>,
    favorited: Vec<bool>,
    star: Option<TexId>,
    shadow: Option<TexId>,
    gb_shadow: Option<TexId>,
    gbc_shadow: Option<TexId>,
    shells: Vec<Option<GbShell>>,
    ride: f32,
    vel: f32,
    held: Option<(i32, Millis, usize)>,
}

impl Shelf {
    pub fn new(carts: Vec<Cart>) -> Self {
        Shelf {
            keys: carts.iter().map(Cart::key).collect(),
            fallback_colours: carts
                .iter()
                .map(|cart| label_colour(&label_text(cart)))
                .collect(),
            shells: carts.iter().map(gb_shell_of).collect(),
            carts,
            index: 0,
            scroll: 0.0,
            faces: Vec::new(),
            favorites: BTreeSet::new(),
            favorited: Vec::new(),
            star: None,
            shadow: None,
            gb_shadow: None,
            gbc_shadow: None,
            ride: 0.0,
            vel: 0.0,
            held: None,
        }
    }

    pub fn select(&mut self, i: usize) {
        self.index = i;
        self.scroll = i as f32;
        self.ride = i as f32;
        self.vel = 0.0;
    }

    pub fn set_shadow(&mut self, face: TexId) {
        self.shadow = Some(face);
    }

    pub fn set_gb_shadow(&mut self, shell: GbShell, face: TexId) {
        match shell {
            GbShell::Notched => self.gb_shadow = Some(face),
            GbShell::Rounded => self.gbc_shadow = Some(face),
        }
    }

    pub fn set_faces(&mut self, faces: Vec<TexId>) {
        self.faces = faces.into_iter().map(Some).collect();
    }

    pub fn set_favorite_star(&mut self, star: TexId) {
        self.star = Some(star);
    }

    pub fn set_favorites(&mut self, keys: &BTreeSet<String>) {
        self.favorites = keys.clone();
        self.favorited = self.keys.iter().map(|key| keys.contains(key)).collect();
    }

    pub fn cart_key(&self, index: usize) -> Option<&str> {
        self.keys.get(index).map(String::as_str)
    }

    pub fn set_cart_faces(&mut self, faces: &HashMap<String, TexId>) {
        self.faces = self
            .keys
            .iter()
            .map(|key| faces.get(key).copied())
            .collect();
    }

    pub fn replace_carts(&mut self, carts: Vec<Cart>, faces: &HashMap<String, TexId>) {
        let selected = self.cart_key(self.index).map(str::to_owned);
        let old = self.index;
        self.keys = carts.iter().map(Cart::key).collect();
        self.fallback_colours = carts
            .iter()
            .map(|cart| label_colour(&label_text(cart)))
            .collect();
        self.favorited = self
            .keys
            .iter()
            .map(|key| self.favorites.contains(key))
            .collect();
        self.shells = carts.iter().map(gb_shell_of).collect();
        self.carts = carts;
        self.set_cart_faces(faces);
        let index = self
            .keys
            .iter()
            .position(|key| Some(key.as_str()) == selected.as_deref())
            .unwrap_or_else(|| {
                if self.carts.is_empty() {
                    0
                } else {
                    old % self.carts.len()
                }
            });
        self.release_hold();
        self.select(index);
    }

    pub fn find(&self, key: &str) -> Option<(&Cart, Option<TexId>)> {
        let i = self.keys.iter().position(|candidate| candidate == key)?;
        Some((&self.carts[i], self.faces.get(i).copied().flatten()))
    }

    pub fn left(&mut self) {
        self.step(-1);
    }

    pub fn right(&mut self) {
        self.step(1);
    }

    pub fn jump_next_letter(&mut self) {
        self.jump(1);
    }

    pub fn jump_prev_letter(&mut self) {
        self.jump(-1);
    }

    fn start_of_letter(&self, from: usize) -> usize {
        let n = self.carts.len();
        let letter = slot_store::initial(&self.carts[from].stem);
        let mut at = from;
        for _ in 0..n {
            let before = (at as i32 - 1).rem_euclid(n as i32) as usize;
            if slot_store::initial(&self.carts[before].stem) != letter {
                break;
            }
            at = before;
        }
        at
    }

    fn jump(&mut self, dir: i32) {
        let n = self.carts.len();
        if n < 2 {
            return;
        }
        let wrap = |i: i32| i.rem_euclid(n as i32) as usize;
        let here = slot_store::initial(&self.carts[self.index].stem);
        if self
            .carts
            .iter()
            .all(|c| slot_store::initial(&c.stem) == here)
        {
            return;
        }
        let target = match dir > 0 {
            true => {
                let mut at = self.index;
                for _ in 0..n {
                    at = wrap(at as i32 + 1);
                    if slot_store::initial(&self.carts[at].stem) != here {
                        break;
                    }
                }
                at
            }
            false => {
                let start = self.start_of_letter(self.index);
                match start == self.index {
                    true => self.start_of_letter(wrap(start as i32 - 1)),
                    false => start,
                }
            }
        };
        let ahead = (target as i32 - self.index as i32).rem_euclid(n as i32);
        let delta = match dir > 0 {
            true => ahead,
            false => ahead - n as i32,
        };
        self.index = target;
        self.ride += delta as f32;
    }

    pub fn hold_left(&mut self, now: Millis) {
        self.hold(-1, now);
    }

    pub fn hold_right(&mut self, now: Millis) {
        self.hold(1, now);
    }

    fn hold(&mut self, by: i32, now: Millis) {
        self.step(by);
        self.held = Some((by, now + REPEAT_DELAY_MS, 0));
    }

    pub fn release_left(&mut self) {
        self.release(-1);
    }

    pub fn release_right(&mut self) {
        self.release(1);
    }

    fn release(&mut self, by: i32) {
        if matches!(self.held, Some((held, _, _)) if held == by) {
            self.held = None;
        }
    }

    pub fn release_hold(&mut self) {
        self.held = None;
    }

    pub fn tick(&mut self, now: Millis) {
        let Some((by, due, fired)) = self.held else {
            return;
        };
        if now < due {
            return;
        }
        self.step(by);
        let rate = REPEAT_MS[fired.min(REPEAT_MS.len() - 1)];
        self.held = Some((by, now + rate, fired + 1));
    }

    fn step(&mut self, by: i32) {
        let n = self.carts.len();
        if n < 2 {
            return;
        }
        self.index = (self.index as i32 + by).rem_euclid(n as i32) as usize;
        self.ride += by as f32;
    }

    pub fn scroll_target(&self) -> f32 {
        let n = self.carts.len();
        if n == 0 {
            return 0.0;
        }
        let from = self.ride;
        let n = n as f32;
        from + (self.index as f32 - from + n / 2.0).rem_euclid(n) - n / 2.0
    }

    pub fn cart_at_offset(&self, off: i32) -> Option<usize> {
        let n = self.carts.len() as i32;
        if n == 0 {
            return None;
        }
        if n == 1 {
            return (off == 0).then_some(self.index);
        }
        Some((self.index as i32 + off).rem_euclid(n) as usize)
    }

    pub fn selected_at(&self) -> (f32, f32) {
        let w = self
            .carts
            .get(self.index)
            .map_or(CART_W, |c| cart_box(c.platform).0) as f32;
        let offset = self.scroll_target() - self.scroll;
        let scale = shrink(offset);
        (OUT_W as f32 / 2.0 + offset * w - w * scale / 2.0, scale)
    }

    pub fn update(&mut self, dt: f32) {
        let accel = -2.0 * OMEGA * self.vel - OMEGA * OMEGA * (self.scroll - self.scroll_target());
        self.vel += accel * dt;
        self.scroll += self.vel * dt;
    }

    pub fn draw(&self, shake: f32, out: &mut Vec<Draw>) {
        self.draw_row(None, shake, 0.0, 1.0, out);
        draw_empty_slot(out);
    }

    pub fn draw_row(
        &self,
        hidden: Option<&str>,
        shake: f32,
        recede: f32,
        dim: f32,
        out: &mut Vec<Draw>,
    ) {
        let recede = recede.clamp(0.0, 1.0);
        let dim = dim.clamp(0.0, 1.0);
        let target = self.scroll_target();
        for slot in -SLOTS..=SLOTS {
            let Some(i) = self.cart_at_offset(slot) else {
                continue;
            };
            let cart = &self.carts[i];
            if hidden.is_some() && hidden == self.cart_key(i) {
                continue;
            }
            let offset = target + slot as f32 - self.scroll;
            let t = offset.abs().min(1.0);
            let scale = shrink(offset);
            let alpha = (1.0 + (SIDE_ALPHA - 1.0) * t) * (1.0 - recede);
            let (cw, ch) = cart_box(cart.platform);
            let (w, h) = (cw as f32 * scale, ch as f32 * scale);
            let away = offset.signum() * (1.0 + offset.abs());
            let x = OUT_W as f32 / 2.0 + offset * cw as f32 - w / 2.0 + away * PART * recede;
            if x + w <= 0.0 || x >= OUT_W as f32 || alpha <= 0.0 {
                continue;
            }
            let x = x + shake;
            let y = foot_y(ch as f32) - h;
            if alpha < 1.0 {
                let backing = match self.shells.get(i).copied().flatten() {
                    None => self.shadow,
                    Some(GbShell::Notched) => self.gb_shadow,
                    Some(GbShell::Rounded) => self.gbc_shadow,
                };
                if let Some(tex) = backing {
                    out.push(Draw::Tex {
                        x,
                        y,
                        w,
                        h,
                        tex,
                        alpha: recede_alpha(alpha),
                    });
                }
            }
            out.push(match self.faces.get(i) {
                Some(Some(tex)) => Draw::Tex {
                    x,
                    y,
                    w,
                    h,
                    tex: *tex,
                    alpha: alpha * dim,
                },
                _ => {
                    let c = self
                        .fallback_colours
                        .get(i)
                        .copied()
                        .unwrap_or_else(|| label_colour(&label_text(cart)));
                    Draw::Rect {
                        x,
                        y,
                        w,
                        h,
                        colour: [
                            c[0] as f32 / 255.0,
                            c[1] as f32 / 255.0,
                            c[2] as f32 / 255.0,
                            alpha * dim,
                        ],
                    }
                }
            });
            if let Some(tex) = self.star.filter(|_| self.favorited.get(i) == Some(&true)) {
                out.push(Draw::Tex {
                    x: x + w - (FAVORITE_STAR_PX as f32 + FAVORITE_STAR_INSET) * scale,
                    y: y + FAVORITE_STAR_INSET * scale,
                    w: FAVORITE_STAR_PX as f32 * scale,
                    h: FAVORITE_STAR_PX as f32 * scale,
                    tex,
                    alpha: alpha * dim,
                });
            }
        }
    }
}

fn shrink(offset: f32) -> f32 {
    1.0 + (SIDE_SCALE - 1.0) * offset.abs().min(1.0)
}

fn recede_alpha(face_alpha: f32) -> f32 {
    (face_alpha / SIDE_ALPHA).clamp(0.0, 1.0)
}

pub const FAVORITE_STAR_PX: u32 = 24;
pub const FAVORITE_STAR_INSET: f32 = 10.0;

pub fn favorite_star_face() -> crate::CartFace {
    let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
      <path fill="#efc75e" stroke="#302511" stroke-width="1"
        d="M12 1.5 15.1 8.3 22.5 9.1 17 14.2 18.5 21.5 12 17.8 5.5 21.5 7 14.2 1.5 9.1 8.9 8.3Z"/>
    </svg>"##;
    crate::CartFace {
        rgba: crate::art::render_svg(svg, FAVORITE_STAR_PX, FAVORITE_STAR_PX)
            .unwrap_or_else(|| vec![0; (FAVORITE_STAR_PX * FAVORITE_STAR_PX * 4) as usize]),
        w: FAVORITE_STAR_PX,
        h: FAVORITE_STAR_PX,
    }
}
