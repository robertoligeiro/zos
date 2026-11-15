use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use font8x8::{UnicodeFonts, BASIC_FONTS};
use spin::Mutex;

static TERMINAL: Mutex<Option<Terminal>> = Mutex::new(None);
/// Normal builds keep the boot screen clean: after the framebuffer is up, kernel log
/// text goes to the serial port only, until a fault needs to be shown (`reveal`).
static QUIET: AtomicBool = AtomicBool::new(false);
/// Background of the boot logo (tools\mksplash.py palette[0]); the console server
/// draws the logo over the same color, so the screen never flashes.
const BOOT_BACKGROUND: (u32, u32, u32) = (0x01, 0x1d, 0x94);

struct Terminal {
    base: usize,
    pitch: usize,
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    foreground: u32,
    background: u32,
    /// Background used for fault text (the dark green of the test/boot console).
    fault_background: u32,
}

impl Terminal {
    fn pixel(&mut self, x: usize, y: usize, color: u32) {
        assert!(x < self.width && y < self.height);
        let address = self.base + y * self.pitch + x * 4;
        // SAFETY: the validated RGB32 framebuffer covers this aligned pixel address.
        unsafe { (address as *mut u32).write_volatile(color) };
    }

    fn clear(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.pixel(x, y, self.background);
            }
        }
        self.x = 0;
        self.y = 0;
    }

    fn newline(&mut self) {
        self.x = 0;
        self.y += 16;
        if self.y + 16 > self.height {
            // This boot console wraps; scrolling belongs to the Step 5 console server.
            self.clear();
        }
    }

    fn character(&mut self, ch: char) {
        if ch == '\n' {
            self.newline();
            return;
        }
        if self.x + 8 > self.width {
            self.newline();
        }
        let glyph = BASIC_FONTS.get(ch).unwrap_or([0; 8]);
        for row in 0..16 {
            for column in 0..8 {
                let color = if glyph[row / 2] & (1 << column) != 0 {
                    self.foreground
                } else {
                    self.background
                };
                self.pixel(self.x + column, self.y + row, color);
            }
        }
        self.x += 8;
    }
}

impl Write for Terminal {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for ch in text.chars() {
            self.character(ch);
        }
        Ok(())
    }
}

pub fn init() {
    let response = crate::boot::FRAMEBUFFER
        .response()
        .expect("Framebuffer missing");
    let fb = response
        .framebuffers()
        .first()
        .expect("No framebuffer returned");
    assert_eq!(fb.bpp, 32, "Boot console requires RGB32");
    assert_eq!(fb.memory_model, limine::framebuffer::FRAMEBUFFER_RGB);
    assert!(fb.width >= 8 && fb.height >= 16);
    assert!(fb.pitch >= fb.width * 4 && fb.pitch % 4 == 0);
    assert_eq!(fb.address() as usize % 4, 0);
    for (size, shift) in [
        (fb.red_mask_size, fb.red_mask_shift),
        (fb.green_mask_size, fb.green_mask_shift),
        (fb.blue_mask_size, fb.blue_mask_shift),
    ] {
        assert_eq!(size, 8, "Boot console requires 8-bit RGB channels");
        assert!(shift <= 24);
    }
    let color = |r: u32, g: u32, b: u32| {
        (r << fb.red_mask_shift) | (g << fb.green_mask_shift) | (b << fb.blue_mask_shift)
    };
    // Acceptance-test kernels keep their log text on screen for review.
    let quiet = crate::ACCEPTANCE_FEATURES == 0;
    let fault_background = color(0x08, 0x10, 0x08);
    let mut terminal = Terminal {
        base: fb.address() as usize,
        pitch: fb.pitch as usize,
        width: fb.width as usize,
        height: fb.height as usize,
        x: 0,
        y: 0,
        foreground: color(0x90, 0xee, 0x90),
        background: if quiet {
            color(BOOT_BACKGROUND.0, BOOT_BACKGROUND.1, BOOT_BACKGROUND.2)
        } else {
            fault_background
        },
        fault_background,
    };
    terminal.clear();
    *TERMINAL.lock() = Some(terminal);
    crate::logln!(
        "FRAMEBUFFER ready: {}x{}, embedded 8x16 font",
        fb.width,
        fb.height
    );
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Ends quiet mode with a cleared, readable screen so fault text is visible. Safe to
/// call from fault handlers: it never waits for a lock.
pub fn reveal() {
    if QUIET.swap(false, Ordering::Relaxed) {
        if let Some(mut guard) = TERMINAL.try_lock() {
            if let Some(terminal) = guard.as_mut() {
                terminal.background = terminal.fault_background;
                terminal.clear();
            }
        }
    }
}

pub fn clear() {
    TERMINAL
        .lock()
        .as_mut()
        .expect("Framebuffer not initialized")
        .clear();
}

pub fn handoff() {
    *TERMINAL.lock() = None;
}

pub fn print(args: fmt::Arguments<'_>) {
    if QUIET.load(Ordering::Relaxed) {
        return;
    }
    // Fault-time screen output is best effort; never deadlock on an interrupted writer.
    if let Some(mut terminal) = TERMINAL.try_lock() {
        if let Some(terminal) = terminal.as_mut() {
            terminal
                .write_fmt(args)
                .expect("Framebuffer formatting failed");
        }
    }
}
