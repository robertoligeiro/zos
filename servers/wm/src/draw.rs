//! Clipped 2D drawing onto an RGB32 framebuffer, plus the mouse pointer sprite.
//!
//! Every write is checked against both the caller's clip rectangle and the surface
//! bounds, so a bad coordinate can never reach outside the pixel slice. Colors are
//! passed as 0xRRGGBB and packed with the framebuffer's channel shifts.

use crate::geometry::Rect;
use font8x8::{UnicodeFonts, BASIC_FONTS};

pub const GLYPH_W: i32 = 8;
pub const GLYPH_H: i32 = 16;

pub struct Surface<'a> {
    pixels: &'a mut [u32],
    width: i32,
    height: i32,
    stride: usize,
    shifts: [u8; 3],
}

impl<'a> Surface<'a> {
    /// `pitch` is in bytes. Returns `None` for descriptions that do not make sense or
    /// do not fit the slice.
    pub fn new(
        pixels: &'a mut [u32],
        width: usize,
        height: usize,
        pitch: usize,
        shifts: [u8; 3],
    ) -> Option<Self> {
        let stride = pitch / 4;
        if width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || pitch % 4 != 0
            || stride < width
            || pixels.len() < stride.checked_mul(height)?
            || shifts.iter().any(|&s| s > 24)
        {
            return None;
        }
        Some(Self {
            pixels,
            width: width as i32,
            height: height as i32,
            stride,
            shifts,
        })
    }
    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }
    pub fn pack(&self, rgb: u32) -> u32 {
        ((rgb >> 16 & 255) << self.shifts[0])
            | ((rgb >> 8 & 255) << self.shifts[1])
            | ((rgb & 255) << self.shifts[2])
    }
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            None
        } else {
            Some(y as usize * self.stride + x as usize)
        }
    }
    /// The packed value of a pixel, or `None` outside the surface.
    pub fn get(&self, x: i32, y: i32) -> Option<u32> {
        self.index(x, y).map(|i| self.pixels[i])
    }
    fn set_packed(&mut self, x: i32, y: i32, value: u32) {
        if let Some(i) = self.index(x, y) {
            // SAFETY: `i` is below stride * height, which `new` checked against the slice.
            unsafe { core::ptr::write_volatile(&mut self.pixels[i], value) };
        }
    }
    pub fn fill(&mut self, rect: Rect, clip: Rect, rgb: u32) {
        let area = rect.intersect(clip).intersect(self.bounds());
        let value = self.pack(rgb);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                self.set_packed(x, y, value);
            }
        }
    }
    pub fn glyph(&mut self, x: i32, y: i32, byte: u8, fg: u32, bg: Option<u32>, clip: Rect) {
        let area = Rect::new(x, y, GLYPH_W, GLYPH_H)
            .intersect(clip)
            .intersect(self.bounds());
        if area.is_empty() {
            return;
        }
        let glyph = BASIC_FONTS.get(byte as char).unwrap_or([0; 8]);
        let (fg, bg) = (self.pack(fg), bg.map(|c| self.pack(c)));
        for py in area.y..area.bottom() {
            let row = glyph[((py - y) / 2) as usize];
            for px in area.x..area.right() {
                if row & (1 << (px - x)) != 0 {
                    self.set_packed(px, py, fg);
                } else if let Some(bg) = bg {
                    self.set_packed(px, py, bg);
                }
            }
        }
    }
    /// Draws text left to right from (x, y); returns the x after the last glyph.
    pub fn text(
        &mut self,
        x: i32,
        y: i32,
        text: &[u8],
        fg: u32,
        bg: Option<u32>,
        clip: Rect,
    ) -> i32 {
        let mut at = x;
        for &byte in text {
            if at >= clip.right() {
                break;
            }
            self.glyph(
                at,
                y,
                if (32..=126).contains(&byte) {
                    byte
                } else {
                    b'?'
                },
                fg,
                bg,
                clip,
            );
            at = at.saturating_add(GLYPH_W);
        }
        at
    }
    /// A one-pixel frame: light on top/left and dark on bottom/right when `raised`.
    pub fn bevel(&mut self, rect: Rect, raised: bool, clip: Rect, light: u32, dark: u32) {
        let (top_left, bottom_right) = if raised { (light, dark) } else { (dark, light) };
        self.fill(Rect::new(rect.x, rect.y, rect.w, 1), clip, top_left);
        self.fill(Rect::new(rect.x, rect.y, 1, rect.h), clip, top_left);
        self.fill(
            Rect::new(rect.x, rect.bottom() - 1, rect.w, 1),
            clip,
            bottom_right,
        );
        self.fill(
            Rect::new(rect.right() - 1, rect.y, 1, rect.h),
            clip,
            bottom_right,
        );
    }
    pub fn frame(&mut self, rect: Rect, thickness: i32, clip: Rect, rgb: u32) {
        let t = thickness.max(1);
        self.fill(Rect::new(rect.x, rect.y, rect.w, t), clip, rgb);
        self.fill(Rect::new(rect.x, rect.bottom() - t, rect.w, t), clip, rgb);
        self.fill(Rect::new(rect.x, rect.y, t, rect.h), clip, rgb);
        self.fill(Rect::new(rect.right() - t, rect.y, t, rect.h), clip, rgb);
    }
    /// Inverts a rectangular frame; drawing the same frame again restores the pixels.
    pub fn xor_frame(&mut self, rect: Rect, thickness: i32, clip: Rect) {
        let t = thickness.max(1);
        let area = rect.intersect(clip).intersect(self.bounds());
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let edge = x < rect.x + t
                    || x >= rect.right() - t
                    || y < rect.y + t
                    || y >= rect.bottom() - t;
                if edge {
                    if let Some(i) = self.index(x, y) {
                        let value = self.pixels[i] ^ 0x00ff_ffff;
                        // SAFETY: same bound as `set_packed`.
                        unsafe { core::ptr::write_volatile(&mut self.pixels[i], value) };
                    }
                }
            }
        }
    }
}

