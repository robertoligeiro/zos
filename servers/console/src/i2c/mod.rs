//! Built-in touchpads: HID over I2C behind an Intel serial-IO (DesignWare) controller.
//!
//! Most current laptops wire the touchpad to an I2C bus instead of PS/2 or USB. This
//! module is the whole stack, all of it hardware-independent so it is unit tested
//! against a simulated controller and touchpad:
//!
//! * `designware` – the I2C master (register level, polled, no interrupts, no DMA);
//! * `hid` – the HID-over-I2C protocol (descriptor, reset, power, input reports);
//! * `report` – the HID report-descriptor parser and the touchpad/mouse decoder;
//! * `touchpad` – probing the controllers and polling the device.

pub mod designware;
pub mod hid;
pub mod report;
pub mod touchpad;

#[cfg(test)]
mod simulation;

use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The register window is not a DesignWare I2C controller.
    NotDesignWare,
    /// The device did not acknowledge its address (or a data byte).
    Nack,
    /// Another master won the bus, or the controller reported another abort.
    Abort(u32),
    Timeout,
    /// The device answered, but not as a HID-over-I2C device would.
    Protocol,
    /// A transfer larger than the driver's buffers.
    TooLarge,
}

pub type Result<T> = core::result::Result<T, Error>;

/// Register access plus a clock, so the driver runs against real MMIO or a simulation.
pub trait Regs {
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
    /// A monotonic counter in 10 ms units (the kernel timer tick).
    fn ticks(&self) -> u64;
}

/// One I2C master: write `write` to `address`, then (after a repeated start) read
/// `read.len()` bytes. Either part may be empty, not both.
pub trait Bus {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> Result<()>;
    fn ticks(&self) -> u64;
}

/// A short log line that truncates instead of failing (debug lines are 64 bytes).
pub struct Line {
    bytes: [u8; 60],
    length: usize,
}

impl Line {
    pub fn new() -> Self {
        Self {
            bytes: [0; 60],
            length: 0,
        }
    }
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("?")
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for &byte in text.as_bytes() {
            if self.length < self.bytes.len() {
                self.bytes[self.length] = if byte.is_ascii_graphic() || byte == b' ' {
                    byte
                } else {
                    b'?'
                };
                self.length += 1;
            }
        }
        Ok(())
    }
}

/// Formats into a `Line` and passes it to the logger.
pub fn say(log: &mut dyn FnMut(&str), args: fmt::Arguments<'_>) {
    use fmt::Write;
    let mut line = Line::new();
    let _ = line.write_fmt(args);
    log(line.as_str());
}
