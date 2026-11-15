//! The ZOS window manager server. In the desktop boot entry it runs in place of the
//! text console: it owns the framebuffer and the console channel, so the shell's
//! WRITE/LINE/CLEAR messages land in a terminal window, and the input service's
//! pointer and Alt-key messages drive the windows. Policy lives in the library
//! (`zos_wm`); this file only connects it to the system.

#![no_std]
#![no_main]

use zos_console::{ALT, CLEAR, COLORS, CONSOLE, LINE, POINTER, SCROLLS, WRITE};
use zos_libos as os;
use zos_wm::{
    draw::{Cursor, Surface, GLYPH_H, GLYPH_W},
    geometry::Rect,
    grid::{Grid, COLS, ROWS},
    windows::{Effects, Kind, Manager, State, BORDER, MAX_WINDOWS, TITLE_HEIGHT},
};

const DESKTOP: u32 = 0x1c3f6e;
const FRAME: u32 = 0xc8c8c8;
const LIGHT: u32 = 0xffffff;
const DARK: u32 = 0x606060;
const TITLE_ACTIVE: u32 = 0x0a5c36;
const TITLE_INACTIVE: u32 = 0x4a4a4a;
const TEXT: u32 = 0x000000;
const DIMMED: u32 = 0x909090;
const HELP_BACKGROUND: u32 = 0xf4f4e8;

const HELP: [&[u8]; 11] = [
    b"ZOS window manager",
    b"",
    b"Alt+Tab        next window",
    b"Alt+`          previous window",
    b"Alt+Arrows     move the window",
    b"Alt+M          minimize",
    b"Alt+X          maximize / restore",
    b"Alt+W          close (not the terminal)",
    b"Alt+H          show / hide this help",
    b"",
    b"Mouse: drag a title bar; use its buttons.",
];

struct Desktop<'a> {
    surface: Surface<'a>,
    manager: Manager,
    grid: Grid,
    cursor: Cursor,
    /// The drag outline currently drawn on screen (XOR), if any.
    outline: Option<Rect>,
    /// The outline the drag wants drawn (kept across repaints).
    wanted_outline: Option<Rect>,
}

