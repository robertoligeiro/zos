//! HID report descriptors: parsed once into a flat field list, then used to turn input
//! reports into pointer motion. Two kinds of device are understood:
//!
//! * a mouse (relative X/Y and buttons, what a touchpad reports in "mouse mode"); and
//! * a Windows Precision Touchpad (absolute contacts with tip switch, contact id, X, Y
//!   and a click button), turned into relative motion with tap-to-click.

pub const MAX_FIELDS: usize = 64;
const MAX_REPORT_IDS: usize = 8;
const MAX_USAGES: usize = 16;

const PAGE_DESKTOP: u16 = 0x01;
const PAGE_BUTTON: u16 = 0x09;
const PAGE_DIGITIZER: u16 = 0x0d;
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_FINGER: u16 = 0x22;
const USAGE_TIP: u16 = 0x42;
const USAGE_CONTACT_ID: u16 = 0x51;

/// Full-width pad swipe moves the pointer about this many pixels.
const SWIPE_X: i64 = 1400;
const SWIPE_Y: i64 = 900;
/// A touch shorter than this (10 ms ticks) and nearly stationary is a tap.
const TAP_TICKS: u64 = 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub report_id: u8,
    /// Bit offset inside the report, after the report-id byte.
    pub bit: u16,
    pub size: u8,
    pub page: u16,
    pub usage: u16,
    pub min: i32,
    pub max: i32,
    pub relative: bool,
    /// 1-based index of the finger collection the field is in; 0 outside one.
    pub finger: u8,
}

pub struct Layout {
    fields: [Field; MAX_FIELDS],
    count: usize,
    /// Reports start with a report-id byte (the descriptor declared at least one id).
    pub report_ids: bool,
}

fn item_value(bytes: &[u8], signed: bool) -> i32 {
    let mut value: i64 = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        value |= (byte as i64) << (8 * index);
    }
    if signed && !bytes.is_empty() {
        let bits = 8 * bytes.len();
        if value & (1 << (bits - 1)) != 0 {
            value -= 1 << bits;
        }
    }
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

