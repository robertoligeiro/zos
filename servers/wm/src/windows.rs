//! Window management: the window table, stacking order, focus, the pointer state
//! machine (click, drag, buttons), keyboard shortcuts and the taskbar. Nothing here
//! draws or talks to the system; every operation returns the screen rectangles that
//! need repainting, so the whole policy can be tested on the host.

use crate::geometry::Rect;
use zos_console::keyboard::{ALT_DOWN, ALT_LEFT, ALT_RIGHT, ALT_UP};

pub const MAX_WINDOWS: usize = 8;
pub const TITLE_LEN: usize = 24;
pub const BORDER: i32 = 3;
pub const TITLE_HEIGHT: i32 = 20;
pub const TASKBAR_HEIGHT: i32 = 28;
pub const BUTTON: i32 = 16;
pub const MOVE_STEP: i32 = 16;
/// How much of a window's title bar must stay reachable on screen.
const REACHABLE: i32 = 48;
const TASK_WIDTH: i32 = 148;
const TASK_GAP: i32 = 4;
const TASKS_LEFT: i32 = 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Normal,
    Minimized,
    Maximized,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// The shell's terminal.
    Terminal,
    /// The key-binding help window.
    Help,
    /// A desktop application's window (`index` into the application table).
    App(u8),
}

/// What a launcher menu entry does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Launch {
    /// Show or hide the help window (the window manager does it itself).
    Help,
    /// Start program `n` of `zos_console::gui::APPS`.
    App(u8),
}

/// The launcher menu, top to bottom. Entries 1.. are `gui::APPS` in order.
pub const MENU: [(&[u8], Launch); 3] = [
    (b"Help", Launch::Help),
    (b"Browser", Launch::App(0)),
    (b"Calculator", Launch::App(1)),
];
pub const MENU_ITEM_HEIGHT: i32 = 22;
pub const MENU_WIDTH: i32 = 168;

/// Names a window; the generation makes a stale handle fail after its slot is reused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Handle(pub u32);

#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub kind: Kind,
    pub state: State,
    /// Outer rectangle while in the `Normal` state.
    pub rect: Rect,
    pub closable: bool,
    title: [u8; TITLE_LEN],
    title_length: usize,
    /// State to return to when un-minimized.
    previous: State,
}