impl Desktop<'_> {
    fn terminal_slot(&self) -> Option<usize> {
        self.manager
            .stack()
            .find(|(_, w)| w.kind == Kind::Terminal)
            .map(|(slot, _)| slot)
    }

    // ---- Painting ----------------------------------------------------------
    fn paint(&mut self, clip: Rect) {
        let clip = clip.intersect(self.surface.bounds());
        if clip.is_empty() {
            return;
        }
        let work = self.manager.work_area();
        self.surface.fill(work, clip, DESKTOP);
        let mut slots = [0usize; MAX_WINDOWS];
        let mut count = 0;
        for (slot, _) in self.manager.stack() {
            slots[count] = slot;
            count += 1;
        }
        for &slot in &slots[..count] {
            self.paint_window(slot, clip);
        }
        self.paint_taskbar(clip);
    }

    fn paint_window(&mut self, slot: usize, clip: Rect) {
        let outer = self.manager.visible_rect(slot);
        let clip = clip.intersect(outer);
        let Some(window) = self.manager.window(slot).copied() else {
            return;
        };
        if clip.is_empty() {
            return;
        }
        let active = self.manager.focused() == Some(slot);
        self.surface.fill(outer, clip, FRAME);
        self.surface.bevel(outer, true, clip, LIGHT, DARK);
        let title = self.manager.title_rect(slot);
        self.surface.fill(
            title,
            clip,
            if active { TITLE_ACTIVE } else { TITLE_INACTIVE },
        );
        let (close, maximize, minimize) = self.manager.buttons(slot);
        let text_clip = clip.intersect(Rect::new(
            title.x,
            title.y,
            minimize.x - title.x - 2,
            title.h,
        ));
        self.surface.text(
            title.x + 6,
            title.y + (TITLE_HEIGHT - GLYPH_H) / 2,
            window.title(),
            if active { LIGHT } else { 0xd0d0d0 },
            None,
            text_clip,
        );
        for (button, label, enabled) in [
            (minimize, b'-', true),
            (
                maximize,
                if window.state == State::Maximized {
                    b'='
                } else {
                    b'+'
                },
                true,
            ),
            (close, b'x', window.closable),
        ] {
            self.surface.fill(button, clip, FRAME);
            self.surface.bevel(button, true, clip, LIGHT, DARK);
            self.surface.glyph(
                button.x + (button.w - GLYPH_W) / 2,
                button.y + (button.h - GLYPH_H) / 2,
                label,
                if enabled { TEXT } else { DIMMED },
                None,
                clip.intersect(button),
            );
        }
        let content = self.manager.content_rect(slot);
        match window.kind {
            Kind::Terminal => self.paint_terminal(content, clip.intersect(content), active),
            Kind::Help => {
                self.surface.fill(content, clip, HELP_BACKGROUND);
                for (line, text) in HELP.iter().enumerate() {
                    self.surface.text(
                        content.x + 10,
                        content.y + 10 + line as i32 * GLYPH_H,
                        text,
                        TEXT,
                        None,
                        clip.intersect(content),
                    );
                }
            }
        }
    }

    fn paint_terminal(&mut self, content: Rect, clip: Rect, active: bool) {
        if clip.is_empty() {
            return;
        }
        // Every cell paints its own background, so only the margin around the grid is
        // filled: filling the whole area first made output visibly blink on real hardware.
        let grid_w = COLS as i32 * GLYPH_W;
        let grid_h = ROWS as i32 * GLYPH_H;
        if content.w > grid_w {
            let strip = Rect::new(content.x + grid_w, content.y, content.w - grid_w, content.h);
            self.surface.fill(strip, clip, zos_wm::grid::DEFAULT_BG);
        }
        if content.h > grid_h {
            let strip = Rect::new(content.x, content.y + grid_h, grid_w, content.h - grid_h);
            self.surface.fill(strip, clip, zos_wm::grid::DEFAULT_BG);
        }
        for row in 0..ROWS {
            let y = content.y + row as i32 * GLYPH_H;
            if y + GLYPH_H <= clip.y || y >= clip.bottom() {
                continue;
            }
            for col in 0..COLS {
                let x = content.x + col as i32 * GLYPH_W;
                if x + GLYPH_W <= clip.x || x >= clip.right() {
                    continue;
                }
                let cell = self.grid.cell(row, col);
                self.surface
                    .glyph(x, y, cell.byte, cell.fg, Some(cell.bg), clip);
                if active && row == self.grid.y && col == self.grid.x {
                    self.surface
                        .fill(Rect::new(x, y + 14, GLYPH_W, 2), clip, cell.fg);
                }
            }
        }
    }

    fn paint_taskbar(&mut self, clip: Rect) {
        let bar = self.manager.taskbar();
        let area = clip.intersect(bar);
        if area.is_empty() {
            return;
        }
        self.surface.fill(bar, area, FRAME);
        self.surface
            .fill(Rect::new(bar.x, bar.y, bar.w, 1), area, LIGHT);
        let launcher = self.manager.launcher_rect();
        self.surface.fill(launcher, area, FRAME);
        self.surface.bevel(launcher, true, area, LIGHT, DARK);
        self.surface.text(
            launcher.x + 12,
            launcher.y + 3,
            b"ZOS",
            TEXT,
            None,
            area.intersect(launcher),
        );
        // Always-visible hint: the keyboard works everywhere, the pointer may not.
        let hint: &[u8] = b"Alt+H: keyboard help";
        let hint_x = bar.x + bar.w - hint.len() as i32 * GLYPH_W - 12;
        self.surface
            .text(hint_x, bar.y + 6, hint, DIMMED, None, area);
        let mut buttons = [(0usize, Rect::EMPTY); MAX_WINDOWS];
        let mut count = 0;
        for entry in self.manager.task_buttons() {
            buttons[count] = entry;
            count += 1;
        }
        for &(slot, button) in &buttons[..count] {
            let Some(window) = self.manager.window(slot).copied() else {
                continue;
            };
            let pressed = self.manager.focused() == Some(slot) && window.state != State::Minimized;
            self.surface.fill(button, area, FRAME);
            self.surface.bevel(button, !pressed, area, LIGHT, DARK);
            let room = (button.w / GLYPH_W - 2).max(0) as usize;
            let title = window.title();
            self.surface.text(
                button.x + GLYPH_W,
                button.y + 3,
                &title[..title.len().min(room)],
                if window.state == State::Minimized {
                    DIMMED
                } else {
                    TEXT
                },
                None,
                area.intersect(button),
            );
        }
    }

    /// Repaints rectangles and redraws the overlays (drag outline, pointer) on top.
    fn apply(&mut self, fx: &Effects) {
        if let Some((_, new)) = fx.outline {
            self.wanted_outline = if new.is_empty() { None } else { Some(new) };
        }
        self.cursor.hide(&mut self.surface);
        let bounds = self.surface.bounds();
        if let Some(outline) = self.outline.take() {
            self.surface.xor_frame(outline, 3, bounds);
        }
        for &rect in fx.damage() {
            self.paint(rect);
        }
        if let Some(outline) = self.wanted_outline.filter(|_| self.manager.dragging()) {
            self.surface.xor_frame(outline, 3, bounds);
            self.outline = Some(outline);
        } else {
            self.wanted_outline = None;
        }
        let (x, y) = self.manager.pointer();
        self.cursor.show(&mut self.surface, x, y);
    }

    /// Repaints the terminal rows that changed since the last call.
    fn flush_terminal(&mut self) {
        let mask = self.grid.take_dirty();
        let Some(slot) = self.terminal_slot() else {
            return;
        };
        if mask == 0 {
            return;
        }
        let content = self.manager.content_rect(slot);
        let first = mask.trailing_zeros() as i32;
        let last = 31 - mask.leading_zeros() as i32;
        let rows = Rect::new(
            content.x,
            content.y + first * GLYPH_H,
            COLS as i32 * GLYPH_W,
            (last - first + 1) * GLYPH_H,
        );
        let visible = rows.intersect(self.manager.visible_rect(slot));
        // Fast path: when nothing else covers those rows (and no drag is in progress),
        // redraw only the changed text rows instead of the whole window stack.
        let covered = self.outline.is_some()
            || (0..MAX_WINDOWS).any(|other| {
                other != slot
                    && self.manager.window(other).is_some()
                    && !self
                        .manager
                        .visible_rect(other)
                        .intersect(visible)
                        .is_empty()
            })
            || self.manager.window(slot).map(|w| w.state) == Some(State::Minimized);
        if !covered {
            let active = self.manager.focused() == Some(slot);
            self.cursor.hide(&mut self.surface);
            self.paint_terminal(content, visible, active);
            let (x, y) = self.manager.pointer();
            self.cursor.show(&mut self.surface, x, y);
            return;
        }
        let mut fx = Effects::default();
        fx.add(visible);
        self.apply(&fx);
    }
}