impl Layout {
    pub fn parse(descriptor: &[u8]) -> Self {
        let mut layout = Self {
            fields: [Field::default(); MAX_FIELDS],
            count: 0,
            report_ids: false,
        };
        let (mut page, mut min, mut max) = (0u16, 0i32, 0i32);
        let (mut size, mut count, mut report_id) = (0u32, 0u32, 0u8);
        let mut usages = [0u16; MAX_USAGES];
        let mut usage_count = 0;
        let (mut range_min, mut range_max, mut have_range) = (0u16, 0u16, false);
        let mut offsets = [(0u8, 0u16); MAX_REPORT_IDS];
        let mut offsets_used = 0;
        let (mut depth, mut finger_depth, mut finger, mut fingers) = (0u32, 0u32, 0u8, 0u8);
        let mut at = 0;
        while at < descriptor.len() {
            let prefix = descriptor[at];
            if prefix == 0xfe {
                // Long item: skip it whole.
                let length = descriptor.get(at + 1).copied().unwrap_or(0) as usize;
                at += 3 + length;
                continue;
            }
            let length = [0, 1, 2, 4][(prefix & 3) as usize];
            let Some(data) = descriptor.get(at + 1..at + 1 + length) else {
                break;
            };
            at += 1 + length;
            let (kind, tag) = (prefix >> 2 & 3, prefix >> 4);
            match (kind, tag) {
                // Global items.
                (1, 0) => page = item_value(data, false) as u16,
                (1, 1) => min = item_value(data, true),
                (1, 2) => {
                    // A positive-looking maximum that only fits unsigned (e.g. 0xff).
                    max = item_value(data, min < 0);
                }
                (1, 7) => size = item_value(data, false) as u32,
                (1, 8) => {
                    report_id = item_value(data, false) as u8;
                    layout.report_ids = true;
                }
                (1, 9) => count = item_value(data, false) as u32,
                // Local items.
                (2, 0) => {
                    if usage_count < MAX_USAGES {
                        usages[usage_count] = item_value(data, false) as u16;
                        usage_count += 1;
                    }
                }
                (2, 1) => {
                    range_min = item_value(data, false) as u16;
                    have_range = true;
                }
                (2, 2) => range_max = item_value(data, false) as u16,
                // Main items.
                (0, 8) => {
                    let flags = item_value(data, false);
                    let slot = match offsets[..offsets_used]
                        .iter()
                        .position(|o| o.0 == report_id)
                    {
                        Some(slot) => slot,
                        None if offsets_used < MAX_REPORT_IDS => {
                            offsets[offsets_used] = (report_id, 0);
                            offsets_used += 1;
                            offsets_used - 1
                        }
                        None => 0,
                    };
                    let constant = flags & 1 != 0;
                    let array = flags & 2 == 0;
                    if !constant && !array && size > 0 && size <= 32 {
                        for index in 0..count.min(MAX_FIELDS as u32) as usize {
                            let usage = if have_range {
                                range_min.saturating_add(index as u16).min(range_max)
                            } else if index < usage_count {
                                usages[index]
                            } else if usage_count > 0 {
                                usages[usage_count - 1]
                            } else {
                                0
                            };
                            if layout.count < MAX_FIELDS {
                                layout.fields[layout.count] = Field {
                                    report_id,
                                    bit: offsets[slot].1 + (index as u32 * size) as u16,
                                    size: size as u8,
                                    page,
                                    usage,
                                    min,
                                    max,
                                    relative: flags & 4 != 0,
                                    finger,
                                };
                                layout.count += 1;
                            }
                        }
                    }
                    offsets[slot].1 = offsets[slot]
                        .1
                        .saturating_add(size.saturating_mul(count).min(u16::MAX as u32) as u16);
                    usage_count = 0;
                    have_range = false;
                }
                (0, 10) => {
                    depth += 1;
                    if page == PAGE_DIGITIZER && usage_count > 0 && usages[0] == USAGE_FINGER {
                        fingers = fingers.saturating_add(1);
                        finger = fingers;
                        finger_depth = depth;
                    }
                    usage_count = 0;
                    have_range = false;
                }
                (0, 12) => {
                    if finger != 0 && depth == finger_depth {
                        finger = 0;
                    }
                    depth = depth.saturating_sub(1);
                }
                (0, _) => {
                    usage_count = 0;
                    have_range = false;
                }
                _ => (),
            }
        }
        layout
    }

    pub fn fields(&self) -> &[Field] {
        &self.fields[..self.count]
    }

    fn find(
        &self,
        report_id: u8,
        page: u16,
        usage: u16,
        finger: u8,
        relative: bool,
    ) -> Option<Field> {
        self.fields().iter().copied().find(|field| {
            field.report_id == report_id
                && field.page == page
                && field.usage == usage
                && field.finger == finger
                && field.relative == relative
        })
    }

    /// True when some report carries relative X and Y.
    pub fn is_mouse(&self) -> bool {
        self.fields()
            .iter()
            .any(|f| f.page == PAGE_DESKTOP && f.usage == USAGE_X && f.relative)
    }

    /// True when some report carries absolute finger contacts.
    pub fn is_touchpad(&self) -> bool {
        self.fields()
            .iter()
            .any(|f| f.page == PAGE_DIGITIZER && f.usage == USAGE_TIP && f.finger != 0)
    }
}

/// Reads `field` from a report (after the report-id byte); 0 if it does not fit.
pub fn extract(report: &[u8], field: &Field) -> i32 {
    let mut value: u64 = 0;
    for index in 0..field.size as usize {
        let bit = field.bit as usize + index;
        let Some(&byte) = report.get(bit / 8) else {
            return 0;
        };
        value |= ((byte >> (bit % 8) & 1) as u64) << index;
    }
    if field.min < 0 && field.size < 32 && value >> (field.size - 1) & 1 != 0 {
        value |= u64::MAX << field.size;
    }
    value as i64 as i32
}

/// Pointer motion as HID defines it: positive `dy` is down the screen. (The pointer
/// events the rest of ZOS passes around use the PS/2 convention, up is positive; the
/// input service flips it.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    pub dx: i16,
    pub dy: i16,
    /// Bit 0 left, bit 1 right, bit 2 middle.
    pub buttons: u8,
}

