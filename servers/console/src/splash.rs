//! Boot screen: the ZOS logo, shown by the console server until the first real
//! message (the shell's banner, or an error) arrives.
//!
//! The image is `assets/zos-splash.bin`, made by `tools/mksplash.py`: a square
//! 2-bits-per-pixel bitmap with a four-color palette. It is drawn 1:1 and centered
//! when the framebuffer is large enough, otherwise scaled down (nearest neighbor)
//! to fit. Every access is bounds checked, so any mode, pitch or odd size is safe.

use zos_libos::Framebuffer;

static IMAGE: &[u8] = include_bytes!("../../../assets/zos-splash.bin");
const HEADER: usize = 24;

struct Image<'a> {
    width: usize,
    height: usize,
    palette: [u32; 4],
    body: &'a [u8],
}

impl<'a> Image<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER || &data[..4] != b"ZSPL" {
            return None;
        }
        let width = u16::from_le_bytes([data[4], data[5]]) as usize;
        let height = u16::from_le_bytes([data[6], data[7]]) as usize;
        if width == 0 || height == 0 || data.len() != HEADER + height * width.div_ceil(4) {
            return None;
        }
        let mut palette = [0; 4];
        for (i, color) in palette.iter_mut().enumerate() {
            *color = u32::from_le_bytes(data[8 + i * 4..12 + i * 4].try_into().unwrap());
        }
        Some(Self {
            width,
            height,
            palette,
            body: &data[HEADER..],
        })
    }
    fn color(&self, x: usize, y: usize) -> u32 {
        let byte = self.body[y * self.width.div_ceil(4) + x / 4];
        self.palette[(byte >> (6 - 2 * (x % 4)) & 3) as usize]
    }
}

/// Draws the logo into the whole framebuffer. Returns false, drawing nothing, when
/// the image or the framebuffer description is unusable.
pub fn draw(pixels: &mut [u32], fb: Framebuffer) -> bool {
    draw_image(IMAGE, pixels, fb)
}

fn draw_image(data: &[u8], pixels: &mut [u32], fb: Framebuffer) -> bool {
    let Some(image) = Image::parse(data) else {
        return false;
    };
    let stride = fb.pitch / 4;
    if fb.width == 0
        || fb.height == 0
        || fb.pitch % 4 != 0
        || stride < fb.width
        || pixels.len() < stride * fb.height
        || fb.shifts.iter().any(|&shift| shift > 24)
    {
        return false;
    }
    let pack = |rgb: u32| {
        ((rgb >> 16 & 255) << fb.shifts[0])
            | ((rgb >> 8 & 255) << fb.shifts[1])
            | ((rgb & 255) << fb.shifts[2])
    };
    let background = pack(image.palette[0]);
    for y in 0..fb.height {
        for x in 0..fb.width {
            // SAFETY: the index is below stride * height, checked against the slice above.
            unsafe { core::ptr::write_volatile(&mut pixels[y * stride + x], background) };
        }
    }
    // Largest square that fits, never enlarged past the image's own size.
    let fit = fb.width.min(fb.height);
    let (draw_width, draw_height) = if fit >= image.width && fit >= image.height {
        (image.width, image.height)
    } else {
        (
            (image.width * fit / image.width.max(image.height)).max(1),
            (image.height * fit / image.width.max(image.height)).max(1),
        )
    };
    let (left, top) = ((fb.width - draw_width) / 2, (fb.height - draw_height) / 2);
    for y in 0..draw_height {
        let source_y = y * image.height / draw_height;
        for x in 0..draw_width {
            let color = pack(image.color(x * image.width / draw_width, source_y));
            // SAFETY: left + x < fb.width and top + y < fb.height by construction.
            unsafe { core::ptr::write_volatile(&mut pixels[(top + y) * stride + left + x], color) };
        }
    }
    true
}

/// Timer ticks (100 Hz) per animation frame.
pub const TICKS_PER_FRAME: u32 = 12;

/// The palm and wrist of the zombie hand, 16 columns by rows 5..=15 (`#` is a pixel).
const PALM: [&[u8]; 11] = [
    b".###########...",
    b".############..",
    b".#############.",
    b".#############.",
    b"..############.",
    b"..###########..",
    b"...#########...",
    b"...#########...",
    b"....#######....",
    b"....#######....",
    b"....#######....",
];

