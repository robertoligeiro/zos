//! Finds a HID-over-I2C pointing device on an I2C controller and polls it.
//!
//! Real systems learn the device's address from ACPI, which ZOS does not read yet, so
//! the bus is scanned instead: every address that answers a one-byte read is logged
//! (for diagnosis). Addresses where touchpads are known to live, and any other address
//! outside the ranges used by memory and power-management chips, are asked for a HID
//! descriptor (a two-byte register write plus a read). The reserved ranges are never
//! written to.

use super::designware::DesignWare;
use super::hid::{HidDevice, MAX_INPUT, MAX_REPORT_DESCRIPTOR};
use super::report::{Decoder, Layout, Motion};
use super::{say, Bus, Error, Line, Regs};

/// 7-bit addresses used by touchpads and touchscreens (Elan, Synaptics, ALPS, Goodix,
/// Cirque, Hynitron, Microsoft) in Linux's device tables.
const TOUCHPAD_ADDRESSES: [u8; 13] = [
    0x10, 0x14, 0x15, 0x1a, 0x20, 0x24, 0x2a, 0x2c, 0x2d, 0x38, 0x3a, 0x4a, 0x5d,
];
/// Addresses that are never written to: reserved ranges, SPD/EEPROM memory and
/// smart-battery / power-management chips, where a stray write could do harm.
fn off_limits(address: u8) -> bool {
    matches!(address, 0x08..=0x0f | 0x30..=0x37 | 0x50..=0x57)
}

/// Consecutive failed polls (errors, not "nothing to say") before the device is
/// declared gone.
const MAX_FAILURES: u32 = 300;
/// How many raw reports are copied to the log.
const SAMPLES: u32 = 6;

pub struct Touchpad<B: Bus> {
    bus: B,
    device: HidDevice,
    decoder: Decoder,
    failures: u32,
    seen: u32,
    note: Option<Line>,
}

impl<B: Bus> Touchpad<B> {
    /// Polls once (call on every timer tick). Returns pointer motion if there is any.
    pub fn poll(&mut self) -> Option<Motion> {
        if self.dead() {
            return None;
        }
        let mut buffer = [0u8; MAX_INPUT];
        let now = self.bus.ticks();
        match self.device.poll(&mut self.bus, &mut buffer) {
            Ok(Some(report)) => {
                self.failures = 0;
                self.seen += 1;
                if self.seen <= SAMPLES {
                    // The first few raw reports go to the log: what a real device
                    // sends is the one thing a simulation cannot tell us.
                    use core::fmt::Write;
                    let mut line = Line::new();
                    let _ = write!(line, "I2C pad #{} len={}", self.seen, report.len());
                    for byte in report.iter().take(13) {
                        let _ = write!(line, " {byte:02x}");
                    }
                    self.note = Some(line);
                }
                self.decoder.decode(report, now)
            }
            Ok(None) => {
                self.failures = 0;
                self.decoder.idle()
            }
            // No acknowledge: many devices answer a read with nothing to send this
            // way, so it is not counted as a failure.
            Err(Error::Nack) => self.decoder.idle(),
            Err(_) => {
                self.failures += 1;
                self.decoder.idle()
            }
        }
    }

    /// A log line about one of the first reports, once, if there is one.
    pub fn take_note(&mut self) -> Option<Line> {
        self.note.take()
    }

    pub fn dead(&self) -> bool {
        self.failures >= MAX_FAILURES
    }

    pub fn address(&self) -> u8 {
        self.device.address
    }
}