#[derive(Clone, Copy)]
struct Track {
    id: i64,
    x: i64,
    y: i64,
    carry_x: i64,
    carry_y: i64,
    start: u64,
    moved: i64,
}

pub struct Decoder {
    layout: Layout,
    track: Option<Track>,
    buttons: u8,
    release_tap: bool,
}

impl Decoder {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            track: None,
            buttons: 0,
            release_tap: false,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Called when a poll produced no report: finishes a pending tap click.
    pub fn idle(&mut self) -> Option<Motion> {
        if self.release_tap {
            self.release_tap = false;
            self.buttons = 0;
            return Some(Motion {
                dx: 0,
                dy: 0,
                buttons: 0,
            });
        }
        None
    }

    /// Turns one input report (starting with its report id, when the device uses
    /// ids) into pointer motion, if it carries any.
    pub fn decode(&mut self, report: &[u8], now: u64) -> Option<Motion> {
        let (id, data) = if self.layout.report_ids {
            (*report.first()?, report.get(1..)?)
        } else {
            (0, report)
        };
        if let (Some(x), Some(y)) = (
            self.layout.find(id, PAGE_DESKTOP, USAGE_X, 0, true),
            self.layout.find(id, PAGE_DESKTOP, USAGE_Y, 0, true),
        ) {
            let dx = extract(data, &x).clamp(-2047, 2047) as i16;
            let dy = extract(data, &y).clamp(-2047, 2047) as i16;
            let mut buttons = 0;
            for button in 0..3 {
                if let Some(field) = self.layout.find(id, PAGE_BUTTON, 1 + button, 0, false) {
                    buttons |= ((extract(data, &field) != 0) as u8) << button;
                }
            }
            return Some(Motion { dx, dy, buttons });
        }
        self.touchpad(id, data, now)
    }

