//! The desktop application protocol: how a program gets a window on the ZOS desktop.
//!
//! An application is an ordinary program. It sends small requests (at most 64 bytes)
//! to the window manager on the console channel; the manager identifies the sender
//! from the kernel's reply token, never from the message. The reply to every request
//! starts with a status byte. Windows are named by a one-byte handle that carries a
//! generation, so a closed window's handle never works again.
//!
//! The manager keeps, per window, a bounded list of drawing operations (rectangles
//! and text) and repaints from it whenever the window is exposed; an application
//! redraws by sending `CLEAR` and its operations again. Input comes back through
//! `WAIT`, which the manager answers when there is an event (a parked reply, so the
//! application blocks in the kernel instead of polling).

use crate::keyboard::Key;
use zos_libos as os;

/// Requests, after the console protocol's own opcodes (0..=7).
pub const CREATE: u8 = 16;
pub const CLEAR: u8 = 17;
pub const RECT: u8 = 18;
pub const TEXT: u8 = 19;
pub const PRESENT: u8 = 20;
pub const WAIT: u8 = 21;
pub const CLOSE: u8 = 22;

/// Reply status codes.
pub const OK: u8 = 0;
pub const INVALID: u8 = 1;
/// The window's drawing list is full: `CLEAR` and draw less.
pub const FULL: u8 = 2;
/// Not your window (or it is gone).
pub const DENIED: u8 = 3;
/// No room for another window.
pub const NO_WINDOW: u8 = 4;

/// Event kinds in a `WAIT` reply: `[OK, kind, a, b, c, d, e]`.
pub const EVENT_KEY: u8 = 1;
pub const EVENT_CLICK: u8 = 2;
pub const EVENT_CLOSE: u8 = 3;

pub const MAX_TEXT: usize = 44;
/// Largest content area an application may ask for.
pub const MAX_WIDTH: u16 = 900;
pub const MAX_HEIGHT: u16 = 600;
pub const TITLE: usize = 24;

/// The desktop programs, in launcher order. Each is installed as `/bin/<name>`.
pub const APPS: [&[u8]; 2] = [b"browser", b"calc"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    /// A mouse button went down at window-relative `x`, `y` (`buttons` as PS/2).
    Click {
        x: i16,
        y: i16,
        buttons: u8,
    },
    /// The user closed the window (the handle is already invalid).
    Close,
}

impl Event {
    pub fn encode(self) -> [u8; 6] {
        match self {
            Event::Key(key) => {
                let [a, b] = key.encode();
                [EVENT_KEY, a, b, 0, 0, 0]
            }
            Event::Click { x, y, buttons } => {
                let (x, y) = (x.to_le_bytes(), y.to_le_bytes());
                [EVENT_CLICK, x[0], x[1], y[0], y[1], buttons]
            }
            Event::Close => [EVENT_CLOSE, 0, 0, 0, 0, 0],
        }
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [EVENT_KEY, a, b, ..] => Key::decode(&[*a, *b]).map(Event::Key),
            [EVENT_CLICK, x0, x1, y0, y1, buttons, ..] => Some(Event::Click {
                x: i16::from_le_bytes([*x0, *x1]),
                y: i16::from_le_bytes([*y0, *y1]),
                buttons: *buttons,
            }),
            [EVENT_CLOSE, ..] => Some(Event::Close),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The manager refused (status byte) or the kernel did.
    Status(u8),
    Kernel(os::Error),
    Corrupt,
}

pub type Result<T> = core::result::Result<T, Error>;

fn request(message: &[u8], reply: &mut [u8]) -> Result<usize> {
    let length = os::send(crate::CONSOLE, message, reply).map_err(Error::Kernel)?;
    match reply.first() {
        Some(&OK) if length >= 1 => Ok(length),
        Some(&status) if length >= 1 => Err(Error::Status(status)),
        _ => Err(Error::Corrupt),
    }
}

/// A window on the desktop, owned by the calling program.
pub struct Window {
    id: u8,
}

impl Window {
    /// Opens a window with a content area of `width` x `height` pixels.
    pub fn create(title: &[u8], width: u16, height: u16) -> Result<Self> {
        let mut message = [0u8; 64];
        message[0] = CREATE;
        message[1..3].copy_from_slice(&width.to_le_bytes());
        message[3..5].copy_from_slice(&height.to_le_bytes());
        let length = title.len().min(TITLE);
        message[5..5 + length].copy_from_slice(&title[..length]);
        let mut reply = [0u8; 2];
        let count = request(&message[..5 + length], &mut reply)?;
        if count != 2 {
            return Err(Error::Corrupt);
        }
        Ok(Self { id: reply[1] })
    }

    /// Empties the drawing list and sets the background color (0xRRGGBB).
    pub fn clear(&self, color: u32) -> Result<()> {
        let c = color.to_be_bytes();
        request(&[CLEAR, self.id, c[1], c[2], c[3]], &mut [0; 1]).map(|_| ())
    }

    pub fn rect(&self, x: i16, y: i16, width: u16, height: u16, color: u32) -> Result<()> {
        let mut message = [0u8; 14];
        message[0] = RECT;
        message[1] = self.id;
        message[2..4].copy_from_slice(&x.to_le_bytes());
        message[4..6].copy_from_slice(&y.to_le_bytes());
        message[6..8].copy_from_slice(&width.to_le_bytes());
        message[8..10].copy_from_slice(&height.to_le_bytes());
        message[10..13].copy_from_slice(&color.to_be_bytes()[1..]);
        request(&message[..13], &mut [0; 1]).map(|_| ())
    }

    /// Draws text (8x16 glyphs) with its top-left corner at `x`, `y`. Longer text is
    /// sent in pieces.
    pub fn text(&self, x: i16, y: i16, color: u32, text: &[u8]) -> Result<()> {
        for (index, part) in text.chunks(MAX_TEXT).enumerate() {
            let mut message = [0u8; 64];
            message[0] = TEXT;
            message[1] = self.id;
            let at = x.saturating_add((index * MAX_TEXT * 8) as i16);
            message[2..4].copy_from_slice(&at.to_le_bytes());
            message[4..6].copy_from_slice(&y.to_le_bytes());
            message[6..9].copy_from_slice(&color.to_be_bytes()[1..]);
            message[9..9 + part.len()].copy_from_slice(part);
            request(&message[..9 + part.len()], &mut [0; 1])?;
        }
        Ok(())
    }

    /// Shows what has been drawn since the last `clear`.
    pub fn present(&self) -> Result<()> {
        request(&[PRESENT, self.id], &mut [0; 1]).map(|_| ())
    }

    /// Waits for the next event.
    pub fn wait(&self) -> Result<Event> {
        let mut reply = [0u8; 8];
        let count = request(&[WAIT, self.id], &mut reply)?;
        Event::decode(&reply[1..count]).ok_or(Error::Corrupt)
    }

    pub fn close(self) -> Result<()> {
        request(&[CLOSE, self.id], &mut [0; 1]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip() {
        for event in [
            Event::Key(Key::Character(b'q')),
            Event::Key(Key::Enter),
            Event::Key(Key::Alt(b'h')),
            Event::Click {
                x: -3,
                y: 300,
                buttons: 5,
            },
            Event::Close,
        ] {
            assert_eq!(Event::decode(&event.encode()), Some(event));
        }
        assert_eq!(Event::decode(&[]), None);
        assert_eq!(Event::decode(&[EVENT_KEY, 0, 0]), None);
        assert_eq!(Event::decode(&[99, 0, 0, 0, 0, 0]), None);
    }
}
