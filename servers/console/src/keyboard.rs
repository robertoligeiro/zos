#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Character(u8),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    Clear,
    Tab,
    /// A key pressed while Alt is held: the unshifted character, `\t` for Tab, or
    /// `ALT_LEFT`..`ALT_DOWN` for the arrow keys. Used for window-manager shortcuts.
    Alt(u8),
}

pub const ALT_LEFT: u8 = 0x80;
pub const ALT_RIGHT: u8 = 0x81;
pub const ALT_UP: u8 = 0x82;
pub const ALT_DOWN: u8 = 0x83;

#[derive(Default)]
pub struct Keyboard {
    shifts: u8,
    controls: u8,
    alts: u8,
    caps: bool,
    caps_down: bool,
    extended: bool,
    pause: u8,
}

impl Keyboard {
    pub fn feed(&mut self, byte: u8) -> Option<Key> {
        if self.pause != 0 {
            self.pause -= 1;
            return None;
        }
        if byte == 0xe1 {
            self.pause = 5;
            self.extended = false;
            return None;
        }
        if byte == 0xe0 {
            self.extended = true;
            return None;
        }
        let extended = core::mem::take(&mut self.extended);
        let released = byte & 0x80 != 0;
        let scan = byte & 0x7f;
        let update = |bits: &mut u8, bit: u8| {
            if released {
                *bits &= !bit
            } else {
                *bits |= bit
            }
        };
        if scan == 0x1d {
            update(&mut self.controls, if extended { 2 } else { 1 });
            return None;
        }
        // Left Alt is 0x38; right Alt (AltGr on many layouts) is the extended 0x38.
        if scan == 0x38 {
            update(&mut self.alts, if extended { 2 } else { 1 });
            return None;
        }
        if !extended && matches!(scan, 0x2a | 0x36) {
            update(&mut self.shifts, if scan == 0x2a { 1 } else { 2 });
            return None;
        }
        if !extended && scan == 0x3a {
            if !released && !self.caps_down {
                self.caps = !self.caps;
            }
            self.caps_down = !released;
            return None;
        }
        if released {
            return None;
        }
        if extended {
            let alt = self.alts != 0;
            return match scan {
                0x1c => Some(Key::Enter),
                0x35 => Some(Key::Character(b'/')),
                0x47 => Some(Key::Home),
                0x4f => Some(Key::End),
                0x4b if alt => Some(Key::Alt(ALT_LEFT)),
                0x4d if alt => Some(Key::Alt(ALT_RIGHT)),
                0x48 if alt => Some(Key::Alt(ALT_UP)),
                0x50 if alt => Some(Key::Alt(ALT_DOWN)),
                0x4b => Some(Key::Left),
                0x4d => Some(Key::Right),
                0x48 => Some(Key::Up),
                0x50 => Some(Key::Down),
                0x53 => Some(Key::Delete),
                _ => None,
            };
        }
        match scan {
            0x1c => return Some(Key::Enter),
            0x0e => return Some(Key::Backspace),
            0x0f => {
                return Some(if self.alts != 0 {
                    Key::Alt(b'\t')
                } else {
                    Key::Tab
                })
            }
            _ => (),
        }
        let normal = match scan {
            0x02..=0x0d => b"1234567890-="[(scan - 2) as usize],
            0x10..=0x1b => b"qwertyuiop[]"[(scan - 0x10) as usize],
            0x1e..=0x28 => b"asdfghjkl;'"[(scan - 0x1e) as usize],
            0x29 => b'`',
            0x2b => b'\\',
            0x2c..=0x35 => b"zxcvbnm,./"[(scan - 0x2c) as usize],
            0x39 => b' ',
            _ => return None,
        };
        if self.alts != 0 {
            return Some(Key::Alt(normal));
        }
        if self.controls != 0 {
            return match normal {
                b'a' => Some(Key::Home),
                b'e' => Some(Key::End),
                b'l' => Some(Key::Clear),
                _ => None,
            };
        }
        let shifted = self.shifts != 0;
        let ch = if normal.is_ascii_lowercase() {
            if shifted ^ self.caps {
                normal.to_ascii_uppercase()
            } else {
                normal
            }
        } else if shifted {
            match normal {
                b'1'..=b'9' => b"!@#$%^&*("[(normal - b'1') as usize],
                b'0' => b')',
                b'-' => b'_',
                b'=' => b'+',
                b'[' => b'{',
                b']' => b'}',
                b';' => b':',
                b'\'' => b'"',
                b'`' => b'~',
                b'\\' => b'|',
                b',' => b'<',
                b'.' => b'>',
                b'/' => b'?',
                _ => normal,
            }
        } else {
            normal
        };
        Some(Key::Character(ch))
    }
}