/// Looks for a pointing device on `bus` (controller number `index`, for the log).
pub fn probe_bus<B: Bus>(
    index: usize,
    mut bus: B,
    log: &mut dyn FnMut(&str),
) -> Option<Touchpad<B>> {
    // 1. Who answers? (read-only probing, like `i2cdetect -r`)
    let mut answering = [0u8; 16];
    let mut count = 0;
    let mut timeouts = 0;
    for address in 0x08..=0x77u8 {
        let mut byte = [0u8; 1];
        match bus.transfer(address, &[], &mut byte) {
            Ok(()) => {
                if count < answering.len() {
                    answering[count] = address;
                }
                count += 1;
            }
            Err(Error::Timeout) => {
                timeouts += 1;
                if timeouts >= 3 {
                    say(log, format_args!("I2C{index}: bus not responding, skipped"));
                    return None;
                }
            }
            Err(_) => (),
        }
    }
    let mut line = super::Line::new();
    {
        use core::fmt::Write;
        let _ = write!(line, "I2C{index}: {count} devices:");
        for &address in &answering[..count.min(answering.len())] {
            let _ = write!(line, " {address:02x}");
        }
    }
    log(line.as_str());
    // 2. Ask the likely ones for a HID descriptor.
    for &address in &answering[..count.min(answering.len())] {
        if off_limits(address) && !TOUCHPAD_ADDRESSES.contains(&address) {
            continue;
        }
        let device = match HidDevice::identify(&mut bus, address) {
            Ok(device) => device,
            Err(error) => {
                say(
                    log,
                    format_args!("I2C{index}: {address:02x} not HID ({error:?})"),
                );
                continue;
            }
        };
        let descriptor = device.descriptor;
        say(
            log,
            format_args!(
                "I2C{index}: HID {address:02x} id={:04x}:{:04x} desc@{:04x} in={}",
                descriptor.vendor,
                descriptor.product,
                device.descriptor_register,
                descriptor.max_input_length
            ),
        );
        let mut bytes = [0u8; MAX_REPORT_DESCRIPTOR];
        let length = match device.report_descriptor(&mut bus, &mut bytes) {
            Ok(length) => length,
            Err(error) => {
                say(
                    log,
                    format_args!("I2C{index}: report descriptor failed {error:?}"),
                );
                continue;
            }
        };
        let layout = Layout::parse(&bytes[..length]);
        let (mouse, pad) = (layout.is_mouse(), layout.is_touchpad());
        say(
            log,
            format_args!(
                "I2C{index}: report {length}B fields={} mouse={mouse} pad={pad}",
                layout.fields().len()
            ),
        );
        if !mouse && !pad {
            continue;
        }
        if let Err(error) = device.start(&mut bus) {
            say(log, format_args!("I2C{index}: start failed {error:?}"));
            continue;
        }
        say(
            log,
            format_args!("I2C{index}: touchpad {address:02x} ready"),
        );
        return Some(Touchpad {
            bus,
            device,
            decoder: Decoder::new(layout),
            failures: 0,
            seen: 0,
            note: None,
        });
    }
    None
}

