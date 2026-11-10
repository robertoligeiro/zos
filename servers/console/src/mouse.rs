#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    pub dx: i16,
    pub dy: i16,
    pub buttons: u8,
}

#[derive(Default)]
pub struct Mouse {
    bytes: [u8; 3],
    position: usize,
}

impl Mouse {
    pub fn feed(&mut self, byte: u8) -> Option<Motion> {
        if self.position == 0 && byte & 8 == 0 {
            return None;
        }
        self.bytes[self.position] = byte;
        self.position += 1;
        if self.position != 3 {
            return None;
        }
        self.position = 0;
        let [flags, x, y] = self.bytes;
        if flags & 0xc0 != 0 {
            return None;
        }
        Some(Motion {
            dx: x as i16 - if flags & 0x10 != 0 { 256 } else { 0 },
            dy: y as i16 - if flags & 0x20 != 0 { 256 } else { 0 },
            buttons: flags & 7,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sync_signed_nine_bit_motion_buttons_and_overflow() {
        let mut mouse = Mouse::default();
        assert_eq!(mouse.feed(0), None);
        mouse.feed(0x1b);
        mouse.feed(0);
        assert_eq!(
            mouse.feed(127),
            Some(Motion {
                dx: -256,
                dy: 127,
                buttons: 3
            })
        );
        mouse.feed(0xc8);
        mouse.feed(20);
        assert_eq!(mouse.feed(30), None);
        mouse.feed(0x28);
        mouse.feed(255);
        assert_eq!(
            mouse.feed(255),
            Some(Motion {
                dx: 255,
                dy: -1,
                buttons: 0
            })
        );
    }
}