impl Key {
    pub fn encode(self) -> [u8; 2] {
        match self {
            Self::Character(ch) => [0, ch],
            Self::Enter => [1, 0],
            Self::Backspace => [2, 0],
            Self::Delete => [3, 0],
            Self::Left => [4, 0],
            Self::Right => [5, 0],
            Self::Home => [6, 0],
            Self::End => [7, 0],
            Self::Up => [8, 0],
            Self::Down => [9, 0],
            Self::Clear => [10, 0],
            Self::Tab => [12, 0],
            Self::Alt(ch) => [11, ch],
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [0, ch @ 32..=126] => Some(Self::Character(*ch)),
            [11, ch @ (b'\t' | 32..=126 | ALT_LEFT..=ALT_DOWN)] => Some(Self::Alt(*ch)),
            [12, 0] => Some(Self::Tab),
            [1, 0] => Some(Self::Enter),
            [2, 0] => Some(Self::Backspace),
            [3, 0] => Some(Self::Delete),
            [4, 0] => Some(Self::Left),
            [5, 0] => Some(Self::Right),
            [6, 0] => Some(Self::Home),
            [7, 0] => Some(Self::End),
            [8, 0] => Some(Self::Up),
            [9, 0] => Some(Self::Down),
            [10, 0] => Some(Self::Clear),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modifiers_releases_and_extended_keys() {
        let mut keyboard = Keyboard::default();
        assert_eq!(keyboard.feed(0x1e), Some(Key::Character(b'a')));
        assert_eq!(keyboard.feed(0x9e), None);
        keyboard.feed(0x2a);
        keyboard.feed(0x36);
        keyboard.feed(0xaa);
        assert_eq!(keyboard.feed(0x1e), Some(Key::Character(b'A')));
        keyboard.feed(0xb6);
        keyboard.feed(0x3a);
        keyboard.feed(0x3a);
        keyboard.feed(0xba);
        assert_eq!(keyboard.feed(0x1e), Some(Key::Character(b'A')));
        keyboard.feed(0x2a);
        assert_eq!(keyboard.feed(0x1e), Some(Key::Character(b'a')));
        assert_eq!(keyboard.feed(0x02), Some(Key::Character(b'!')));
        keyboard.feed(0xe0);
        assert_eq!(keyboard.feed(0x4b), Some(Key::Left));
        keyboard.feed(0xe0);
        assert_eq!(keyboard.feed(0xcb), None);
        keyboard.feed(0xe1);
        for byte in [0x1d, 0x45, 0xe1, 0x9d, 0xc5] {
            assert_eq!(keyboard.feed(byte), None);
        }
        keyboard.feed(0xaa);
        keyboard.feed(0x3a);
        keyboard.feed(0xba);
        assert_eq!(keyboard.feed(0x1e), Some(Key::Character(b'a')));
        keyboard.feed(0xe0);
        keyboard.feed(0x1d);
        assert_eq!(keyboard.feed(0x26), Some(Key::Clear));
        keyboard.feed(0xe0);
        keyboard.feed(0x9d);
        assert_eq!(keyboard.feed(0x26), Some(Key::Character(b'l')));
    }

    #[test]
    fn tab_and_alt_combinations_for_window_management() {
        let mut keyboard = Keyboard::default();
        assert_eq!(keyboard.feed(0x0f), Some(Key::Tab));
        assert_eq!(keyboard.feed(0x8f), None);
        // Left Alt held: Tab, letters (unshifted), digits and the arrow keys.
        assert_eq!(keyboard.feed(0x38), None);
        assert_eq!(keyboard.feed(0x0f), Some(Key::Alt(b'\t')));
        assert_eq!(keyboard.feed(0x32), Some(Key::Alt(b'm')));
        keyboard.feed(0x2a);
        assert_eq!(
            keyboard.feed(0x32),
            Some(Key::Alt(b'm')),
            "Shift does not change the shortcut"
        );
        keyboard.feed(0xaa);
        assert_eq!(keyboard.feed(0x02), Some(Key::Alt(b'1')));
        for (scan, key) in [
            (0x4b, ALT_LEFT),
            (0x4d, ALT_RIGHT),
            (0x48, ALT_UP),
            (0x50, ALT_DOWN),
        ] {
            keyboard.feed(0xe0);
            assert_eq!(keyboard.feed(scan), Some(Key::Alt(key)));
        }
        // Releasing Alt restores normal keys; Ctrl combinations are unaffected by it.
        keyboard.feed(0xb8);
        assert_eq!(keyboard.feed(0x32), Some(Key::Character(b'm')));
        keyboard.feed(0xe0);
        assert_eq!(keyboard.feed(0x4b), Some(Key::Left));
        // Right Alt (extended 0x38) counts too, and each Alt is tracked separately.
        keyboard.feed(0xe0);
        keyboard.feed(0x38);
        assert_eq!(keyboard.feed(0x11), Some(Key::Alt(b'w')));
        keyboard.feed(0x38);
        assert_eq!(keyboard.feed(0x11), Some(Key::Alt(b'w')));
        keyboard.feed(0xe0);
        keyboard.feed(0xb8);
        keyboard.feed(0xb8);
        assert_eq!(keyboard.feed(0x11), Some(Key::Character(b'w')));
    }

    #[test]
    fn every_key_survives_encoding_and_hostile_messages_are_rejected() {
        for key in [
            Key::Character(b'x'),
            Key::Enter,
            Key::Tab,
            Key::Alt(b'\t'),
            Key::Alt(b'q'),
            Key::Alt(ALT_LEFT),
            Key::Alt(ALT_DOWN),
            Key::Clear,
        ] {
            assert_eq!(Key::decode(&key.encode()), Some(key));
        }
        for bad in [
            &[11, 0][..],
            &[11, 31],
            &[11, 127],
            &[11, 0x84],
            &[11, 0x7f],
            &[12, 1],
            &[13, 0],
            &[11],
            &[],
        ] {
            assert_eq!(Key::decode(bad), None, "{bad:?}");
        }
    }
}