/// The 16x16 zombie hand at `openness` 0 (fingers spread) to 3 (closed fist), one
/// `u16` per row with the leftmost pixel in the top bit.
pub fn hand(openness: usize) -> [u16; 16] {
    let openness = openness.min(3);
    let mut rows = [0u16; 16];
    for (i, palm) in PALM.iter().enumerate() {
        for (x, &c) in palm.iter().enumerate() {
            if c == b'#' {
                rows[5 + i] |= 0x8000 >> x;
            }
        }
    }
    // Four fingers, two pixels wide; they shorten into the palm as the hand closes.
    for (x0, open_top) in [(1usize, 2usize), (4, 0), (7, 0), (10, 2)] {
        let top = open_top + (4 - open_top) * openness / 3;
        for row in rows.iter_mut().take(5).skip(top) {
            *row |= (0xc000u16) >> x0;
        }
    }
    rows
}

/// Draws one frame of the "OS Loading" animation (a zombie hand opening and closing,
/// then the text) over the logo. `step` counts timer ticks. Returns false, drawing
/// nothing, when the image or the framebuffer is unusable.
pub fn animate(pixels: &mut [u32], fb: Framebuffer, step: u32) -> bool {
    use font8x8::{UnicodeFonts, BASIC_FONTS};
    let Some(image) = Image::parse(IMAGE) else {
        return false;
    };
    let stride = fb.pitch / 4;
    if fb.width == 0
        || fb.height == 0
        || fb.pitch % 4 != 0
        || stride < fb.width
        || pixels.len() < stride * fb.height
        || fb.shifts.iter().any(|&shift| shift > 24)
    {
        return false;
    }
    let pack = |rgb: u32| {
        ((rgb >> 16 & 255) << fb.shifts[0])
            | ((rgb >> 8 & 255) << fb.shifts[1])
            | ((rgb & 255) << fb.shifts[2])
    };
    let mut put = |x: usize, y: usize, value: u32| {
        if x < fb.width && y < fb.height {
            // SAFETY: inside width x height, which the slice length covers (checked above).
            unsafe { core::ptr::write_volatile(&mut pixels[y * stride + x], value) };
        }
    };
    let size = fb.width.min(fb.height).min(image.width);
    let frame = (step / TICKS_PER_FRAME) as usize;
    let openness = [0, 1, 2, 3, 2, 1][frame % 6];
    let dots = (frame / 2) % 4;
    let hand_scale = (size / 192).max(1);
    let text_scale = (size / 384).max(1);
    let gap = 3 * text_scale;
    let pad = 4 * text_scale;
    let text_width = 13 * 8 * text_scale; // "OS Loading..." at its widest
    let hand_size = 16 * hand_scale;
    let panel_width = hand_size + gap * 3 + text_width + 2 * pad;
    let panel_height = hand_size + 2 * pad;
    // The logo picture's fake boot list (57% to 81% of its height) makes way for the
    // animation, which is centered in that band.
    let (logo_left, logo_top) = ((fb.width - size) / 2, (fb.height - size) / 2);
    let (band_left, band_top) = (logo_left + size * 3 / 100, logo_top + size * 565 / 1000);
    let (band_width, band_height) = (size * 94 / 100, size * 245 / 1000);
    let left = band_left + band_width.saturating_sub(panel_width) / 2;
    let top = band_top + band_height.saturating_sub(panel_height) / 2;
    let (background, white, green) = (
        pack(image.palette[0]),
        pack(image.palette[3]),
        pack(0x7ed957),
    );
    for y in band_top..band_top + band_height {
        for x in band_left..band_left + band_width {
            put(x, y, background);
        }
    }
    let rows = hand(openness);
    for (row, bits) in rows.iter().enumerate() {
        for column in 0..16 {
            if bits & (0x8000 >> column) == 0 {
                continue;
            }
            for dy in 0..hand_scale {
                for dx in 0..hand_scale {
                    put(
                        left + pad + column * hand_scale + dx,
                        top + pad + row * hand_scale + dy,
                        green,
                    );
                }
            }
        }
    }
    let text_left = left + pad + hand_size + gap * 3;
    let text_top = top + pad + (hand_size.saturating_sub(8 * text_scale)) / 2;
    let mut text = [b'.'; 13];
    text[..10].copy_from_slice(b"OS Loading");
    let length = 10 + dots;
    for (index, &byte) in text[..length].iter().enumerate() {
        let glyph = BASIC_FONTS.get(byte as char).unwrap_or([0; 8]);
        for (gy, bits) in glyph.iter().enumerate() {
            for gx in 0..8 {
                if bits & (1 << gx) == 0 {
                    continue;
                }
                for dy in 0..text_scale {
                    for dx in 0..text_scale {
                        put(
                            text_left + (index * 8 + gx) * text_scale + dx,
                            text_top + gy * text_scale + dy,
                            white,
                        );
                    }
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const PALETTE: [u32; 4] = [0x000010, 0x404060, 0x8080a0, 0xffffff];

    /// A `size` x `size` test image whose pixel (x, y) has shade (x + y) % 4.
    fn image(size: usize) -> Vec<u8> {
        let mut data = b"ZSPL".to_vec();
        data.extend((size as u16).to_le_bytes());
        data.extend((size as u16).to_le_bytes());
        for color in PALETTE {
            data.extend(color.to_le_bytes());
        }
        for y in 0..size {
            let mut row = std::vec![0u8; size.div_ceil(4)];
            for x in 0..size {
                row[x / 4] |= (((x + y) % 4) as u8) << (6 - 2 * (x % 4));
            }
            data.extend(row);
        }
        data
    }
    fn framebuffer(width: usize, height: usize, pitch: usize) -> Framebuffer {
        Framebuffer {
            address: 0,
            width,
            height,
            pitch,
            // The channel shifts a real BGRX framebuffer reports.
            shifts: [16, 8, 0],
            length: pitch * height,
        }
    }
    fn paint(data: &[u8], width: usize, height: usize, pitch: usize) -> (bool, Vec<u32>) {
        let mut pixels = std::vec![0xdead_beef; pitch / 4 * height];
        let ok = draw_image(data, &mut pixels, framebuffer(width, height, pitch));
        (ok, pixels)
    }

    #[test]
    fn draws_one_to_one_centered_on_a_larger_screen() {
        let (ok, pixels) = paint(&image(8), 20, 14, 20 * 4);
        assert!(ok);
        let at = |x: usize, y: usize| pixels[y * 20 + x];
        // Image origin lands at ((20-8)/2, (14-8)/2) = (6, 3).
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(at(6 + x, 3 + y), PALETTE[(x + y) % 4], "({x},{y})");
            }
        }
        // Everything else is the background color.
        assert_eq!(at(0, 0), PALETTE[0]);
        assert_eq!(at(19, 13), PALETTE[0]);
        assert_eq!(at(5, 5), PALETTE[0]);
        assert_eq!(at(14, 5), PALETTE[0]);
    }

    #[test]
    fn pitch_padding_is_left_alone_and_channel_order_follows_the_shifts() {
        let (ok, pixels) = paint(&image(4), 8, 4, 8 * 4 + 16);
        assert!(ok);
        let stride = 8 + 4;
        for y in 0..4 {
            for padding in 8..stride {
                assert_eq!(
                    pixels[y * stride + padding],
                    0xdead_beef,
                    "row {y} padding was touched"
                );
            }
        }
        // A framebuffer with red in the low byte swaps the packed channels.
        let mut swapped = std::vec![0; 8 * 4];
        let fb = Framebuffer {
            shifts: [0, 8, 16],
            ..framebuffer(8, 4, 32)
        };
        assert!(draw_image(&image(4), &mut swapped, fb));
        assert_eq!(swapped[0], 0x100000);
    }

    #[test]
    fn a_small_screen_gets_a_downscaled_logo_without_overrun() {
        for (width, height) in [(4, 4), (5, 3), (3, 7), (1, 1), (2, 9), (6, 6), (7, 5)] {
            let (ok, pixels) = paint(&image(8), width, height, width * 4);
            assert!(ok, "{width}x{height}");
            assert_eq!(pixels.len(), width * height);
            assert!(
                pixels.iter().all(|&p| PALETTE.contains(&p)),
                "{width}x{height}"
            );
        }
        // 4x4 from the 8x8 test image samples every other pixel: shade (2x+2y) % 4.
        let (_, pixels) = paint(&image(8), 4, 4, 16);
        assert_eq!(pixels[0], PALETTE[0]);
        assert_eq!(pixels[1], PALETTE[2]);
        assert_eq!(pixels[4 + 1], PALETTE[0]);
    }

    #[test]
    fn the_hand_opens_and_closes() {
        let count = |rows: [u16; 16]| rows.iter().map(|r| r.count_ones()).sum::<u32>();
        let open = hand(0);
        let fist = hand(3);
        assert_ne!(open, fist);
        assert!(count(open) > count(fist), "fingers are spread when open");
        // The first frame has fingertips on the top row; the fist has none above row 4.
        assert_ne!(open[0], 0);
        assert!(fist[..4].iter().all(|&r| r == 0));
        // Frames in between are monotone, and out-of-range openness means closed.
        assert!(count(hand(1)) >= count(hand(2)) && count(hand(2)) >= count(hand(3)));
        assert_eq!(hand(9), fist);
    }

    #[test]
    fn the_loading_animation_changes_each_frame_and_stays_on_screen() {
        // The embedded logo is 768 wide, so use screens around it, including odd ones.
        for (width, height) in [
            (1024, 768),
            (1920, 1080),
            (768, 768),
            (640, 480),
            (5, 3),
            (1, 1),
        ] {
            let fb = framebuffer(width, height, width * 4);
            let mut previous: Option<Vec<u32>> = None;
            for frame in 0..8u32 {
                let mut pixels = std::vec![0xdead_beef; width * height];
                assert!(
                    animate(&mut pixels, fb, frame * TICKS_PER_FRAME),
                    "{width}x{height}"
                );
                if width >= 640 {
                    // Outside the band, nothing is touched.
                    assert_eq!(pixels[0], 0xdead_beef);
                    assert_eq!(pixels[width * height - 1], 0xdead_beef);
                    if let Some(old) = &previous {
                        assert_ne!(old, &pixels, "{width}x{height} frame {frame}");
                    }
                }
                previous = Some(pixels);
            }
        }
        let mut pixels = std::vec![0; 16];
        let bad = Framebuffer {
            pitch: 6,
            ..framebuffer(4, 4, 16)
        };
        assert!(!animate(&mut pixels, bad, 0));
    }

    #[test]
    fn unusable_images_and_framebuffers_draw_nothing() {
        let good = image(4);
        let untouched = |ok: bool, pixels: &[u32]| !ok && pixels.iter().all(|&p| p == 0xdead_beef);
        // Bad magic, truncated, extended, zero size.
        let mut bad = good.clone();
        bad[0] = b'X';
        let (ok, pixels) = paint(&bad, 8, 8, 32);
        assert!(untouched(ok, &pixels));
        for length in [0, 3, HEADER, good.len() - 1] {
            let (ok, pixels) = paint(&good[..length], 8, 8, 32);
            assert!(untouched(ok, &pixels), "length {length}");
        }
        let mut longer = good.clone();
        longer.push(0);
        assert!(!paint(&longer, 8, 8, 32).0);
        let mut zero = good.clone();
        zero[4] = 0;
        zero[5] = 0;
        assert!(!paint(&zero, 8, 8, 32).0);
        // Framebuffer descriptions that do not make sense.
        for (width, height, pitch) in [(0, 8, 32), (8, 0, 32), (8, 8, 30), (8, 8, 16)] {
            assert!(
                !paint(&good, width, height, pitch).0,
                "{width}x{height} pitch {pitch}"
            );
        }
        // A pixel slice shorter than pitch * height.
        let mut short = std::vec![0u32; 10];
        assert!(!draw_image(&good, &mut short, framebuffer(8, 8, 32)));
        assert!(!draw_image(
            &good,
            &mut std::vec![0u32; 64],
            Framebuffer {
                shifts: [25, 8, 0],
                ..framebuffer(8, 8, 32)
            }
        ));
    }

    #[test]
    fn the_real_logo_is_embedded_and_valid() {
        let image =
            Image::parse(IMAGE).expect("assets/zos-splash.bin must be a valid splash image");
        assert_eq!((image.width, image.height), (768, 768));
        // The kernel paints this background while it boots (kernel/src/framebuffer.rs).
        assert_eq!(image.palette[0], 0x011d94);
        assert_eq!(image.palette[3] >> 16, 0xfe);
        // It must fill a typical 1024x768 mode exactly as drawn 1:1.
        let (ok, pixels) = paint(IMAGE, 1024, 768, 1024 * 4);
        assert!(ok);
        assert_eq!(pixels[0], image.palette[0]);
        assert!(pixels.iter().any(|&p| p == image.palette[3]));
    }
}
