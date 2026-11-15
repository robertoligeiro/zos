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
