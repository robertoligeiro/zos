//! Desktop application windows: the manager-side state behind the `gui` protocol.
//!
//! Everything here is bounded and checked: a fixed number of application windows, a
//! fixed drawing list and event queue per window, one parked `WAIT` per window, and
//! handles that carry a generation so a closed window's handle is refused forever.
//! An application only ever reaches its own window: every request is matched against
//! the sender the kernel reported, not anything the message claims.

use crate::windows::Handle;
use zos_console::gui::{self, Event, MAX_HEIGHT, MAX_TEXT, MAX_WIDTH};

pub const MAX_APPS: usize = 4;
pub const MAX_OPS: usize = 256;
pub const EVENTS: usize = 8;
pub const MIN_WIDTH: u16 = 64;
pub const MIN_HEIGHT: u16 = 48;

#[derive(Clone, Copy)]
pub struct Op {
    pub is_text: bool,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub color: u32,
    length: u8,
    bytes: [u8; MAX_TEXT],
}

impl Op {
    const EMPTY: Op = Op {
        is_text: false,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        color: 0,
        length: 0,
        bytes: [0; MAX_TEXT],
    };
    pub fn text(&self) -> &[u8] {
        &self.bytes[..self.length as usize]
    }
}

pub struct App {
    pub owner: usize,
    pub handle: Handle,
    /// The window manager's slot for this window.
    pub slot: usize,
    pub width: u16,
    pub height: u16,
    pub background: u32,
    ops: [Op; MAX_OPS],
    count: usize,
    waiter: Option<usize>,
    events: [Option<Event>; EVENTS],
    head: usize,
    length: usize,
    /// Events that did not fit the queue (pointer clicks and keys; never `Close`).
    pub dropped: u32,
}

/// What `wait` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited {
    /// An event was already queued.
    Event(Event),
    /// The caller's reply token is kept until an event arrives.
    Parked,
}

pub struct Apps {
    apps: [Option<App>; MAX_APPS],
    generations: [u8; MAX_APPS],
}

impl Default for Apps {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn ops(&self) -> &[Op] {
        &self.ops[..self.count]
    }
    pub fn clear(&mut self, color: u32) {
        self.count = 0;
        self.background = color & 0xff_ffff;
    }
    pub fn rect(&mut self, x: i16, y: i16, width: u16, height: u16, color: u32) -> u8 {
        self.push(Op {
            is_text: false,
            x,
            y,
            width,
            height,
            color: color & 0xff_ffff,
            ..Op::EMPTY
        })
    }
    pub fn text(&mut self, x: i16, y: i16, color: u32, text: &[u8]) -> u8 {
        if text.len() > MAX_TEXT {
            return gui::INVALID;
        }
        let mut op = Op {
            is_text: true,
            x,
            y,
            color: color & 0xff_ffff,
            length: text.len() as u8,
            ..Op::EMPTY
        };
        for (to, &from) in op.bytes.iter_mut().zip(text) {
            *to = if (32..=126).contains(&from) {
                from
            } else {
                b'?'
            };
        }
        self.push(op)
    }
    fn push(&mut self, op: Op) -> u8 {
        if self.count == MAX_OPS {
            return gui::FULL;
        }
        self.ops[self.count] = op;
        self.count += 1;
        gui::OK
    }
    /// Takes the next event, or parks `token` until one arrives.
    pub fn wait(&mut self, token: usize) -> Result<Waited, u8> {
        if let Some(event) = self.pop() {
            return Ok(Waited::Event(event));
        }
        if self.waiter.is_some() {
            return Err(gui::DENIED);
        }
        self.waiter = Some(token);
        Ok(Waited::Parked)
    }
    fn pop(&mut self) -> Option<Event> {
        if self.length == 0 {
            return None;
        }
        let event = self.events[self.head].take();
        self.head = (self.head + 1) % EVENTS;
        self.length -= 1;
        event
    }
    /// Queues an event. If a `WAIT` is parked it is answered at once instead: the
    /// returned token and event are for the caller to reply with.
    pub fn push_event(&mut self, event: Event) -> Option<(usize, Event)> {
        if let Some(token) = self.waiter.take() {
            return Some((token, event));
        }
        if self.length == EVENTS {
            if event == Event::Close {
                // Closing must get through: it replaces the newest queued event.
                let last = (self.head + EVENTS - 1) % EVENTS;
                self.events[last] = Some(event);
            } else {
                self.dropped = self.dropped.saturating_add(1);
            }
            return None;
        }
        self.events[(self.head + self.length) % EVENTS] = Some(event);
        self.length += 1;
        None
    }
}