// ---- Pointer ---------------------------------------------------------------

pub const CURSOR_W: usize = 12;
pub const CURSOR_H: usize = 16;
const ARROW: [&[u8; CURSOR_W]; CURSOR_H] = [
    b"#           ",
    b"##          ",
    b"#.#         ",
    b"#..#        ",
    b"#...#       ",
    b"#....#      ",
    b"#.....#     ",
    b"#......#    ",
    b"#.......#   ",
    b"#........#  ",
    b"#.....#####.",
    b"#..#..#     ",
    b"#.# #..#    ",
    b"##  #..#    ",
    b"#    #..#   ",
    b"     ###    ",
];

/// The pointer: drawn last, with the pixels beneath it saved so it can move without
/// repainting anything.
pub struct Cursor {
    x: i32,
    y: i32,
    visible: bool,
    saved: [u32; CURSOR_W * CURSOR_H],
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

impl Cursor {
    pub const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            visible: false,
            saved: [0; CURSOR_W * CURSOR_H],
        }
    }
    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, CURSOR_W as i32, CURSOR_H as i32)
    }
    pub fn visible(&self) -> bool {
        self.visible
    }
    /// Restores the pixels under the pointer.
    pub fn hide(&mut self, surface: &mut Surface<'_>) {
        if !self.visible {
            return;
        }
        for row in 0..CURSOR_H {
            for col in 0..CURSOR_W {
                surface.set_packed(
                    self.x + col as i32,
                    self.y + row as i32,
                    self.saved[row * CURSOR_W + col],
                );
            }
        }
        self.visible = false;
    }
    /// Saves the pixels at (x, y) and draws the pointer there.
    pub fn show(&mut self, surface: &mut Surface<'_>, x: i32, y: i32) {
        self.hide(surface);
        (self.x, self.y) = (x, y);
        let (black, white) = (surface.pack(0x000000), surface.pack(0xffffff));
        for (row, line) in ARROW.iter().enumerate() {
            for (col, &sprite) in line.iter().enumerate() {
                let (px, py) = (x + col as i32, y + row as i32);
                self.saved[row * CURSOR_W + col] = surface.get(px, py).unwrap_or(0);
                match sprite {
                    b'#' => surface.set_packed(px, py, black),
                    b'.' => surface.set_packed(px, py, white),
                    _ => (),
                }
            }
        }
        self.visible = true;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    const SHIFTS: [u8; 3] = [16, 8, 0];

    fn surface(pixels: &mut Vec<u32>, width: usize, height: usize, pitch: usize) -> Surface<'_> {
        Surface::new(pixels, width, height, pitch, SHIFTS).unwrap()
    }

    #[test]
    fn invalid_surfaces_are_refused() {
        let mut small = vec![0u32; 10];
        assert!(
            Surface::new(&mut small, 4, 4, 16, SHIFTS).is_none(),
            "slice too short"
        );
        let mut ok = vec![0u32; 64];
        for (w, h, pitch) in [(0, 4, 32), (4, 0, 32), (4, 4, 30), (8, 4, 16), (4, 4, 0)] {
            assert!(
                Surface::new(&mut ok, w, h, pitch, SHIFTS).is_none(),
                "{w}x{h} pitch {pitch}"
            );
        }
        assert!(Surface::new(&mut ok, 4, 4, 16, [25, 8, 0]).is_none());
        assert!(Surface::new(&mut ok, 20000, 1, 80000, SHIFTS).is_none());
        assert!(Surface::new(&mut ok, usize::MAX, 4, 16, SHIFTS).is_none());
    }

    #[test]
    fn fill_respects_the_clip_the_surface_and_the_pitch_padding() {
        let mut pixels = vec![7u32; 12 * 6];
        let mut s = surface(&mut pixels, 8, 6, 12 * 4);
        let all = s.bounds();
        // Partly outside on every side, further limited by a clip.
        s.fill(Rect::new(-5, -5, 100, 100), Rect::new(2, 1, 4, 3), 0x123456);
        s.fill(Rect::new(20, 20, 5, 5), all, 0xffffff);
        s.fill(Rect::new(0, 0, -3, 4), all, 0xffffff);
        let painted = s.pack(0x123456);
        for y in 0..6 {
            for x in 0..8 {
                let inside = (2..6).contains(&x) && (1..4).contains(&y);
                assert_eq!(
                    pixels[y * 12 + x],
                    if inside { painted } else { 7 },
                    "({x},{y})"
                );
            }
            for padding in 8..12 {
                assert_eq!(
                    pixels[y * 12 + padding],
                    7,
                    "padding in row {y} was written"
                );
            }
        }
        assert_eq!(painted, 0x123456);
    }

    #[test]
    fn channel_shifts_follow_the_framebuffer_format() {
        let mut pixels = vec![0u32; 4];
        let s = Surface::new(&mut pixels, 2, 2, 8, [0, 8, 16]).unwrap();
        assert_eq!(s.pack(0x112233), 0x332211);
        let s = Surface::new(&mut pixels, 2, 2, 8, [16, 8, 0]).unwrap();
        assert_eq!(s.pack(0x112233), 0x112233);
    }

    #[test]
    fn glyphs_and_text_are_clipped_not_wrapped() {
        let mut pixels = vec![0u32; 20 * 20];
        let mut s = surface(&mut pixels, 20, 20, 80);
        let clip = Rect::new(0, 0, 12, 20);
        let end = s.text(4, 2, b"ab", 0xffffff, Some(0x0000ff), clip);
        // The second glyph would start at x=12, outside the clip: drawing stops there.
        assert_eq!(end, 12);
        for y in 0..20 {
            for x in 12..20 {
                assert_eq!(pixels[y * 20 + x], 0, "({x},{y}) is outside the clip");
            }
        }
        // The first glyph box is filled with background or foreground only.
        for y in 2..18 {
            for x in 4..12 {
                assert!(pixels[y * 20 + x] == 0x0000ff || pixels[y * 20 + x] == 0xffffff);
            }
        }
        assert_eq!(pixels[1 * 20 + 4], 0, "above the glyph");
        // Without a background only set glyph pixels change.
        let mut other = vec![0u32; 20 * 20];
        let mut o = surface(&mut other, 20, 20, 80);
        o.glyph(0, 0, b'I', 0xffffff, None, o.bounds());
        assert!(other.iter().any(|&p| p == 0xffffff) && other.iter().any(|&p| p == 0));
        // Non-printable bytes draw a '?'; glyphs hanging off the surface do not panic.
        let mut c = vec![0u32; 9 * 9];
        let mut cs = surface(&mut c, 9, 9, 36);
        cs.text(-4, -9, &[1, 200, b'x'], 0xffffff, Some(1), cs.bounds());
        cs.glyph(5, 5, b'W', 0xffffff, Some(2), cs.bounds());
        cs.glyph(i32::MAX - 3, i32::MAX - 3, b'W', 1, Some(2), cs.bounds());
        cs.text(i32::MAX - 4, 0, b"abc", 1, None, cs.bounds());
    }

    #[test]
    fn xor_frames_undo_themselves_and_bevels_stay_inside_their_rect() {
        let mut pixels = vec![0x00aa_5533u32; 16 * 12];
        let original = pixels.clone();
        let mut s = surface(&mut pixels, 16, 12, 64);
        let clip = s.bounds();
        s.xor_frame(Rect::new(2, 2, 10, 8), 2, clip);
        assert_ne!(pixels, original);
        let mut s = surface(&mut pixels, 16, 12, 64);
        s.xor_frame(Rect::new(2, 2, 10, 8), 2, clip);
        assert_eq!(pixels, original);
        // Partly off-screen frames also undo cleanly.
        let mut s = surface(&mut pixels, 16, 12, 64);
        s.xor_frame(Rect::new(-3, 6, 30, 30), 3, clip);
        s.xor_frame(Rect::new(-3, 6, 30, 30), 3, clip);
        assert_eq!(pixels, original);
        let mut b = vec![0u32; 10 * 10];
        let mut bs = surface(&mut b, 10, 10, 40);
        bs.bevel(Rect::new(2, 2, 6, 6), true, bs.bounds(), 0xffffff, 0x000001);
        assert_eq!(b[2 * 10 + 2], 0xffffff);
        assert_eq!(b[7 * 10 + 7], 0x000001);
        assert_eq!(b[3 * 10 + 3], 0, "the inside is left alone");
        assert_eq!(b[1 * 10 + 1], 0, "and nothing outside is touched");
    }

    #[test]
    fn the_pointer_restores_the_pixels_it_covered_anywhere_on_screen() {
        let mut pixels: Vec<u32> = (0..40 * 30).map(|i| 0x0100_0000 + i as u32).collect();
        let original = pixels.clone();
        let mut cursor = Cursor::new();
        for (x, y) in [
            (5, 5),
            (0, 0),
            (30, 20),
            (-6, -8),
            (35, 25),
            (39, 29),
            (-50, 3),
            (100, 100),
        ] {
            let mut s = surface(&mut pixels, 40, 30, 160);
            cursor.show(&mut s, x, y);
            assert!(cursor.visible());
            if (0..40).contains(&x) && (0..30).contains(&y) {
                assert_ne!(pixels, original, "pointer at ({x},{y}) drew nothing");
            }
            let mut s = surface(&mut pixels, 40, 30, 160);
            cursor.hide(&mut s);
            assert!(!cursor.visible());
            assert_eq!(pixels, original, "pointer at ({x},{y}) left residue");
        }
        // Moving without hiding first also restores the old spot.
        let mut s = surface(&mut pixels, 40, 30, 160);
        cursor.show(&mut s, 3, 3);
        cursor.show(&mut s, 20, 10);
        cursor.hide(&mut s);
        assert_eq!(pixels, original);
        // Hiding twice is harmless.
        let mut s = surface(&mut pixels, 40, 30, 160);
        cursor.hide(&mut s);
        assert_eq!(pixels, original);
    }

    #[test]
    fn the_arrow_sprite_is_well_formed() {
        assert!(ARROW.iter().all(|row| row.len() == CURSOR_W));
        assert!(ARROW
            .iter()
            .flat_map(|r| r.iter())
            .all(|c| matches!(c, b'#' | b'.' | b' ')));
        assert_eq!(ARROW[0][0], b'#', "the hot spot is the top-left corner");
    }
}
