//! Software rendering: a pixel buffer, rectangles and glyph blitting.
//!
//! Everything is drawn on the CPU into a `u32` framebuffer (0x00RRGGBB, what
//! softbuffer expects) which the window then presents. No GPU needed.

pub mod font;
pub mod layout;
pub mod ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255 };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }

    /// Parse `rrggbb` (without the `#`).
    pub fn from_hex(hex: &str) -> Option<Color> {
        if hex.len() != 6 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(Color::from_u32(value))
    }

    pub fn from_u32(value: u32) -> Color {
        Color { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
    }

    pub fn to_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// Mix `self` over `under` with the given coverage (0 = under, 255 = self).
    pub fn blend_over(self, under: Color, alpha: u8) -> Color {
        if alpha == 255 {
            return self;
        }
        if alpha == 0 {
            return under;
        }
        let a = alpha as u32;
        let mix = |top: u8, bottom: u8| ((top as u32 * a + bottom as u32 * (255 - a)) / 255) as u8;
        Color { r: mix(self.r, under.r), g: mix(self.g, under.g), b: mix(self.b, under.b) }
    }

    /// Linear mix: `t` = 0 gives `self`, 1 gives `other`.
    pub fn mix(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Color { r: lerp(self.r, other.r), g: lerp(self.g, other.g), b: lerp(self.b, other.b) }
    }

    /// Perceived brightness, 0..=255.
    pub fn luma(self) -> u32 {
        (self.r as u32 * 299 + self.g as u32 * 587 + self.b as u32 * 114) / 1000
    }

    pub fn is_dark(self) -> bool {
        (self.r as u32 * 299 + self.g as u32 * 587 + self.b as u32 * 114) / 1000 < 128
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect { x, y, w: (right - x).max(0), h: (bottom - y).max(0) }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Shrink by `n` pixels on every side.
    pub fn inset(&self, n: i32) -> Rect {
        Rect { x: self.x + n, y: self.y + n, w: (self.w - 2 * n).max(0), h: (self.h - 2 * n).max(0) }
    }
}

/// Draws into a framebuffer, clipped to a rectangle.
pub struct Painter<'a> {
    pixels: &'a mut [u32],
    pub width: usize,
    pub height: usize,
    clip: Rect,
}

impl<'a> Painter<'a> {
    pub fn new(pixels: &'a mut [u32], width: usize, height: usize) -> Painter<'a> {
        debug_assert!(pixels.len() >= width * height);
        Painter { pixels, width, height, clip: Rect::new(0, 0, width as i32, height as i32) }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// Restrict drawing to `rect` (intersected with the current clip).
    pub fn set_clip(&mut self, rect: Rect) {
        self.clip = rect.intersect(self.bounds());
    }

    pub fn reset_clip(&mut self) {
        self.clip = self.bounds();
    }

    pub fn fill(&mut self, color: Color) {
        self.pixels[..self.width * self.height].fill(color.to_u32());
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        let r = rect.intersect(self.clip);
        if r.is_empty() {
            return;
        }
        let value = color.to_u32();
        for y in r.y..r.bottom() {
            let row = y as usize * self.width;
            self.pixels[row + r.x as usize..row + r.right() as usize].fill(value);
        }
    }

    pub fn hline(&mut self, x: i32, y: i32, w: i32, color: Color) {
        self.fill_rect(Rect::new(x, y, w, 1), color);
    }

    pub fn vline(&mut self, x: i32, y: i32, h: i32, color: Color) {
        self.fill_rect(Rect::new(x, y, 1, h), color);
    }

    pub fn rect_outline(&mut self, rect: Rect, color: Color) {
        self.hline(rect.x, rect.y, rect.w, color);
        self.hline(rect.x, rect.bottom() - 1, rect.w, color);
        self.vline(rect.x, rect.y, rect.h, color);
        self.vline(rect.right() - 1, rect.y, rect.h, color);
    }

    /// Blit a coverage bitmap (one byte per pixel) at `x, y` in `color`.
    /// `shear` shifts each row to the right proportionally to its distance from
    /// the bottom, which is how italics are faked with an upright font.
    /// Draw an RGBA image (8 bits per channel, rows top to bottom) at (x, y),
    /// blending it over what is there by its alpha.
    pub fn blit_rgba(&mut self, x: i32, y: i32, w: usize, h: usize, rgba: &[u8]) {
        for row in 0..h {
            let py = y + row as i32;
            if py < self.clip.y || py >= self.clip.bottom() {
                continue;
            }
            for col in 0..w {
                let px = x + col as i32;
                if px < self.clip.x || px >= self.clip.right() {
                    continue;
                }
                let p = (row * w + col) * 4;
                let (color, alpha) = (Color::rgb(rgba[p], rgba[p + 1], rgba[p + 2]), rgba[p + 3]);
                if alpha == 0 {
                    continue;
                }
                let i = py as usize * self.width + px as usize;
                let under = Color::from_u32(self.pixels[i]);
                self.pixels[i] = color.blend_over(under, alpha).to_u32();
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn blit_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], color: Color, shear: f32) {
        for row in 0..h {
            let py = y + row as i32;
            if py < self.clip.y || py >= self.clip.bottom() {
                continue;
            }
            let shift = if shear != 0.0 { ((h - row) as f32 * shear).round() as i32 } else { 0 };
            let row_start = py as usize * self.width;
            for col in 0..w {
                let px = x + col as i32 + shift;
                if px < self.clip.x || px >= self.clip.right() {
                    continue;
                }
                let coverage = mask[row * w + col];
                if coverage == 0 {
                    continue;
                }
                let i = row_start + px as usize;
                let under = Color::from_u32(self.pixels[i]);
                self.pixels[i] = color.blend_over(under, coverage).to_u32();
            }
        }
    }

    /// Write the framebuffer as a binary PPM image (used by `--screenshot`).
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for y in 0..self.height {
            for x in 0..self.width {
                let c = Color::from_u32(self.pixels[y * self.width + x]);
                out.extend_from_slice(&[c.r, c.g, c.b]);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trip() {
        let c = Color::from_hex("1a2b3c").unwrap();
        assert_eq!(c, Color::rgb(0x1a, 0x2b, 0x3c));
        assert_eq!(Color::from_u32(c.to_u32()), c);
        assert_eq!(Color::WHITE.blend_over(Color::BLACK, 255), Color::WHITE);
        assert_eq!(Color::WHITE.blend_over(Color::BLACK, 0), Color::BLACK);
    }

    #[test]
    fn fill_respects_clip() {
        let mut pixels = vec![0u32; 16];
        let mut p = Painter::new(&mut pixels, 4, 4);
        p.set_clip(Rect::new(1, 1, 2, 2));
        p.fill_rect(Rect::new(0, 0, 4, 4), Color::WHITE);
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[5], 0xffffff);
        assert_eq!(pixels[15], 0);
    }
}