impl Apps {
    pub const fn new() -> Self {
        Self {
            apps: [const { None }; MAX_APPS],
            generations: [0; MAX_APPS],
        }
    }

    pub fn app(&self, index: usize) -> Option<&App> {
        self.apps.get(index).and_then(|app| app.as_ref())
    }
    pub fn app_mut(&mut self, index: usize) -> Option<&mut App> {
        self.apps.get_mut(index).and_then(|app| app.as_mut())
    }
    pub fn count(&self) -> usize {
        self.apps.iter().flatten().count()
    }
    /// The index a new window would get, if there is room and `owner` has no window.
    pub fn reserve(&self, owner: usize) -> Result<usize, u8> {
        if self.apps.iter().flatten().any(|app| app.owner == owner) {
            return Err(gui::DENIED);
        }
        self.apps
            .iter()
            .position(Option::is_none)
            .ok_or(gui::NO_WINDOW)
    }
    /// Checks a requested content size.
    pub fn size_ok(width: u16, height: u16) -> bool {
        (MIN_WIDTH..=MAX_WIDTH).contains(&width) && (MIN_HEIGHT..=MAX_HEIGHT).contains(&height)
    }
    /// Records the window the manager created for `owner`; returns its handle byte.
    pub fn open(
        &mut self,
        index: usize,
        owner: usize,
        handle: Handle,
        slot: usize,
        width: u16,
        height: u16,
    ) -> u8 {
        // Generations 1..=63 so that handle byte 0 is never valid.
        self.generations[index] = self.generations[index] % 63 + 1;
        self.apps[index] = Some(App {
            owner,
            handle,
            slot,
            width,
            height,
            background: 0xffffff,
            ops: [Op::EMPTY; MAX_OPS],
            count: 0,
            waiter: None,
            events: [None; EVENTS],
            head: 0,
            length: 0,
            dropped: 0,
        });
        self.generations[index] << 2 | index as u8
    }
    /// The window a request from `owner` names, or the status to refuse with.
    pub fn find(&mut self, owner: usize, id: u8) -> Result<usize, u8> {
        let index = (id & 3) as usize;
        let generation = id >> 2;
        match self.apps.get(index).and_then(|app| app.as_ref()) {
            Some(app) if self.generations[index] == generation && app.owner == owner => Ok(index),
            _ => Err(gui::DENIED),
        }
    }
    /// Removes a window; the token of a parked `WAIT`, if any, is returned so the
    /// caller can answer it with `Event::Close`.
    pub fn close(&mut self, index: usize) -> Option<usize> {
        self.apps
            .get_mut(index)
            .and_then(Option::take)
            .and_then(|app| app.waiter)
    }
    /// The application whose window the manager knows as `slot` with this handle.
    pub fn by_slot(&self, slot: usize) -> Option<usize> {
        self.apps
            .iter()
            .position(|app| app.as_ref().is_some_and(|app| app.slot == slot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zos_console::keyboard::Key;

    fn open(apps: &mut Apps, owner: usize) -> (usize, u8) {
        let index = apps.reserve(owner).unwrap();
        let id = apps.open(index, owner, Handle(0), index, 200, 100);
        (index, id)
    }

    #[test]
    fn a_window_belongs_to_its_creator_and_stale_handles_never_work_again() {
        let mut apps = Apps::new();
        let (index, id) = open(&mut apps, 9);
        assert_ne!(id, 0);
        assert_eq!(apps.find(9, id), Ok(index));
        assert_eq!(apps.find(10, id), Err(gui::DENIED), "another process");
        assert_eq!(
            apps.find(9, id ^ 4),
            Err(gui::DENIED),
            "a guessed generation"
        );
        assert_eq!(apps.find(9, 0), Err(gui::DENIED));
        assert_eq!(apps.close(index), None);
        assert_eq!(apps.find(9, id), Err(gui::DENIED), "closed");
        // The slot is reused for the next window; the old handle still fails.
        let (again, new_id) = open(&mut apps, 9);
        assert_eq!(again, index);
        assert_ne!(new_id, id);
        assert_eq!(apps.find(9, id), Err(gui::DENIED));
        assert_eq!(apps.find(9, new_id), Ok(again));
    }

    #[test]
    fn handle_generations_wrap_without_ever_being_zero() {
        let mut apps = Apps::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let (index, id) = open(&mut apps, 5);
            assert_ne!(id >> 2, 0);
            assert_eq!(id & 3, index as u8);
            seen.insert(id);
            apps.close(index);
        }
        assert_eq!(seen.len(), 63, "63 distinct generations before they repeat");
    }

    #[test]
    fn there_is_room_for_a_few_windows_and_one_per_process() {
        let mut apps = Apps::new();
        for owner in 0..MAX_APPS {
            open(&mut apps, 10 + owner);
        }
        assert_eq!(apps.reserve(99), Err(gui::NO_WINDOW));
        assert_eq!(apps.reserve(10), Err(gui::DENIED), "already has a window");
        assert_eq!(apps.count(), MAX_APPS);
        apps.close(2);
        assert_eq!(apps.reserve(99), Ok(2));
        assert!(Apps::size_ok(64, 48) && Apps::size_ok(900, 600));
        for (w, h) in [
            (63, 100),
            (100, 47),
            (901, 100),
            (100, 601),
            (0, 0),
            (u16::MAX, u16::MAX),
        ] {
            assert!(!Apps::size_ok(w, h), "{w}x{h}");
        }
    }

    #[test]
    fn the_drawing_list_is_bounded_and_clear_empties_it() {
        let mut apps = Apps::new();
        let (index, _) = open(&mut apps, 1);
        let app = apps.app_mut(index).unwrap();
        for i in 0..MAX_OPS {
            assert_eq!(app.rect(i as i16, 0, 4, 4, 0x123456), gui::OK);
        }
        assert_eq!(app.rect(0, 0, 1, 1, 0), gui::FULL);
        assert_eq!(app.text(0, 0, 0, b"x"), gui::FULL);
        assert_eq!(app.ops().len(), MAX_OPS);
        app.clear(0x102030);
        assert_eq!(app.ops().len(), 0);
        assert_eq!(app.background, 0x102030);
        assert_eq!(app.text(3, -4, 0xaabbcc, b"hi\x01\xff"), gui::OK);
        let op = app.ops()[0];
        assert!(op.is_text && (op.x, op.y, op.color) == (3, -4, 0xaabbcc));
        assert_eq!(op.text(), b"hi??", "control and high bytes are never drawn");
        assert_eq!(app.text(0, 0, 0, &[b'a'; MAX_TEXT + 1]), gui::INVALID);
        // Colors cannot carry stray high bits into the pixel.
        app.rect(0, 0, 1, 1, 0xff12_3456);
        assert_eq!(app.ops()[1].color, 0x12_3456);
    }

    #[test]
    fn events_queue_in_order_and_a_parked_wait_is_answered_directly() {
        let mut apps = Apps::new();
        let (index, _) = open(&mut apps, 1);
        let app = apps.app_mut(index).unwrap();
        let keys = [Key::Character(b'a'), Key::Enter, Key::Left];
        for key in keys {
            assert_eq!(app.push_event(Event::Key(key)), None);
        }
        for key in keys {
            assert_eq!(app.wait(7), Ok(Waited::Event(Event::Key(key))));
        }
        // Nothing queued: the first wait parks, a second wait is refused.
        assert_eq!(app.wait(7), Ok(Waited::Parked));
        assert_eq!(app.wait(8), Err(gui::DENIED));
        // The next event goes straight to the parked token and is not queued.
        let click = Event::Click {
            x: 5,
            y: 6,
            buttons: 1,
        };
        assert_eq!(app.push_event(click), Some((7, click)));
        assert_eq!(app.wait(7), Ok(Waited::Parked));
    }

    #[test]
    fn a_full_queue_drops_input_but_never_the_close() {
        let mut apps = Apps::new();
        let (index, _) = open(&mut apps, 1);
        let app = apps.app_mut(index).unwrap();
        for i in 0..EVENTS + 5 {
            let key = Key::Character(b'a' + (i % 20) as u8);
            assert_eq!(app.push_event(Event::Key(key)), None);
        }
        assert_eq!(app.dropped, 5);
        app.push_event(Event::Close);
        assert_eq!(app.dropped, 5, "the close is not counted as dropped");
        let mut last = None;
        while let Ok(Waited::Event(event)) = app.wait(1) {
            last = Some(event);
        }
        assert_eq!(
            last,
            Some(Event::Close),
            "close arrives, after the older input"
        );
    }

    #[test]
    fn closing_returns_the_parked_waiter_so_it_can_be_told() {
        let mut apps = Apps::new();
        let (index, id) = open(&mut apps, 3);
        assert_eq!(apps.app_mut(index).unwrap().wait(42), Ok(Waited::Parked));
        assert_eq!(apps.by_slot(index), Some(index));
        assert_eq!(apps.close(index), Some(42));
        assert_eq!(apps.close(index), None, "closing twice is harmless");
        assert_eq!(apps.find(3, id), Err(gui::DENIED));
        assert_eq!(apps.by_slot(index), None);
        assert!(apps.app(index).is_none() && apps.app(99).is_none());
    }
}