/// Opens controller `index` (register window `regs`) and looks for a touchpad on it.
pub fn probe<R: Regs>(
    index: usize,
    mut regs: R,
    log: &mut dyn FnMut(&str),
) -> Option<Touchpad<DesignWare<R>>> {
    // The component-type register is the signature; what a rejected window returns
    // tells whether it is unpowered/unrouted (all ones) or some other device. The
    // Intel reset register must be released first, or it reads as zero.
    regs.write(0x204, 3);
    let signature = regs.read(0xfc);
    if signature != 0x4457_0140 {
        say(
            log,
            format_args!("I2C{index}: id register reads {signature:#010x}"),
        );
    }
    match DesignWare::new(regs) {
        Ok((controller, _, timing)) => {
            say(
                log,
                format_args!(
                    "I2C{index}: DesignWare fifo={} fs/ss={timing:#x}",
                    controller.tx_depth()
                ),
            );
            probe_bus(index, controller, log)
        }
        Err(error) => {
            say(
                log,
                format_args!("I2C{index}: not a DesignWare I2C ({error:?})"),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::report::tests::{report, touchpad_descriptor, BOOT_MOUSE};
    use super::super::simulation::{Device, HidTouchpad, Plain, Simulation};
    use super::*;
    use std::boxed::Box;
    use std::string::{String, ToString};
    use std::vec;
    use std::vec::Vec;

    fn probe_with(
        devices: Vec<Box<dyn Device>>,
        log: &mut Vec<String>,
    ) -> Option<Touchpad<DesignWare<Simulation>>> {
        probe(0, Simulation::new(devices), &mut |line| {
            log.push(line.to_string())
        })
    }

    #[test]
    fn a_touchpad_among_other_devices_is_found_started_and_polled() {
        let mut pad = HidTouchpad::new(0x2c, touchpad_descriptor());
        // The address scan reads one byte from every device that answers, which may
        // consume a pending report (harmless: a finger-move report is repeated).
        pad.reports.push_back(Vec::new());
        for report in [
            report(Some((1, 600, 400)), false),
            report(Some((1, 640, 380)), false),
            report(Some((1, 640, 380)), true),
        ] {
            pad.reports.push_back(report.to_vec());
        }
        let mut log = Vec::new();
        let devices: Vec<Box<dyn Device>> =
            vec![Box::new(Plain(0x50)), Box::new(pad), Box::new(Plain(0x0b))];
        let mut touchpad = probe_with(devices, &mut log).expect("touchpad found");
        assert_eq!(touchpad.address(), 0x2c);
        let text = log.join("\n");
        assert!(text.contains("3 devices: 0b 2c 50"), "{text}");
        assert!(text.contains("HID 2c id=04f3:1234"), "{text}");
        assert!(text.contains("pad=true"), "{text}");
        assert!(text.contains("touchpad 2c ready"), "{text}");
        assert!(
            log.iter().all(|line| line.len() <= 60),
            "debug lines must fit 64 bytes"
        );
        // First report: finger down (no motion); then a drag; then a click.
        assert_eq!(touchpad.poll(), None);
        let moved = touchpad.poll().unwrap();
        assert_eq!((moved.dx, moved.dy), (46, -22));
        let clicked = touchpad.poll().unwrap();
        assert_eq!(clicked.buttons, 1);
        // Nothing queued: silence, and the device is still alive.
        assert_eq!(touchpad.poll(), None);
        assert!(!touchpad.dead());
    }

    #[test]
    fn the_device_is_powered_on_then_reset_before_use() {
        let pad = HidTouchpad::new(0x15, BOOT_MOUSE.to_vec());
        let commands = pad.commands.clone();
        let mut log = Vec::new();
        let touchpad = probe_with(vec![Box::new(pad)], &mut log).expect("mouse-mode pad found");
        assert_eq!(touchpad.address(), 0x15);
        // SET_POWER(on) = opcode 8 with state 0, then RESET = opcode 1.
        assert_eq!(*commands.borrow(), vec![[0, 8], [0, 1]]);
    }

    #[test]
    fn a_mouse_mode_pad_reports_relative_motion() {
        let mut pad = HidTouchpad::new(0x15, BOOT_MOUSE.to_vec());
        pad.reports.push_back(Vec::new()); // consumed by the address scan
        pad.reports.push_back(vec![0b001, 7, 0xfb]);
        let mut log = Vec::new();
        let mut touchpad = probe_with(vec![Box::new(pad)], &mut log).unwrap();
        let motion = touchpad.poll().unwrap();
        assert_eq!((motion.dx, motion.dy, motion.buttons), (7, -5, 1));
    }

    #[test]
    fn the_alternate_descriptor_register_is_tried() {
        let mut pad = HidTouchpad::new(0x2c, BOOT_MOUSE.to_vec());
        pad.descriptor_register = 0x0020;
        let mut log = Vec::new();
        assert!(probe_with(vec![Box::new(pad)], &mut log).is_some());
    }

    #[test]
    fn no_touchpad_means_none_and_a_useful_log() {
        let mut log = Vec::new();
        assert!(probe_with(vec![Box::new(Plain(0x50)), Box::new(Plain(0x2c))], &mut log).is_none());
        let text = log.join("\n");
        assert!(text.contains("2 devices: 2c 50"), "{text}");
        assert!(text.contains("2c not HID"), "{text}");
        assert!(
            !text.contains("50 not HID"),
            "memory-chip addresses are left alone: {text}"
        );
    }

    #[test]
    fn memory_and_power_chip_addresses_are_never_written_to() {
        // A HID device at an EEPROM / battery address is not even asked.
        let pad = HidTouchpad::new(0x50, BOOT_MOUSE.to_vec());
        let mut log = Vec::new();
        assert!(probe_with(vec![Box::new(pad)], &mut log).is_none());
        assert!(!log.join("\n").contains("HID 50"));
    }

    #[test]
    fn a_non_pointer_hid_device_is_skipped() {
        // A keyboard-like descriptor (usage page 1, usage 6) has neither mouse nor pad.
        let keyboard = vec![0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0xc0];
        let pad = HidTouchpad::new(0x2c, keyboard);
        let mut log = Vec::new();
        assert!(probe_with(vec![Box::new(pad)], &mut log).is_none());
        assert!(log.join("\n").contains("mouse=false pad=false"));
    }

    #[test]
    fn a_device_that_stops_answering_is_eventually_declared_dead() {
        // Simulate unplugging by probing a good device, then polling a bus whose
        // device vanished: use a bus wrapper that always NACKs.
        struct Silent(u64);
        impl Bus for Silent {
            fn transfer(&mut self, _: u8, _: &[u8], _: &mut [u8]) -> super::super::Result<()> {
                Err(Error::Timeout)
            }
            fn ticks(&self) -> u64 {
                self.0
            }
        }
        let pad = HidTouchpad::new(0x2c, BOOT_MOUSE.to_vec());
        let mut log = Vec::new();
        let touchpad = probe_with(vec![Box::new(pad)], &mut log).unwrap();
        let mut dead = Touchpad {
            bus: Silent(0),
            device: touchpad.device,
            decoder: touchpad.decoder,
            failures: 0,
            seen: 0,
            note: None,
        };
        for _ in 0..MAX_FAILURES {
            assert!(!dead.dead());
            assert_eq!(dead.poll(), None);
        }
        assert!(dead.dead());
        assert_eq!(dead.poll(), None);
    }

    #[test]
    fn a_bus_that_hangs_is_abandoned_quickly() {
        struct Hung;
        impl Bus for Hung {
            fn transfer(&mut self, _: u8, _: &[u8], _: &mut [u8]) -> super::super::Result<()> {
                Err(Error::Timeout)
            }
            fn ticks(&self) -> u64 {
                0
            }
        }
        let mut log = Vec::new();
        assert!(probe_bus(1, Hung, &mut |line| log.push(line.to_string())).is_none());
        assert_eq!(log, ["I2C1: bus not responding, skipped"]);
    }
}

#[cfg(test)]
mod range_tests {
    use super::off_limits;

    #[test]
    fn only_memory_and_power_ranges_are_off_limits() {
        for address in [0x08, 0x0b, 0x0f, 0x30, 0x37, 0x50, 0x57] {
            assert!(off_limits(address), "{address:#x}");
        }
        for address in [0x10, 0x15, 0x2c, 0x38, 0x4a, 0x58, 0x5d, 0x77] {
            assert!(!off_limits(address), "{address:#x}");
        }
    }
}

#[cfg(test)]
mod note_tests {
    use super::super::report::tests::{report, touchpad_descriptor};
    use super::super::simulation::{Device, HidTouchpad, Simulation};
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    #[test]
    fn the_first_reports_are_noted_for_the_log_and_nacks_never_kill_the_device() {
        let mut pad = HidTouchpad::new(0x15, touchpad_descriptor());
        pad.reports.push_back(Vec::new()); // eaten by the address scan
        for _ in 0..8 {
            pad.reports
                .push_back(report(Some((1, 600, 400)), false).to_vec());
        }
        let devices: Vec<Box<dyn Device>> = std::vec![Box::new(pad)];
        let mut touchpad = probe(0, Simulation::new(devices), &mut |_| ()).unwrap();
        let mut notes = Vec::new();
        for _ in 0..10 {
            touchpad.poll();
            if let Some(note) = touchpad.take_note() {
                notes.push(std::string::String::from(note.as_str()));
            }
        }
        assert_eq!(notes.len(), SAMPLES as usize);
        assert!(
            notes[0].starts_with("I2C pad #1 len=14 01 01 01"),
            "{}",
            notes[0]
        );
        assert!(notes[0].len() <= 60);
        assert!(!touchpad.dead());
    }
}
