use font8x8::{UnicodeFonts, BASIC_FONTS};
use zos_libos::Framebuffer;

const COLUMNS: usize = 128;
const ROWS: usize = 48;
const FOREGROUND: u32 = 0x90ee90;
const BACKGROUND: u32 = 0x081008;

#[derive(Clone, Copy)]
struct Cell {
    byte: u8,
    foreground: u32,
    background: u32,
}
const BLANK: Cell = Cell {
    byte: b' ',
    foreground: FOREGROUND,
    background: BACKGROUND,
};

pub struct Terminal {
    cells: [Cell; COLUMNS * ROWS],
    dirty: [bool; ROWS],
    columns: usize,
    rows: usize,
    pub x: usize,
    pub y: usize,
    foreground: u32,
    background: u32,
    pub scrolls: usize,
    pub mouse_x: usize,
    pub mouse_y: usize,
    pub buttons: u8,
}

impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}

impl Terminal {
    pub const fn new() -> Self {
        Self {
            cells: [BLANK; COLUMNS * ROWS],
            dirty: [true; ROWS],
            columns: COLUMNS,
            rows: ROWS,
            x: 0,
            y: 0,
            foreground: FOREGROUND,
            background: BACKGROUND,
            scrolls: 0,
            mouse_x: 512,
            mouse_y: 384,
            buttons: 0,
        }
    }

    pub fn configure(&mut self, width: usize, height: usize) {
        assert!(width % 8 == 0 && height % 16 == 0);
        assert!((64..=COLUMNS).contains(&(width / 8)));
        assert!((2..=ROWS).contains(&(height / 16)));
        self.columns = width / 8;
        self.rows = height / 16;
        self.mouse_x = width / 2;
        self.mouse_y = height / 2;
        self.clear();
    }