impl Window {
    pub fn title(&self) -> &[u8] {
        &self.title[..self.title_length]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Title,
    Close,
    Minimize,
    Maximize,
    Content,
    Border,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Desktop,
    Taskbar,
    Launcher,
    /// An entry of the open launcher menu.
    MenuItem(usize),
    Task(usize),
    Window(usize, Part),
}

/// What changed: rectangles to repaint, and the drag outline to move (erase the
/// first, draw the second; either may be empty).
#[derive(Clone, Copy, Default, Debug)]
pub struct Effects {
    rects: [Rect; 6],
    count: usize,
    pub outline: Option<(Rect, Rect)>,
}

impl Effects {
    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        if self.count < self.rects.len() {
            self.rects[self.count] = rect;
            self.count += 1;
        } else {
            // Out of slots: merge into the last one rather than lose damage.
            self.rects[self.count - 1] = self.rects[self.count - 1].union(rect);
        }
    }
    pub fn damage(&self) -> &[Rect] {
        &self.rects[..self.count]
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0 && self.outline.is_none()
    }
    fn merge(&mut self, other: Effects) {
        for &rect in other.damage() {
            self.add(rect);
        }
        if other.outline.is_some() {
            self.outline = other.outline;
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Drag {
    slot: usize,
    grab_x: i32,
    grab_y: i32,
    outline: Rect,
}

pub struct Manager {
    screen: Rect,
    windows: [Option<Window>; MAX_WINDOWS],
    generations: [u16; MAX_WINDOWS],
    /// Slots from the back of the stack to the front.
    order: [usize; MAX_WINDOWS],
    count: usize,
    focus: Option<usize>,
    pointer_x: i32,
    pointer_y: i32,
    buttons: u8,
    drag: Option<Drag>,
    /// The highlighted entry while the launcher menu is open.
    menu: Option<usize>,
    /// An application the user asked for; the display server starts it.
    launch: Option<Launch>,
}

impl Manager {
    /// `None` for a screen too small to be usable.
    pub fn new(width: i32, height: i32) -> Option<Self> {
        if !(320..=16384).contains(&width) || !(240..=16384).contains(&height) {
            return None;
        }
        Some(Self {
            screen: Rect::new(0, 0, width, height),
            windows: [None; MAX_WINDOWS],
            generations: [0; MAX_WINDOWS],
            order: [0; MAX_WINDOWS],
            count: 0,
            focus: None,
            pointer_x: width / 2,
            pointer_y: height / 2,
            buttons: 0,
            drag: None,
            menu: None,
            launch: None,
        })
    }

    // ---- Geometry ----------------------------------------------------------
    pub fn screen(&self) -> Rect {
        self.screen
    }
    pub fn work_area(&self) -> Rect {
        Rect::new(0, 0, self.screen.w, self.screen.h - TASKBAR_HEIGHT)
    }
    pub fn taskbar(&self) -> Rect {
        Rect::new(
            0,
            self.screen.h - TASKBAR_HEIGHT,
            self.screen.w,
            TASKBAR_HEIGHT,
        )
    }
    pub fn launcher_rect(&self) -> Rect {
        let bar = self.taskbar();
        Rect::new(4, bar.y + 3, 48, TASKBAR_HEIGHT - 6)
    }
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }
    pub fn menu_selected(&self) -> Option<usize> {
        self.menu
    }
    /// The popup above the launcher button (meaningful while the menu is open).
    pub fn menu_rect(&self) -> Rect {
        let height = MENU.len() as i32 * MENU_ITEM_HEIGHT + 8;
        Rect::new(4, self.taskbar().y - height - 2, MENU_WIDTH, height)
    }
    pub fn menu_item_rect(&self, item: usize) -> Rect {
        let menu = self.menu_rect();
        Rect::new(
            menu.x + 4,
            menu.y + 4 + item as i32 * MENU_ITEM_HEIGHT,
            menu.w - 8,
            MENU_ITEM_HEIGHT,
        )
    }
    /// The application the user asked to start since the last call, if any.
    pub fn take_launch(&mut self) -> Option<Launch> {
        self.launch.take()
    }
    fn toggle_menu(&mut self) -> Effects {
        let mut fx = Effects::default();
        fx.add(self.menu_rect());
        self.menu = match self.menu {
            Some(_) => None,
            None => Some(0),
        };
        fx
    }
    fn close_menu(&mut self) -> Effects {
        let mut fx = Effects::default();
        if self.menu.take().is_some() {
            fx.add(self.menu_rect());
        }
        fx
    }
    fn menu_activate(&mut self, item: usize) -> Effects {
        let mut fx = self.close_menu();
        match MENU.get(item).map(|entry| entry.1) {
            Some(Launch::Help) => fx.merge(self.toggle_help()),
            Some(app @ Launch::App(_)) => self.launch = Some(app),
            None => (),
        }
        fx
    }
    fn menu_move(&mut self, step: isize) -> Effects {
        let mut fx = Effects::default();
        if let Some(selected) = self.menu {
            let next = (selected as isize + step).rem_euclid(MENU.len() as isize) as usize;
            self.menu = Some(next);
            fx.add(self.menu_rect());
        }
        fx
    }
    /// The taskbar button of each window, in slot order, as (slot, rectangle). Buttons
    /// that would not fit on screen are left out.
    pub fn task_buttons(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        let bar = self.taskbar();
        self.windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.is_some())
            .enumerate()
            .map(move |(n, (slot, _))| {
                let x = TASKS_LEFT + n as i32 * (TASK_WIDTH + TASK_GAP);
                (
                    slot,
                    Rect::new(x, bar.y + 3, TASK_WIDTH, TASKBAR_HEIGHT - 6),
                )
            })
            .filter(move |(_, r)| r.right() <= bar.right())
    }
    pub fn pointer(&self) -> (i32, i32) {
        (self.pointer_x, self.pointer_y)
    }
    pub fn window(&self, slot: usize) -> Option<&Window> {
        self.windows.get(slot).and_then(|w| w.as_ref())
    }
    pub fn focused(&self) -> Option<usize> {
        self.focus
    }
    pub fn window_count(&self) -> usize {
        self.count
    }
    /// Windows from the back of the stack to the front.
    pub fn stack(&self) -> impl Iterator<Item = (usize, &Window)> + '_ {
        self.order[..self.count]
            .iter()
            .filter_map(|&slot| self.windows[slot].as_ref().map(|w| (slot, w)))
    }
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }
    pub fn handle(&self, slot: usize) -> Option<Handle> {
        self.window(slot)?;
        Some(Handle((self.generations[slot] as u32) << 8 | slot as u32))
    }
    pub fn resolve(&self, handle: Handle) -> Option<usize> {
        let slot = (handle.0 & 255) as usize;
        (slot < MAX_WINDOWS
            && self.windows[slot].is_some()
            && self.generations[slot] as u32 == handle.0 >> 8)
            .then_some(slot)
    }
    /// The outer rectangle a window currently occupies on screen (empty if minimized).
    pub fn visible_rect(&self, slot: usize) -> Rect {
        match self.window(slot) {
            Some(w) => match w.state {
                State::Normal => w.rect,
                State::Maximized => self.work_area(),
                State::Minimized => Rect::EMPTY,
            },
            None => Rect::EMPTY,
        }
    }
    pub fn title_rect(&self, slot: usize) -> Rect {
        let r = self.visible_rect(slot);
        Rect::new(r.x + BORDER, r.y + BORDER, r.w - 2 * BORDER, TITLE_HEIGHT)
    }
    pub fn content_rect(&self, slot: usize) -> Rect {
        let r = self.visible_rect(slot);
        Rect::new(
            r.x + BORDER,
            r.y + BORDER + TITLE_HEIGHT,
            r.w - 2 * BORDER,
            r.h - 2 * BORDER - TITLE_HEIGHT,
        )
    }
    /// (close, maximize, minimize) button rectangles, right-aligned in the title bar.
    pub fn buttons(&self, slot: usize) -> (Rect, Rect, Rect) {
        let t = self.title_rect(slot);
        let y = t.y + (TITLE_HEIGHT - BUTTON) / 2;
        let close = Rect::new(t.right() - BUTTON - 2, y, BUTTON, BUTTON);
        let maximize = Rect::new(close.x - BUTTON - 2, y, BUTTON, BUTTON);
        let minimize = Rect::new(maximize.x - BUTTON - 2, y, BUTTON, BUTTON);
        (close, maximize, minimize)
    }

    // ---- Hit testing -------------------------------------------------------
    pub fn hit(&self, x: i32, y: i32) -> Hit {
        if self.menu.is_some() && self.menu_rect().contains(x, y) {
            let item =
                ((y - self.menu_rect().y - 4) / MENU_ITEM_HEIGHT).clamp(0, MENU.len() as i32 - 1);
            return Hit::MenuItem(item as usize);
        }
        if self.taskbar().contains(x, y) {
            if self.launcher_rect().contains(x, y) {
                return Hit::Launcher;
            }
            return match self.task_buttons().find(|(_, r)| r.contains(x, y)) {
                Some((slot, _)) => Hit::Task(slot),
                None => Hit::Taskbar,
            };
        }
        for &slot in self.order[..self.count].iter().rev() {
            let outer = self.visible_rect(slot);
            if !outer.contains(x, y) {
                continue;
            }
            let part = if !outer.inset(BORDER).contains(x, y) {
                Part::Border
            } else if self.title_rect(slot).contains(x, y) {
                let (close, maximize, minimize) = self.buttons(slot);
                if close.contains(x, y) {
                    Part::Close
                } else if maximize.contains(x, y) {
                    Part::Maximize
                } else if minimize.contains(x, y) {
                    Part::Minimize
                } else {
                    Part::Title
                }
            } else {
                Part::Content
            };
            return Hit::Window(slot, part);
        }
        Hit::Desktop
    }

    // ---- Window operations -------------------------------------------------
    /// Positions a rectangle so its title bar stays reachable on the desktop.
    fn clamp_position(&self, rect: Rect) -> Rect {
        let work = self.work_area();
        let x = rect.x.clamp(REACHABLE - rect.w, work.w - REACHABLE);
        let y = rect.y.clamp(0, (work.h - TITLE_HEIGHT - BORDER).max(0));
        Rect::new(x, y, rect.w, rect.h)
    }
    pub fn create(
        &mut self,
        kind: Kind,
        title: &[u8],
        rect: Rect,
        closable: bool,
    ) -> Option<(Handle, Effects)> {
        if rect.w < 3 * BUTTON + 2 * BORDER + 16 || rect.h < TITLE_HEIGHT + 2 * BORDER + 8 {
            return None;
        }
        let slot = self.windows.iter().position(|w| w.is_none())?;
        let mut name = [0u8; TITLE_LEN];
        let length = title.len().min(TITLE_LEN);
        name[..length].copy_from_slice(&title[..length]);
        self.generations[slot] = self.generations[slot].wrapping_add(1);
        self.windows[slot] = Some(Window {
            kind,
            state: State::Normal,
            rect: self.clamp_position(rect),
            closable,
            title: name,
            title_length: length,
            previous: State::Normal,
        });
        self.order[self.count] = slot;
        self.count += 1;
        let mut fx = Effects::default();
        fx.merge(self.focus_on(slot));
        Some((self.handle(slot)?, fx))
    }
    /// Raises a window to the front and gives it the focus; damage covers it and the
    /// window that lost the focus (whose title bar changes color).
    fn focus_on(&mut self, slot: usize) -> Effects {
        let mut fx = Effects::default();
        let previous = self.focus;
        if let Some(position) = self.order[..self.count].iter().position(|&s| s == slot) {
            self.order.copy_within(position + 1..self.count, position);
            self.order[self.count - 1] = slot;
        }
        self.focus = Some(slot);
        fx.add(self.visible_rect(slot));
        if let Some(old) = previous.filter(|&p| p != slot) {
            fx.add(self.visible_rect(old));
        }
        // Task buttons show the window set, the focus and minimized windows.
        fx.add(self.taskbar());
        fx
    }
    /// Brings a window to the front, restoring it first if it is minimized.
    pub fn activate(&mut self, slot: usize) -> Effects {
        let Some(window) = self.windows.get_mut(slot).and_then(|w| w.as_mut()) else {
            return Effects::default();
        };
        if window.state == State::Minimized {
            window.state = window.previous;
        }
        self.focus_on(slot)
    }
    fn next_focus(&self, excluding: usize) -> Option<usize> {
        self.order[..self.count].iter().rev().copied().find(|&s| {
            s != excluding && self.window(s).is_some_and(|w| w.state != State::Minimized)
        })
    }
    pub fn minimize(&mut self, slot: usize) -> Effects {
        let mut fx = Effects::default();
        let was = self.visible_rect(slot);
        let Some(window) = self.windows.get_mut(slot).and_then(|w| w.as_mut()) else {
            return fx;
        };
        if window.state == State::Minimized {
            return fx;
        }
        window.previous = window.state;
        window.state = State::Minimized;
        fx.add(was);
        fx.add(self.taskbar());
        if self.focus == Some(slot) {
            self.focus = self.next_focus(slot);
            if let Some(next) = self.focus {
                fx.add(self.visible_rect(next));
            }
        }
        fx
    }
    pub fn toggle_maximize(&mut self, slot: usize) -> Effects {
        let was = self.visible_rect(slot);
        let Some(window) = self.windows.get_mut(slot).and_then(|w| w.as_mut()) else {
            return Effects::default();
        };
        window.state = match window.state {
            State::Maximized => State::Normal,
            _ => State::Maximized,
        };
        let mut fx = self.focus_on(slot);
        fx.add(was);
        fx
    }
    /// `false` (and no change) for a window that may not be closed.
    pub fn close(&mut self, slot: usize) -> Effects {
        let mut fx = Effects::default();
        if !self.window(slot).is_some_and(|w| w.closable) {
            return fx;
        }
        fx.add(self.visible_rect(slot));
        fx.add(self.taskbar());
        if let Some(position) = self.order[..self.count].iter().position(|&s| s == slot) {
            self.order.copy_within(position + 1..self.count, position);
            self.count -= 1;
        }
        self.windows[slot] = None;
        if self.drag.is_some_and(|d| d.slot == slot) {
            self.drag = None;
            fx.outline = None;
        }
        if self.focus == Some(slot) {
            self.focus = self.next_focus(slot);
            if let Some(next) = self.focus {
                fx.add(self.visible_rect(next));
            }
        }
        fx
    }
    pub fn move_by(&mut self, slot: usize, dx: i32, dy: i32) -> Effects {
        let mut fx = Effects::default();
        let Some(window) = self.window(slot).copied() else {
            return fx;
        };
        if window.state != State::Normal {
            return fx;
        }
        let moved = self.clamp_position(window.rect.translate(dx, dy));
        if moved != window.rect {
            fx.add(window.rect);
            fx.add(moved);
            self.windows[slot].as_mut().unwrap().rect = moved;
        }
        fx
    }
    /// Alt+Tab: forward activates the least recently used window; backward sends the
    /// front window to the back and activates the new front.
    pub fn cycle(&mut self, forward: bool) -> Effects {
        if self.count < 2 {
            return Effects::default();
        }
        if forward {
            self.activate(self.order[0])
        } else {
            let front = self.order[self.count - 1];
            self.order.copy_within(0..self.count - 1, 1);
            self.order[0] = front;
            let target = self.next_focus(front).unwrap_or(front);
            let mut fx = self.activate(target);
            fx.add(self.visible_rect(front));
            fx
        }
    }
    pub fn help_slot(&self) -> Option<usize> {
        self.windows
            .iter()
            .position(|w| w.is_some_and(|w| w.kind == Kind::Help))
    }
    /// Opens the help window, or closes it if it is already open.
    pub fn toggle_help(&mut self) -> Effects {
        if let Some(slot) = self.help_slot() {
            return self.close(slot);
        }
        let width = 440.min(self.screen.w - 40);
        let rect = Rect::new(
            (self.screen.w - width) / 2,
            60,
            width,
            300.min(self.work_area().h - 80),
        );
        self.create(Kind::Help, b"ZOS Help", rect, true)
            .map(|(_, fx)| fx)
            .unwrap_or_default()
    }

    // ---- Input -------------------------------------------------------------
    /// Applies an Alt+key shortcut from the keyboard.
    pub fn alt(&mut self, key: u8) -> Effects {
        let focused = self.focus;
        if self.menu.is_some() {
            // With the launcher open its keys come first.
            match key {
                ALT_UP => return self.menu_move(-1),
                ALT_DOWN => return self.menu_move(1),
                b'l' => return self.menu_activate(self.menu.unwrap_or(0)),
                b'w' => return self.close_menu(),
                _ => (),
            }
        }
        match key {
            b'l' => self.toggle_menu(),
            // Direct shortcuts for the applications.
            b'b' => {
                self.launch = Some(Launch::App(0));
                Effects::default()
            }
            b'c' => {
                self.launch = Some(Launch::App(1));
                Effects::default()
            }
            b'\t' => self.cycle(true),
            b'`' => self.cycle(false),
            b'h' => self.toggle_help(),
            b'm' => focused.map(|s| self.minimize(s)).unwrap_or_default(),
            b'x' => focused.map(|s| self.toggle_maximize(s)).unwrap_or_default(),
            b'w' => focused.map(|s| self.close(s)).unwrap_or_default(),
            ALT_LEFT => focused
                .map(|s| self.move_by(s, -MOVE_STEP, 0))
                .unwrap_or_default(),
            ALT_RIGHT => focused
                .map(|s| self.move_by(s, MOVE_STEP, 0))
                .unwrap_or_default(),
            ALT_UP => focused
                .map(|s| self.move_by(s, 0, -MOVE_STEP))
                .unwrap_or_default(),
            ALT_DOWN => focused
                .map(|s| self.move_by(s, 0, MOVE_STEP))
                .unwrap_or_default(),
            _ => Effects::default(),
        }
    }

    /// Applies a pointer report: relative motion (positive `dy` is up, as the input
    /// drivers send it) and the button mask (bit 0 is the left button).
    pub fn pointer_event(&mut self, dx: i32, dy: i32, buttons: u8) -> Effects {
        self.pointer_x = self
            .pointer_x
            .saturating_add(dx)
            .clamp(0, self.screen.w - 1);
        self.pointer_y = self
            .pointer_y
            .saturating_sub(dy)
            .clamp(0, self.screen.h - 1);
        let was_down = self.buttons & 1 != 0;
        let down = buttons & 1 != 0;
        self.buttons = buttons;
        let mut fx = Effects::default();
        if let Some(mut drag) = self.drag {
            if down {
                let target = self.clamp_position(Rect::new(
                    self.pointer_x - drag.grab_x,
                    self.pointer_y - drag.grab_y,
                    drag.outline.w,
                    drag.outline.h,
                ));
                if target != drag.outline {
                    fx.outline = Some((drag.outline, target));
                    drag.outline = target;
                    self.drag = Some(drag);
                }
            } else {
                // Released: the window moves where the outline is.
                self.drag = None;
                if let Some(window) = self.windows[drag.slot].as_mut() {
                    fx.add(window.rect);
                    window.rect = drag.outline;
                    fx.add(drag.outline);
                }
                fx.outline = Some((drag.outline, Rect::EMPTY));
            }
            return fx;
        }
        if down && !was_down {
            fx = self.click(self.pointer_x, self.pointer_y);
        }
        fx
    }

    fn click(&mut self, x: i32, y: i32) -> Effects {
        let hit = self.hit(x, y);
        let mut closed = Effects::default();
        if self.menu.is_some() && !matches!(hit, Hit::MenuItem(_) | Hit::Launcher) {
            // A click anywhere else dismisses the menu and then does its normal job.
            closed = self.close_menu();
        }
        let mut fx = self.click_on(hit, x, y);
        fx.merge(closed);
        fx
    }

    fn click_on(&mut self, hit: Hit, x: i32, y: i32) -> Effects {
        match hit {
            Hit::MenuItem(item) => self.menu_activate(item),
            Hit::Launcher => self.toggle_menu(),
            Hit::Task(slot) => {
                if self.focus == Some(slot)
                    && self
                        .window(slot)
                        .is_some_and(|w| w.state != State::Minimized)
                {
                    self.minimize(slot)
                } else {
                    self.activate(slot)
                }
            }
            Hit::Window(slot, part) => {
                let mut fx = self.activate(slot);
                match part {
                    Part::Close => fx.merge(self.close(slot)),
                    Part::Minimize => fx.merge(self.minimize(slot)),
                    Part::Maximize => fx.merge(self.toggle_maximize(slot)),
                    Part::Title => {
                        let window = self.windows[slot].unwrap();
                        if window.state == State::Normal {
                            self.drag = Some(Drag {
                                slot,
                                grab_x: x - window.rect.x,
                                grab_y: y - window.rect.y,
                                outline: window.rect,
                            });
                        }
                    }
                    Part::Content | Part::Border => (),
                }
                fx
            }
            Hit::Desktop | Hit::Taskbar => Effects::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: i32 = 1024;
    const H: i32 = 768;

    fn manager() -> Manager {
        Manager::new(W, H).unwrap()
    }
    fn open(m: &mut Manager, kind: Kind, name: &str, x: i32, y: i32, w: i32, h: i32) -> usize {
        let (handle, _) = m
            .create(kind, name.as_bytes(), Rect::new(x, y, w, h), true)
            .unwrap();
        m.resolve(handle).unwrap()
    }
    fn order(m: &Manager) -> std::vec::Vec<usize> {
        m.stack().map(|(s, _)| s).collect()
    }
    fn click(m: &mut Manager, x: i32, y: i32) -> Effects {
        // Place the pointer, then press and release.
        let (px, py) = m.pointer();
        m.pointer_event(x - px, py - y, 0);
        let fx = m.pointer_event(0, 0, 1);
        let mut all = fx;
        all.merge(m.pointer_event(0, 0, 0));
        all
    }
    fn covered(fx: &Effects, rect: Rect) -> bool {
        fx.damage().iter().any(|d| d.intersect(rect) == rect)
    }

    #[test]
    fn tiny_screens_are_refused_and_geometry_is_consistent() {
        assert!(Manager::new(319, 480).is_none());
        assert!(Manager::new(640, 239).is_none());
        assert!(Manager::new(20000, 480).is_none());
        let mut m = manager();
        assert_eq!(m.work_area(), Rect::new(0, 0, W, H - TASKBAR_HEIGHT));
        assert_eq!(
            m.taskbar(),
            Rect::new(0, H - TASKBAR_HEIGHT, W, TASKBAR_HEIGHT)
        );
        let a = open(&mut m, Kind::Terminal, "t", 50, 50, 400, 300);
        let outer = m.visible_rect(a);
        let (close, maximize, minimize) = m.buttons(a);
        for b in [close, maximize, minimize] {
            assert_eq!(
                b.intersect(m.title_rect(a)),
                b,
                "buttons sit inside the title bar"
            );
        }
        assert!(close.x > maximize.x && maximize.x > minimize.x);
        assert!(close.intersect(maximize).is_empty() && maximize.intersect(minimize).is_empty());
        assert_eq!(m.content_rect(a).y, outer.y + BORDER + TITLE_HEIGHT);
        assert_eq!(m.content_rect(a).intersect(outer), m.content_rect(a));
    }

    #[test]
    fn hit_testing_finds_the_topmost_window_and_the_right_part() {
        let mut m = manager();
        let back = open(&mut m, Kind::Terminal, "back", 100, 100, 400, 300);
        let front = open(&mut m, Kind::Help, "front", 300, 200, 400, 300);
        // A point inside both windows belongs to the front one.
        let overlap = m.content_rect(front);
        assert!(m
            .visible_rect(back)
            .contains(overlap.x + 10, overlap.y + 10));
        assert_eq!(
            m.hit(overlap.x + 10, overlap.y + 10),
            Hit::Window(front, Part::Content)
        );
        // Inside only the back window.
        let only_back = m.content_rect(back);
        assert!(!m
            .visible_rect(front)
            .contains(only_back.x + 5, only_back.y + 5));
        assert_eq!(
            m.hit(only_back.x + 5, only_back.y + 5),
            Hit::Window(back, Part::Content)
        );
        assert_eq!(m.hit(5, 5), Hit::Desktop);
        assert_eq!(m.hit(W - 1, H - TASKBAR_HEIGHT - 1), Hit::Desktop);
        // Border, title and buttons of the front window.
        assert_eq!(m.hit(300, 300), Hit::Window(front, Part::Border));
        let title = m.title_rect(front);
        assert_eq!(
            m.hit(title.x + 4, title.y + 4),
            Hit::Window(front, Part::Title)
        );
        let (close, maximize, minimize) = m.buttons(front);
        assert_eq!(
            m.hit(close.x + 2, close.y + 2),
            Hit::Window(front, Part::Close)
        );
        assert_eq!(
            m.hit(maximize.x + 2, maximize.y + 2),
            Hit::Window(front, Part::Maximize)
        );
        assert_eq!(
            m.hit(minimize.x + 2, minimize.y + 2),
            Hit::Window(front, Part::Minimize)
        );
        // The right edge of a rectangle is outside it: x = 300 + 400 at y = 250 is
        // beyond the front window and beyond the back window (which ends at x = 500).
        assert_eq!(m.hit(300 + 400, 250), Hit::Desktop);
        assert_eq!(m.hit(300 + 399, 250), Hit::Window(front, Part::Border));
        // The taskbar and its launcher and task buttons.
        assert_eq!(m.hit(10, H - 10), Hit::Launcher);
        assert_eq!(m.hit(W - 5, H - 10), Hit::Taskbar);
        let (slot, rect) = m.task_buttons().next().unwrap();
        assert_eq!(m.hit(rect.x + 3, rect.y + 3), Hit::Task(slot));
    }
    #[test]
    fn clicking_raises_focuses_and_repaints_both_windows() {
        let mut m = manager();
        let a = open(&mut m, Kind::Terminal, "a", 100, 100, 300, 200);
        let b = open(&mut m, Kind::Help, "b", 500, 100, 300, 200);
        let c = open(&mut m, Kind::Help, "c", 300, 300, 300, 200);
        assert_eq!(order(&m), [a, b, c]);
        assert_eq!(m.focused(), Some(c));
        let content = m.content_rect(a);
        let fx = click(&mut m, content.x + 10, content.y + 10);
        assert_eq!(order(&m), [b, c, a]);
        assert_eq!(m.focused(), Some(a));
        assert!(
            covered(&fx, m.visible_rect(a)) && covered(&fx, m.visible_rect(c)),
            "old and new focus repainted"
        );
        assert!(
            !covered(&fx, m.visible_rect(b)),
            "an unrelated window is left alone"
        );
        // Clicking the desktop changes nothing.
        let fx = click(&mut m, 5, 5);
        assert!(fx.is_empty());
        assert_eq!(m.focused(), Some(a));
    }

    #[test]
    fn the_taskbar_is_repainted_whenever_the_window_set_or_focus_changes() {
        let mut m = manager();
        let bar = m.taskbar();
        let (handle, fx) = m
            .create(Kind::Terminal, b"t", Rect::new(50, 50, 300, 200), false)
            .unwrap();
        assert!(
            covered(&fx, bar),
            "a new window needs its task button drawn"
        );
        let (second, fx) = m
            .create(Kind::Help, b"h", Rect::new(80, 80, 300, 200), true)
            .unwrap();
        assert!(covered(&fx, bar));
        let (first, second) = (m.resolve(handle).unwrap(), m.resolve(second).unwrap());
        assert!(
            covered(&m.activate(first), bar),
            "focus changes the pressed button"
        );
        assert!(
            covered(&m.minimize(first), bar),
            "minimizing greys the button"
        );
        assert!(covered(&m.activate(first), bar));
        assert!(covered(&m.toggle_maximize(first), bar));
        assert!(
            covered(&m.close(second), bar),
            "a closed window loses its button"
        );
        // Operations that change nothing visible leave the taskbar alone.
        assert!(m.move_by(first, 0, 0).is_empty());
        assert!(m.close(first).is_empty(), "the terminal cannot be closed");
    }

    #[test]
    fn handles_go_stale_when_a_slot_is_reused() {
        let mut m = manager();
        let (first, _) = m
            .create(Kind::Help, b"one", Rect::new(50, 50, 300, 200), true)
            .unwrap();
        let slot = m.resolve(first).unwrap();
        m.close(slot);
        assert_eq!(m.resolve(first), None);
        let (second, _) = m
            .create(Kind::Help, b"two", Rect::new(50, 50, 300, 200), true)
            .unwrap();
        assert_eq!(m.resolve(second), Some(slot), "the slot is reused");
        assert_eq!(m.resolve(first), None, "but the old handle stays invalid");
        assert_eq!(m.resolve(Handle(0)), None);
        assert_eq!(m.resolve(Handle(u32::MAX)), None);
        assert_eq!(m.resolve(Handle(200)), None, "slot out of range");
        assert_eq!(m.handle(7), None, "empty slot");
    }

    #[test]
    fn the_window_table_is_bounded_and_rejects_unusable_sizes() {
        let mut m = manager();
        for i in 0..MAX_WINDOWS {
            assert!(m
                .create(
                    Kind::Help,
                    b"w",
                    Rect::new(10 + i as i32, 10, 300, 200),
                    true
                )
                .is_some());
        }
        assert!(m
            .create(
                Kind::Help,
                b"one too many",
                Rect::new(10, 10, 300, 200),
                true
            )
            .is_none());
        assert_eq!(m.window_count(), MAX_WINDOWS);
        let mut n = manager();
        assert!(n
            .create(Kind::Help, b"x", Rect::new(0, 0, 10, 10), true)
            .is_none());
        assert!(n
            .create(Kind::Help, b"x", Rect::new(0, 0, 300, 10), true)
            .is_none());
        assert!(n
            .create(Kind::Help, b"x", Rect::new(0, 0, -300, 200), true)
            .is_none());
        // Titles are truncated, never overflowing the fixed buffer.
        let long = [b'a'; 100];
        let (h, _) = n
            .create(Kind::Help, &long, Rect::new(10, 10, 300, 200), true)
            .unwrap();
        assert_eq!(
            n.window(n.resolve(h).unwrap()).unwrap().title().len(),
            TITLE_LEN
        );
    }

    #[test]
    fn new_windows_are_pulled_onto_the_desktop() {
        let mut m = manager();
        let a = open(&mut m, Kind::Help, "far", 5000, 5000, 300, 200);
        let r = m.visible_rect(a);
        assert!(r.x < W && r.y < H - TASKBAR_HEIGHT, "{r:?}");
        let b = open(&mut m, Kind::Help, "left", -5000, -5000, 300, 200);
        let r = m.visible_rect(b);
        assert!(r.right() >= REACHABLE && r.y >= 0, "{r:?}");
    }

    #[test]
    fn minimize_restore_and_maximize_manage_focus_and_damage() {
        let mut m = manager();
        let a = open(&mut m, Kind::Terminal, "a", 100, 100, 300, 200);
        let b = open(&mut m, Kind::Help, "b", 200, 150, 300, 200);
        let b_rect = m.visible_rect(b);
        let fx = m.minimize(b);
        assert!(covered(&fx, b_rect), "the vacated area is repainted");
        assert_eq!(m.visible_rect(b), Rect::EMPTY);
        assert_eq!(
            m.focused(),
            Some(a),
            "focus passes to the next visible window"
        );
        // A minimized window cannot be hit, but its task button can.
        assert!(
            !matches!(m.hit(250, 200), Hit::Window(s, _) if s == b),
            "a minimized window cannot be hit"
        );
        let (_, button) = m.task_buttons().find(|(s, _)| *s == b).unwrap();
        click(&mut m, button.x + 5, button.y + 5);
        assert_eq!(m.window(b).unwrap().state, State::Normal);
        assert_eq!(m.focused(), Some(b));
        assert_eq!(order(&m).last(), Some(&b));
        // Clicking the focused window's task button minimizes it.
        click(&mut m, button.x + 5, button.y + 5);
        assert_eq!(m.window(b).unwrap().state, State::Minimized);
        // Maximize fills the work area and restores to the old rectangle.
        let before = m.visible_rect(a);
        let fx = m.toggle_maximize(a);
        assert_eq!(m.visible_rect(a), m.work_area());
        assert!(covered(&fx, before) && covered(&fx, m.work_area()));
        m.toggle_maximize(a);
        assert_eq!(m.visible_rect(a), before);
        // A window minimized while maximized comes back maximized.
        m.toggle_maximize(a);
        m.minimize(a);
        m.activate(a);
        assert_eq!(m.window(a).unwrap().state, State::Maximized);
        // Minimizing everything leaves nothing focused.
        m.minimize(a);
        assert_eq!(m.focused(), None);
    }

    #[test]
    fn alt_tab_cycles_through_every_window_and_back() {
        let mut m = manager();
        let a = open(&mut m, Kind::Terminal, "a", 100, 100, 300, 200);
        let b = open(&mut m, Kind::Help, "b", 150, 120, 300, 200);
        let c = open(&mut m, Kind::Help, "c", 200, 140, 300, 200);
        assert_eq!(m.focused(), Some(c));
        m.alt(b'\t');
        assert_eq!(
            m.focused(),
            Some(a),
            "the least recently used comes forward"
        );
        m.alt(b'\t');
        assert_eq!(m.focused(), Some(b));
        m.alt(b'\t');
        assert_eq!(m.focused(), Some(c));
        m.alt(b'`');
        assert_eq!(
            order(&m)[0],
            c,
            "backward sends the front window to the back"
        );
        assert_eq!(m.focused(), Some(b));
        // Alt+Tab restores a minimized window it lands on.
        let mut other = manager();
        let x = open(&mut other, Kind::Terminal, "x", 100, 100, 300, 200);
        let _y = open(&mut other, Kind::Help, "y", 150, 120, 300, 200);
        let z = open(&mut other, Kind::Help, "z", 200, 140, 300, 200);
        other.minimize(x);
        assert_eq!(other.window(x).unwrap().state, State::Minimized);
        assert_eq!(other.focused(), Some(z));
        other.alt(b'\t');
        assert_eq!(
            other.window(x).unwrap().state,
            State::Normal,
            "the back-most window was minimized"
        );
        assert_eq!(other.focused(), Some(x));
        // One window (or none): nothing to cycle.
        let mut single = manager();
        assert!(single.alt(b'\t').is_empty());
        let only = open(&mut single, Kind::Terminal, "only", 10, 10, 300, 200);
        assert!(single.alt(b'\t').is_empty());
        assert_eq!(single.focused(), Some(only));
    }

    #[test]
    fn dragging_moves_an_outline_and_commits_on_release() {
        let mut m = manager();
        let a = open(&mut m, Kind::Terminal, "a", 100, 100, 300, 200);
        let title = m.title_rect(a);
        // Press on the title bar.
        let (px, py) = m.pointer();
        m.pointer_event(title.x + 20 - px, py - (title.y + 5), 0);
        m.pointer_event(0, 0, 1);
        assert!(m.dragging());
        // Move right 50 and down 30 (screen y grows downward, drivers send dy up).
        let fx = m.pointer_event(50, -30, 1);
        let (old, new) = fx.outline.unwrap();
        assert_eq!(old, Rect::new(100, 100, 300, 200));
        assert_eq!(new, Rect::new(150, 130, 300, 200));
        assert_eq!(
            m.visible_rect(a),
            Rect::new(100, 100, 300, 200),
            "the window itself has not moved yet"
        );
        assert!(
            m.pointer_event(0, 0, 1).outline.is_none(),
            "no motion, no outline change"
        );
        let fx = m.pointer_event(10, 0, 0);
        assert!(!m.dragging());
        assert_eq!(
            m.visible_rect(a),
            Rect::new(150, 130, 300, 200),
            "the window lands where the outline was"
        );
        assert_eq!(
            fx.outline,
            Some((Rect::new(150, 130, 300, 200), Rect::EMPTY))
        );
        assert!(
            covered(&fx, Rect::new(100, 100, 300, 200))
                && covered(&fx, Rect::new(150, 130, 300, 200))
        );
        // Dragging far off screen keeps the title bar reachable.
        let title = m.title_rect(a);
        let (px, py) = m.pointer();
        m.pointer_event(title.x + 5 - px, py - (title.y + 5), 0);
        m.pointer_event(0, 0, 1);
        m.pointer_event(-5000, 5000, 1);
        m.pointer_event(0, 0, 0);
        let r = m.visible_rect(a);
        assert!(r.right() >= REACHABLE && r.y >= 0 && r.y <= H, "{r:?}");
        // Maximized windows cannot be dragged.
        m.toggle_maximize(a);
        let title = m.title_rect(a);
        click(&mut m, title.x + 100, title.y + 5);
        assert!(!m.dragging());
    }

    #[test]
    fn closing_during_a_drag_and_unclosable_windows() {
        let mut m = manager();
        let (term, _) = m
            .create(Kind::Terminal, b"term", Rect::new(50, 50, 400, 300), false)
            .unwrap();
        let term = m.resolve(term).unwrap();
        assert!(
            m.close(term).is_empty(),
            "the shell terminal cannot be closed"
        );
        assert!(m.window(term).is_some());
        let (close, _, _) = m.buttons(term);
        click(&mut m, close.x + 3, close.y + 3);
        assert!(m.window(term).is_some());
        m.alt(b'w');
        assert!(m.window(term).is_some());
        // A closable window disappears, taking an in-progress drag with it.
        let h = open(&mut m, Kind::Help, "help", 300, 200, 300, 200);
        let title = m.title_rect(h);
        let (px, py) = m.pointer();
        m.pointer_event(title.x + 20 - px, py - (title.y + 5), 0);
        m.pointer_event(0, 0, 1);
        assert!(m.dragging());
        m.close(h);
        assert!(!m.dragging());
        assert_eq!(m.focused(), Some(term));
        let fx = m.pointer_event(10, 10, 0);
        assert!(
            fx.outline.is_none(),
            "a stale drag must not resurrect the closed window"
        );
    }

    #[test]
    fn title_buttons_work_by_mouse() {
        let mut m = manager();
        let h = open(&mut m, Kind::Help, "h", 200, 100, 300, 200);
        let (_, _, minimize) = m.buttons(h);
        click(&mut m, minimize.x + 3, minimize.y + 3);
        assert_eq!(m.window(h).unwrap().state, State::Minimized);
        m.activate(h);
        let (_, maximize, _) = m.buttons(h);
        click(&mut m, maximize.x + 3, maximize.y + 3);
        assert_eq!(m.window(h).unwrap().state, State::Maximized);
        let (close, _, _) = m.buttons(h);
        click(&mut m, close.x + 3, close.y + 3);
        assert!(m.window(h).is_none());
    }

    #[test]
    fn keyboard_shortcuts_move_resize_and_open_help() {
        let mut m = manager();
        let t = open(&mut m, Kind::Terminal, "t", 100, 100, 400, 300);
        m.alt(ALT_RIGHT);
        m.alt(ALT_DOWN);
        assert_eq!(
            m.visible_rect(t),
            Rect::new(100 + MOVE_STEP, 100 + MOVE_STEP, 400, 300)
        );
        m.alt(ALT_LEFT);
        m.alt(ALT_UP);
        assert_eq!(m.visible_rect(t), Rect::new(100, 100, 400, 300));
        for _ in 0..200 {
            m.alt(ALT_LEFT);
            m.alt(ALT_UP);
        }
        let r = m.visible_rect(t);
        assert!(r.right() >= REACHABLE && r.y == 0, "{r:?}");
        m.alt(b'x');
        assert_eq!(m.window(t).unwrap().state, State::Maximized);
        assert!(m.alt(ALT_RIGHT).is_empty(), "maximized windows do not move");
        m.alt(b'x');
        m.alt(b'm');
        assert_eq!(m.window(t).unwrap().state, State::Minimized);
        assert_eq!(m.focused(), None, "the only window is minimized");
        assert!(
            m.alt(b'm').is_empty() && m.alt(b'x').is_empty(),
            "nothing focused: nothing to act on"
        );
        // Help opens, and closes again (also from the launcher menu).
        m.alt(b'h');
        let help = m.help_slot().unwrap();
        assert_eq!(m.focused(), Some(help));
        m.alt(b'h');
        assert!(m.help_slot().is_none());
        let launcher = m.launcher_rect();
        click(&mut m, launcher.x + 5, launcher.y + 5);
        assert!(m.menu_open() && m.help_slot().is_none());
        let item = m.menu_item_rect(0);
        click(&mut m, item.x + 5, item.y + 5);
        assert!(m.help_slot().is_some() && !m.menu_open());
        click(&mut m, launcher.x + 5, launcher.y + 5);
        let item = m.menu_item_rect(0);
        click(&mut m, item.x + 5, item.y + 5);
        assert!(m.help_slot().is_none());
        // Unknown keys do nothing.
        assert!(m.alt(b'q').is_empty() && m.alt(0).is_empty() && m.alt(255).is_empty());
    }

    #[test]
    fn the_launcher_menu_starts_applications_from_the_mouse_and_the_keyboard() {
        let mut m = manager();
        let t = open(&mut m, Kind::Terminal, "t", 100, 100, 400, 300);
        assert_eq!(m.take_launch(), None);
        // Mouse: open, choose "Browser" (entry 1), and the display server is told.
        let launcher = m.launcher_rect();
        let fx = click(&mut m, launcher.x + 5, launcher.y + 5);
        assert!(m.menu_open() && covered(&fx, m.menu_rect()));
        assert!(
            m.menu_rect().bottom() <= m.taskbar().y,
            "the popup sits above the taskbar"
        );
        let browser = m.menu_item_rect(1);
        assert_eq!(m.hit(browser.x + 3, browser.y + 3), Hit::MenuItem(1));
        let fx = click(&mut m, browser.x + 3, browser.y + 3);
        assert!(!m.menu_open() && covered(&fx, m.menu_rect()));
        assert_eq!(m.take_launch(), Some(Launch::App(0)));
        assert_eq!(m.take_launch(), None, "a request is delivered once");
        assert_eq!(
            m.window_count(),
            1,
            "the manager does not create the application window"
        );
        // Keyboard: Alt+L opens, Alt+Down moves (wrapping), Alt+L starts the highlighted.
        m.alt(b'l');
        assert_eq!(m.menu_selected(), Some(0));
        m.alt(ALT_DOWN);
        m.alt(ALT_DOWN);
        assert_eq!(m.menu_selected(), Some(2));
        m.alt(ALT_DOWN);
        assert_eq!(m.menu_selected(), Some(0), "the selection wraps");
        m.alt(ALT_UP);
        assert_eq!(m.menu_selected(), Some(2));
        m.alt(b'l');
        assert_eq!(m.take_launch(), Some(Launch::App(1)));
        assert!(!m.menu_open());
        // While the menu is open Alt+Up/Down choose an entry instead of moving the
        // window, and Alt+W cancels the menu instead of closing the window.
        let before = m.window(t).unwrap().rect;
        m.alt(b'l');
        m.alt(ALT_DOWN);
        m.alt(ALT_UP);
        assert_eq!(m.window(t).unwrap().rect, before);
        m.alt(b'w');
        assert!(!m.menu_open() && m.window(t).is_some() && m.take_launch().is_none());
        // Direct shortcuts.
        m.alt(b'b');
        assert_eq!(m.take_launch(), Some(Launch::App(0)));
        m.alt(b'c');
        assert_eq!(m.take_launch(), Some(Launch::App(1)));
        // A click elsewhere dismisses the menu and still does its own job.
        m.alt(b'h');
        let help = m.help_slot().unwrap();
        m.activate(t);
        m.alt(b'l');
        let c = m.visible_rect(help);
        let fx = click(&mut m, c.x + c.w / 2, c.y + c.h / 2);
        assert!(!m.menu_open() && covered(&fx, m.menu_rect()));
        assert_eq!(
            m.focused(),
            Some(help),
            "the click that closed the menu also focused the window"
        );
        assert_eq!(m.take_launch(), None);
    }

    #[test]
    fn application_windows_are_ordinary_windows_with_their_own_kind() {
        let mut m = manager();
        let a = open(&mut m, Kind::App(2), "app", 80, 80, 300, 200);
        assert_eq!(m.window(a).unwrap().kind, Kind::App(2));
        assert_eq!(m.focused(), Some(a));
        let (close, _, _) = m.buttons(a);
        click(&mut m, close.x + 2, close.y + 2);
        assert!(m.window(a).is_none(), "an application window is closable");
    }

    #[test]
    fn the_pointer_is_clamped_to_the_screen_even_for_extreme_motion() {
        let mut m = manager();
        m.pointer_event(i32::MAX, i32::MIN, 0);
        assert_eq!(m.pointer(), (W - 1, H - 1));
        m.pointer_event(i32::MIN, i32::MAX, 0);
        assert_eq!(m.pointer(), (0, 0));
        m.pointer_event(10, 0, 0);
        m.pointer_event(0, -7, 0);
        assert_eq!(m.pointer(), (10, 7), "a negative dy moves the pointer down");
        // Several buttons at once only count the left one for clicks.
        let mut n = manager();
        let t = open(&mut n, Kind::Terminal, "t", 100, 100, 300, 200);
        let c = n.content_rect(t);
        let (px, py) = n.pointer();
        n.pointer_event(c.x + 5 - px, py - (c.y + 5), 2);
        n.pointer_event(0, 0, 4);
        assert!(!n.dragging());
    }

    #[test]
    fn damage_never_overflows_its_fixed_list() {
        let mut fx = Effects::default();
        for i in 0..20 {
            fx.add(Rect::new(i * 10, 0, 5, 5));
        }
        fx.add(Rect::EMPTY);
        assert_eq!(fx.damage().len(), 6);
        let total = fx.damage().iter().fold(Rect::EMPTY, |a, r| a.union(*r));
        assert_eq!(
            total,
            Rect::new(0, 0, 195, 5),
            "overflow is merged, not dropped"
        );
    }

    #[test]
    fn task_buttons_that_do_not_fit_are_left_out() {
        let mut m = Manager::new(320, 240).unwrap();
        for _ in 0..4 {
            m.create(Kind::Help, b"w", Rect::new(10, 10, 200, 100), true)
                .unwrap();
        }
        let shown = m.task_buttons().count();
        assert!(shown < 4 && shown >= 1, "{shown}");
        assert!(m.task_buttons().all(|(_, r)| r.right() <= 320));
        // An unreachable (hidden) button cannot be hit.
        let hidden_x = TASKS_LEFT + (TASK_WIDTH + TASK_GAP) + 2;
        assert!(hidden_x < 320);
        assert_eq!(m.hit(hidden_x, 240 - 10), Hit::Taskbar);
    }
}