    fn touchpad(&mut self, id: u8, data: &[u8], now: u64) -> Option<Motion> {
        // Contacts of this report: (tip, contact id, x, y).
        let mut contacts = [(false, 0i64, 0i64, 0i64); 6];
        let mut any = false;
        let (mut x_range, mut y_range) = (1i64, 1i64);
        for finger in 1..=contacts.len() as u8 {
            let Some(tip) = self
                .layout
                .find(id, PAGE_DIGITIZER, USAGE_TIP, finger, false)
            else {
                continue;
            };
            let x = self.layout.find(id, PAGE_DESKTOP, USAGE_X, finger, false)?;
            let y = self.layout.find(id, PAGE_DESKTOP, USAGE_Y, finger, false)?;
            x_range = (x.max as i64 - x.min as i64).max(1);
            y_range = (y.max as i64 - y.min as i64).max(1);
            let contact = self
                .layout
                .find(id, PAGE_DIGITIZER, USAGE_CONTACT_ID, finger, false)
                .map_or(finger as i64, |field| extract(data, &field) as i64);
            contacts[finger as usize - 1] = (
                extract(data, &tip) != 0,
                contact,
                extract(data, &x) as i64,
                extract(data, &y) as i64,
            );
            any = true;
        }
        if !any {
            return None;
        }
        let click = self
            .layout
            .find(id, PAGE_BUTTON, 1, 0, false)
            .is_some_and(|field| extract(data, &field) != 0);
        let mut motion = Motion {
            dx: 0,
            dy: 0,
            buttons: click as u8,
        };
        let touching = |contact: &&(bool, i64, i64, i64)| contact.0;
        let following = self
            .track
            .and_then(|track| contacts.iter().filter(touching).find(|c| c.1 == track.id));
        match (following, contacts.iter().find(touching)) {
            (Some(&(_, _, x, y)), _) => {
                let mut track = self.track.unwrap();
                let (ux, uy) = (x - track.x, y - track.y);
                track.moved += ux.abs() + uy.abs();
                track.carry_x += ux * SWIPE_X;
                track.carry_y += uy * SWIPE_Y;
                let (dx, dy) = (track.carry_x / x_range, track.carry_y / y_range);
                track.carry_x -= dx * x_range;
                track.carry_y -= dy * y_range;
                track.x = x;
                track.y = y;
                self.track = Some(track);
                motion.dx = dx.clamp(-2047, 2047) as i16;
                motion.dy = dy.clamp(-2047, 2047) as i16;
            }
            (None, Some(&(_, contact, x, y))) => {
                // A new finger: start following it, no jump.
                self.track = Some(Track {
                    id: contact,
                    x,
                    y,
                    carry_x: 0,
                    carry_y: 0,
                    start: now,
                    moved: 0,
                });
            }
            (None, None) => {
                if let Some(track) = self.track.take() {
                    // Finger lifted: a short, still touch is a click.
                    if now.saturating_sub(track.start) <= TAP_TICKS && track.moved <= x_range / 40 {
                        self.release_tap = true;
                        motion.buttons |= 1;
                    }
                }
            }
        }
        let changed = motion.dx != 0 || motion.dy != 0 || motion.buttons != self.buttons;
        self.buttons = motion.buttons;
        changed.then_some(motion)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::vec::Vec;

    /// The standard boot-protocol mouse: 3 buttons, 8-bit relative X and Y.
    pub(crate) const BOOT_MOUSE: [u8; 50] = [
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
        0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
        0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95,
        0x02, 0x81, 0x06, 0xc0, 0xc0,
    ];

    fn finger() -> Vec<u8> {
        std::vec![
            0x05, 0x0d, 0x09, 0x22, 0xa1, 0x02, // finger collection
            0x15, 0x00, 0x25, 0x01, 0x09, 0x42, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, // tip
            0x95, 0x07, 0x81, 0x03, // padding
            0x75, 0x08, 0x09, 0x51, 0x95, 0x01, 0x26, 0xff, 0x00, 0x81, 0x02, // contact id
            0x05, 0x01, 0x75, 0x10, 0x95, 0x01, 0x26, 0xb0, 0x04, 0x09, 0x30, 0x81,
            0x02, // X 0..1200
            0x26, 0x20, 0x03, 0x09, 0x31, 0x81, 0x02, // Y 0..800
            0xc0,
        ]
    }

    /// Windows Precision Touchpad style: report 1 = two fingers and a click button.
    pub(crate) fn touchpad_descriptor() -> Vec<u8> {
        let mut bytes = std::vec![0x05, 0x0d, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x01];
        bytes.extend(finger());
        bytes.extend(finger());
        bytes.extend([
            0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02,
            0x95, 0x07, 0x81, 0x03, 0xc0,
        ]);
        bytes
    }

    pub(crate) fn report(f1: Option<(u8, u16, u16)>, click: bool) -> [u8; 14] {
        let mut bytes = [0u8; 14];
        bytes[0] = 1;
        if let Some((id, x, y)) = f1 {
            bytes[1] = 1;
            bytes[2] = id;
            bytes[3..5].copy_from_slice(&x.to_le_bytes());
            bytes[5..7].copy_from_slice(&y.to_le_bytes());
        }
        bytes[13] = click as u8;
        bytes
    }

    #[test]
    fn a_boot_mouse_descriptor_becomes_relative_motion_and_buttons() {
        let layout = Layout::parse(&BOOT_MOUSE);
        assert!(layout.is_mouse() && !layout.is_touchpad() && !layout.report_ids);
        let mut decoder = Decoder::new(layout);
        assert_eq!(
            decoder.decode(&[0b101, 5, 0xfd], 0),
            Some(Motion {
                dx: 5,
                dy: -3,
                buttons: 0b101
            })
        );
        assert_eq!(
            decoder.decode(&[0, 0, 0], 0),
            Some(Motion {
                dx: 0,
                dy: 0,
                buttons: 0
            })
        );
    }

    #[test]
    fn the_touchpad_descriptor_has_two_fingers_with_positions_and_ranges() {
        let layout = Layout::parse(&touchpad_descriptor());
        assert!(layout.is_touchpad() && !layout.is_mouse() && layout.report_ids);
        let x: Vec<_> = layout
            .fields()
            .iter()
            .filter(|f| f.page == 1 && f.usage == 0x30)
            .collect();
        assert_eq!(x.len(), 2);
        assert_eq!((x[0].finger, x[1].finger), (1, 2));
        assert_eq!((x[0].bit, x[0].size, x[0].max), (16, 16, 1200));
        assert_eq!(x[1].bit, 16 + 48);
    }

    #[test]
    fn a_finger_drag_moves_the_pointer_without_a_jump_at_touch_down() {
        let mut decoder = Decoder::new(Layout::parse(&touchpad_descriptor()));
        assert_eq!(decoder.decode(&report(Some((3, 600, 400)), false), 0), None);
        let motion = decoder
            .decode(&report(Some((3, 630, 390)), false), 1)
            .unwrap();
        // 30 of 1200 units across, scaled to a 1400-pixel swipe; 10 of 800 down.
        assert_eq!((motion.dx, motion.dy, motion.buttons), (35, -11, 0));
        // No spurious motion when nothing changed.
        assert_eq!(decoder.decode(&report(Some((3, 630, 390)), false), 2), None);
    }

    #[test]
    fn a_new_contact_id_starts_a_new_track_and_the_click_button_is_the_left_button() {
        let mut decoder = Decoder::new(Layout::parse(&touchpad_descriptor()));
        decoder.decode(&report(Some((1, 100, 100)), false), 0);
        decoder.decode(&report(Some((1, 200, 100)), false), 1);
        // The finger id changes: follow the new one from where it lands.
        assert_eq!(decoder.decode(&report(Some((9, 900, 700)), false), 2), None);
        let pressed = decoder
            .decode(&report(Some((9, 900, 700)), true), 3)
            .unwrap();
        assert_eq!((pressed.dx, pressed.dy, pressed.buttons), (0, 0, 1));
        let released = decoder
            .decode(&report(Some((9, 900, 700)), false), 4)
            .unwrap();
        assert_eq!(released.buttons, 0);
    }

    #[test]
    fn a_short_still_touch_is_a_tap_click_and_a_swipe_is_not() {
        let mut decoder = Decoder::new(Layout::parse(&touchpad_descriptor()));
        decoder.decode(&report(Some((1, 500, 500)), false), 100);
        decoder.decode(&report(Some((1, 501, 500)), false), 105);
        let tap = decoder.decode(&report(None, false), 108).unwrap();
        assert_eq!(tap.buttons, 1);
        assert_eq!(decoder.idle().unwrap().buttons, 0);
        assert_eq!(decoder.idle(), None);
        // A long swipe ends without a click.
        decoder.decode(&report(Some((2, 100, 500)), false), 200);
        decoder.decode(&report(Some((2, 600, 500)), false), 203);
        assert_eq!(decoder.decode(&report(None, false), 206), None);
        assert_eq!(decoder.idle(), None);
        // A touch held too long is not a tap either.
        decoder.decode(&report(Some((3, 500, 500)), false), 300);
        assert_eq!(decoder.decode(&report(None, false), 400), None);
    }

    #[test]
    fn extraction_handles_signed_unaligned_and_truncated_fields() {
        let field = |bit, size, min| Field {
            bit,
            size,
            min,
            ..Field::default()
        };
        assert_eq!(extract(&[0b1110_0000], &field(5, 3, -4)), -1);
        assert_eq!(extract(&[0b1110_0000], &field(5, 3, 0)), 7);
        assert_eq!(extract(&[0x34, 0x12], &field(0, 16, 0)), 0x1234);
        assert_eq!(extract(&[0x34], &field(0, 16, 0)), 0);
    }

    #[test]
    fn hostile_descriptors_never_panic_or_overflow_the_field_table() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as u8
        };
        for length in 0..300 {
            let bytes: Vec<u8> = (0..length).map(|_| next()).collect();
            let layout = Layout::parse(&bytes);
            assert!(layout.fields().len() <= MAX_FIELDS);
            let mut decoder = Decoder::new(layout);
            let report: Vec<u8> = (0..20).map(|_| next()).collect();
            decoder.decode(&report, 0);
            decoder.decode(&[], 1);
        }
        // A huge report count must not allocate or loop for long.
        let layout = Layout::parse(&[0x05, 0x01, 0x75, 0x08, 0x95, 0xff, 0x81, 0x06]);
        assert!(layout.fields().len() <= MAX_FIELDS);
    }
}