    fn blank(&self) -> Cell {
        Cell {
            byte: b' ',
            foreground: self.foreground,
            background: self.background,
        }
    }
    pub fn colors(&mut self, foreground: u32, background: u32) {
        self.foreground = foreground;
        self.background = background;
    }
    pub fn clear(&mut self) {
        let blank = self.blank();
        self.cells.fill(blank);
        self.dirty.fill(true);
        self.x = 0;
        self.y = 0;
    }
    fn newline(&mut self) {
        self.x = 0;
        self.y += 1;
        if self.y == self.rows {
            self.cells.copy_within(COLUMNS..self.rows * COLUMNS, 0);
            let blank = self.blank();
            self.cells[(self.rows - 1) * COLUMNS..self.rows * COLUMNS].fill(blank);
            self.dirty.fill(true);
            self.y -= 1;
            self.scrolls += 1;
        }
    }
    pub fn write(&mut self, text: &[u8]) {
        for &byte in text {
            self.dirty[self.y] = true;
            match byte {
                b'\n' => self.newline(),
                b'\r' => self.x = 0,
                8 if self.x > 0 => {
                    self.x -= 1;
                    self.cells[self.y * COLUMNS + self.x] = self.blank();
                }
                32..=126 => {
                    if self.x == self.columns {
                        self.newline();
                    }
                    self.cells[self.y * COLUMNS + self.x] = Cell {
                        byte,
                        ..self.blank()
                    };
                    self.x += 1;
                }
                _ => (),
            }
            self.dirty[self.y] = true;
        }
    }
    pub fn line(&mut self, bytes: &[u8], cursor: usize) {
        assert!(bytes.len() <= crate::editor::CAPACITY && cursor <= bytes.len());
        let blank = self.blank();
        self.cells[self.y * COLUMNS..(self.y + 1) * COLUMNS].fill(blank);
        self.x = 0;
        self.write(b"> ");
        self.write(bytes);
        self.x = cursor + 2;
        self.dirty[self.y] = true;
    }
    pub fn pointer(&mut self, dx: i16, dy: i16, buttons: u8) {
        self.dirty[self.mouse_y / 16] = true;
        if self.mouse_y / 16 > 0 {
            self.dirty[self.mouse_y / 16 - 1] = true;
        }
        if self.mouse_y / 16 + 1 < self.rows {
            self.dirty[self.mouse_y / 16 + 1] = true;
        }
        self.mouse_x = (self.mouse_x as isize + dx as isize)
            .clamp(0, (self.columns * 8 - 1) as isize) as usize;
        self.mouse_y =
            (self.mouse_y as isize - dy as isize).clamp(0, (self.rows * 16 - 1) as isize) as usize;
        self.buttons = buttons;
    }
    pub fn render(&mut self, pixels: &mut [u32], fb: Framebuffer) {
        assert!(fb.width == self.columns * 8 && fb.height == self.rows * 16);
        assert!(fb.pitch % 4 == 0 && fb.pitch / 4 >= fb.width);
        assert!(pixels.len() >= fb.pitch / 4 * fb.height);
        let color = |rgb: u32| {
            ((rgb >> 16 & 255) << fb.shifts[0])
                | ((rgb >> 8 & 255) << fb.shifts[1])
                | ((rgb & 255) << fb.shifts[2])
        };
        let mut pixel = |x: usize, y: usize, value| {
            // SAFETY: slice bounds are checked and the caller owns this surface.
            unsafe { core::ptr::write_volatile(&mut pixels[y * (fb.pitch / 4) + x], value) };
        };
        for row in 0..self.rows {
            if !self.dirty[row] {
                continue;
            }
            for col in 0..self.columns {
                let cell = self.cells[row * COLUMNS + col];
                let glyph = BASIC_FONTS.get(cell.byte as char).unwrap();
                for y in 0..16 {
                    for x in 0..8 {
                        let cursor = row == self.y && col == self.x && y >= 14;
                        let rgb = if cursor || glyph[y / 2] & (1 << x) != 0 {
                            cell.foreground
                        } else {
                            cell.background
                        };
                        pixel(col * 8 + x, row * 16 + y, color(rgb));
                    }
                }
            }
            self.dirty[row] = false;
        }
        for delta in -4_isize..=4 {
            let x = self.mouse_x as isize + delta;
            let y = self.mouse_y as isize + delta;
            let ink = color(if self.buttons == 0 {
                0xffffff
            } else {
                0xff6060
            });
            if (0..fb.width as isize).contains(&x) {
                pixel(x as usize, self.mouse_y, ink);
            }
            if (0..fb.height as isize).contains(&y) {
                pixel(self.mouse_x, y as usize, ink);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    #[test]
    fn scroll_backspace_colors_cursor_and_pointer_restore() {
        let mut terminal = Terminal::new();
        terminal.configure(512, 32);
        terminal.colors(0x123456, 0x654321);
        terminal.write(b"ab\x08c\nsecond\n");
        assert_eq!(terminal.scrolls, 1);
        assert_eq!(terminal.cells[0].byte, b's');
        assert_eq!(terminal.cells[0].foreground, 0x123456);
        assert_eq!(terminal.cells[COLUMNS].byte, b' ');
        terminal.line(b"edited", 3);
        assert_eq!((terminal.x, terminal.y), (5, 1));
        let fb = Framebuffer {
            address: 0,
            width: 512,
            height: 32,
            pitch: 512 * 4,
            shifts: [16, 8, 0],
            length: 512 * 32 * 4,
        };
        let mut pixels = std::vec![0; 512 * 32];
        terminal.render(&mut pixels, fb);
        assert_eq!(pixels[(16 + 14) * 512 + 5 * 8], 0x123456);
        let old = terminal.mouse_y * 512 + terminal.mouse_x;
        assert_eq!(pixels[old], 0xffffff);
        terminal.pointer(-256, 256, 1);
        terminal.render(&mut pixels, fb);
        assert_ne!(pixels[old], 0xffffff);
        assert_eq!((terminal.mouse_x, terminal.mouse_y), (0, 0));
        assert_eq!(pixels[0], 0xff6060);
        terminal.clear();
        terminal.write(b"ab\x08c");
        assert_eq!(terminal.cells[1].byte, b'c');
        assert_eq!(terminal.cells[2].byte, b' ');
    }
}