fn describe(manager: &Manager) {
    for slot in 0..MAX_WINDOWS {
        if let Some(window) = manager.window(slot) {
            let r = manager.visible_rect(slot);
            zos_console::log(format_args!(
                "WM WIN {slot} {:?} {},{} {}x{} focus={}",
                window.state,
                r.x,
                r.y,
                window.rect.w,
                window.rect.h,
                u8::from(manager.focused() == Some(slot)),
            ));
        }
    }
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(CONSOLE));
    assert_eq!(os::io_port_grant(0x60, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(CONSOLE, 1), Err(os::Error::Denied));
    let fb = os::framebuffer_map().unwrap();
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    // SAFETY: this process exclusively owns the mapped RGB32 framebuffer.
    let pixels = unsafe { core::slice::from_raw_parts_mut(fb.address as *mut u32, fb.length / 4) };
    let splash = zos_console::splash::draw(pixels, fb);
    // SAFETY: as above; the logo drawing above has finished with the slice.
    let pixels = unsafe { core::slice::from_raw_parts_mut(fb.address as *mut u32, fb.length / 4) };
    let surface = Surface::new(pixels, fb.width, fb.height, fb.pitch, fb.shifts)
        .expect("Unusable framebuffer");
    let manager =
        Manager::new(fb.width as i32, fb.height as i32).expect("Screen too small for a desktop");
    let mut desktop = Desktop {
        surface,
        manager,
        grid: Grid::new(),
        cursor: Cursor::new(),
        outline: None,
        wanted_outline: None,
    };
    let terminal = Rect::new(
        40,
        36,
        COLS as i32 * GLYPH_W + 2 * BORDER,
        ROWS as i32 * GLYPH_H + TITLE_HEIGHT + 2 * BORDER,
    );
    desktop
        .manager
        .create(Kind::Terminal, b"ZOS Terminal", terminal, false)
        .expect("Cannot create the terminal window");
    os::debug_write("WM ready: desktop, terminal window and window manager").unwrap();
    let mut splash_up = splash;
    if !splash_up {
        let full = desktop.surface.bounds();
        let mut fx = Effects::default();
        fx.add(full);
        desktop.apply(&fx);
    }
    let mut bytes = [0; 128];
    loop {
        let (token, length) = os::receive(CONSOLE, &mut bytes).unwrap();
        assert!(token != 0 && length > 0);
        let message = &bytes[..length];
        let pointer = matches!(message, [POINTER, ..]);
        let query = matches!(message, [SCROLLS]);
        // Anything but pointer motion or a query is real output (the shell banner,
        // an error): the logo makes way for the desktop.
        if splash_up && !pointer && !query {
            splash_up = false;
            let full = desktop.surface.bounds();
            let mut fx = Effects::default();
            fx.add(full);
            desktop.grid.invalidate();
            desktop.apply(&fx);
        }
        let before = (desktop.manager.focused(), desktop.manager.window_count());
        match message {
            [WRITE, text @ ..] => desktop.grid.write(text),
            [COLORS, fr, fg, fb, br, bg, bb] => desktop.grid.colors(
                u32::from_be_bytes([0, *fr, *fg, *fb]),
                u32::from_be_bytes([0, *br, *bg, *bb]),
            ),
            [CLEAR] => desktop.grid.clear(),
            [LINE, cursor, text @ ..] => desktop.grid.line(text, *cursor as usize),
            [POINTER, x0, x1, y0, y1, buttons] => {
                let (dx, dy) = (
                    i16::from_le_bytes([*x0, *x1]),
                    i16::from_le_bytes([*y0, *y1]),
                );
                if !splash_up {
                    let fx = desktop
                        .manager
                        .pointer_event(dx.into(), dy.into(), *buttons);
                    desktop.apply(&fx);
                    let (x, y) = desktop.manager.pointer();
                    zos_console::log(format_args!("MOUSE: x={x} y={y} buttons={buttons}"));
                    if !fx.is_empty() {
                        describe(&desktop.manager);
                    }
                } else {
                    os::reply_live(token, &[]).unwrap();
                    continue;
                }
            }
            [ALT, key] => {
                if !splash_up {
                    let fx = desktop.manager.alt(*key);
                    desktop.apply(&fx);
                    describe(&desktop.manager);
                }
            }
            [SCROLLS] => {
                os::reply(token, &(desktop.grid.scrolls as u64).to_le_bytes()).unwrap();
                continue;
            }
            _ => panic!("Invalid console protocol"),
        }
        if !splash_up {
            desktop.flush_terminal();
        }
        if (desktop.manager.focused(), desktop.manager.window_count()) != before && pointer {
            describe(&desktop.manager);
        }
        os::reply_live(token, &[]).unwrap();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    zos_console::fail(info)
}
