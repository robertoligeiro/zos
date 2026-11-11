use crate::keyboard::{Key, Keyboard};

#[derive(Default)]
pub struct BootKeyboard {
    keyboard: Keyboard,
    previous: [u8; 8],
}

fn scan(usage: u8) -> Option<(u8, bool)> {
    let code = match usage {
        4..=29 => [
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31,
            0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ][(usage - 4) as usize],
        30..=38 => usage - 28,
        39 => 0x0b,
        40 => 0x1c,
        42 => 0x0e,
        44 => 0x39,
        45 => 0x0c,
        46 => 0x0d,
        47 => 0x1a,
        48 => 0x1b,
        49 => 0x2b,
        51 => 0x27,
        52 => 0x28,
        53 => 0x29,
        54 => 0x33,
        55 => 0x34,
        56 => 0x35,
        57 => 0x3a,
        74 => return Some((0x47, true)),
        76 => return Some((0x53, true)),
        77 => return Some((0x4f, true)),
        79 => return Some((0x4d, true)),
        80 => return Some((0x4b, true)),
        81 => return Some((0x50, true)),
        82 => return Some((0x48, true)),
        _ => return None,
    };
    Some((code, false))
}

impl BootKeyboard {
    pub fn report(&mut self, bytes: [u8; 8], mut emit: impl FnMut(Key)) {
        if bytes[2..].iter().any(|code| (1..=3).contains(code)) {
            return;
        }
        for &(bit, code, extended) in &[
            (0, 0x1d, false),
            (4, 0x1d, true),
            (1, 0x2a, false),
            (5, 0x36, false),
        ] {
            if (bytes[0] ^ self.previous[0]) & (1 << bit) != 0 {
                if extended {
                    self.keyboard.feed(0xe0);
                }
                self.keyboard
                    .feed(code | if bytes[0] & (1 << bit) == 0 { 0x80 } else { 0 });
            }
        }
        for &usage in &self.previous[2..] {
            if usage != 0 && !bytes[2..].contains(&usage) {
                if let Some((code, extended)) = scan(usage) {
                    if extended {
                        self.keyboard.feed(0xe0);
                    }
                    self.keyboard.feed(code | 0x80);
                }
            }
        }
        for (index, &usage) in bytes[2..].iter().enumerate() {
            if usage != 0
                && !self.previous[2..].contains(&usage)
                && !bytes[2..2 + index].contains(&usage)
            {
                if let Some((code, extended)) = scan(usage) {
                    if extended {
                        self.keyboard.feed(0xe0);
                    }
                    if let Some(key) = self.keyboard.feed(code) {
                        emit(key);
                    }
                }
            }
        }
        self.previous = bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usb_reports_modifiers_rollover_and_editing() {
        let mut keyboard = BootKeyboard::default();
        let mut found = None;
        let mut report = |bytes| {
            found = None;
            keyboard.report(bytes, |key| found = Some(key));
            found
        };
        assert_eq!(report([2, 0, 4, 0, 0, 0, 0, 0]), Some(Key::Character(b'A')));
        assert_eq!(report([2, 0, 4, 0, 0, 0, 0, 0]), None);
        assert_eq!(report([0, 0, 1, 1, 1, 1, 1, 1]), None);
        assert_eq!(report([0; 8]), None);
        assert_eq!(report([1, 0, 15, 0, 0, 0, 0, 0]), Some(Key::Clear));
        report([0; 8]);
        assert_eq!(report([0, 0, 82, 0, 0, 0, 0, 0]), Some(Key::Up));
        report([0; 8]);
        assert_eq!(report([0, 0, 42, 0, 0, 0, 0, 0]), Some(Key::Backspace));
        report([0; 8]);
        assert_eq!(report([0, 0, 57, 0, 0, 0, 0, 0]), None);
        report([0; 8]);
        assert_eq!(report([0, 0, 4, 0, 0, 0, 0, 0]), Some(Key::Character(b'A')));
    }
}
