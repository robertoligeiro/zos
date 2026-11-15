//! Test-only model of a DesignWare I2C controller with devices on the bus, including a
//! HID-over-I2C touchpad. It follows the databook semantics the driver relies on:
//! DATA_CMD pushes build transactions, a missing device raises TX_ABRT (source bit 0),
//! disabling flushes the FIFOs, and the receive FIFO is finite.

use super::Regs;
use std::boxed::Box;
use std::collections::VecDeque;
use std::vec::Vec;

pub trait Device {
    fn address(&self) -> u8;
    /// A write transaction (everything before a repeated start or the stop).
    fn write(&mut self, bytes: &[u8]);
    /// The next byte of a read transaction.
    fn read(&mut self) -> u8;
    /// A read transaction begins (a new START with the read bit).
    fn begin_read(&mut self);
}

pub struct Simulation {
    pub devices: Vec<Box<dyn Device>>,
    registers: std::collections::HashMap<usize, u32>,
    pub writes_to_reset_register: u32,
    enabled: bool,
    target: u8,
    queue: VecDeque<u32>,
    pending_write: Vec<u8>,
    reading: bool,
    receive: VecDeque<u8>,
    abort_source: u32,
    raw: u32,
    calls: std::cell::Cell<u64>,
    pub receive_depth: usize,
    pub overflowed: bool,
    pub transactions: u32,
}

impl Simulation {
    pub fn new(devices: Vec<Box<dyn Device>>) -> Self {
        let mut registers = std::collections::HashMap::new();
        registers.insert(0xfc, 0x4457_0140);
        registers.insert(0xf4, 0x003f_0f00 | 15 << 16 | 31 << 8); // depth 32 tx / 32 rx
        Self {
            devices,
            registers,
            writes_to_reset_register: 0,
            enabled: false,
            target: 0,
            queue: VecDeque::new(),
            pending_write: Vec::new(),
            reading: false,
            receive: VecDeque::new(),
            abort_source: 0,
            raw: 0,
            calls: std::cell::Cell::new(0),
            receive_depth: 16,
            overflowed: false,
            transactions: 0,
        }
    }

    pub fn set(&mut self, offset: usize, value: u32) {
        self.registers.insert(offset, value);
    }

    fn device(&mut self, address: u8) -> Option<usize> {
        self.devices
            .iter()
            .position(|device| device.address() == address)
    }

    fn flush_write(&mut self) {
        if !self.pending_write.is_empty() {
            let bytes = core::mem::take(&mut self.pending_write);
            let target = self.target;
            if let Some(index) = self.device(target) {
                self.devices[index].write(&bytes);
            }
        }
    }

    fn abort(&mut self, source: u32) {
        self.queue.clear();
        self.pending_write.clear();
        self.abort_source |= source;
        self.reading = false;
        self.raw |= 1 << 6;
    }

    /// Runs every queued command, as the bus would between two register reads.
    fn engine(&mut self) {
        while let Some(command) = self.queue.pop_front() {
            let present = self.device(self.target);
            if present.is_none() {
                self.abort(1);
                return;
            }
            let index = present.unwrap();
            if command & 0x100 != 0 {
                if !self.reading {
                    self.flush_write();
                    self.devices[index].begin_read();
                    self.reading = true;
                }
                let byte = self.devices[index].read();
                if self.receive.len() >= self.receive_depth {
                    self.overflowed = true;
                } else {
                    self.receive.push_back(byte);
                }
            } else {
                self.pending_write.push(command as u8);
            }
            if command & 0x200 != 0 {
                self.flush_write();
                self.transactions += 1;
                self.reading = false;
            }
        }
    }
}

impl Regs for Simulation {
    fn read(&mut self, offset: usize) -> u32 {
        match offset {
            0x70 => {
                self.engine();
                let mut status = 1 << 2 | 1 << 1; // TX empty, TX not full
                if !self.receive.is_empty() {
                    status |= 1 << 3;
                }
                status
            }
            0x34 => {
                self.engine();
                self.raw
            }
            0x10 => self.receive.pop_front().unwrap_or(0) as u32,
            0x80 => self.abort_source,
            0x54 => {
                self.raw &= !(1 << 6);
                self.abort_source = 0;
                0
            }
            0x40 => {
                self.raw = 0;
                0
            }
            0x9c => self.enabled as u32,
            _ => self.registers.get(&offset).copied().unwrap_or(0),
        }
    }

    fn write(&mut self, offset: usize, value: u32) {
        match offset {
            0x6c => {
                self.enabled = value & 1 != 0;
                if !self.enabled {
                    self.queue.clear();
                    self.receive.clear();
                    self.pending_write.clear();
                    self.reading = false;
                }
            }
            0x04 => self.target = value as u8 & 0x7f,
            0x10 => {
                assert!(
                    self.enabled,
                    "DATA_CMD written while the controller is disabled"
                );
                self.queue.push_back(value);
            }
            0x204 => self.writes_to_reset_register += 1,
            _ => (),
        }
        self.registers.insert(offset, value);
    }

    fn ticks(&self) -> u64 {
        let calls = self.calls.get() + 1;
        self.calls.set(calls);
        calls / 25
    }
}

/// A plain device that acknowledges and returns a fixed byte pattern (not HID).
pub struct Plain(pub u8);
impl Device for Plain {
    fn address(&self) -> u8 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {}
    fn begin_read(&mut self) {}
    fn read(&mut self) -> u8 {
        0xa5
    }
}

/// A HID-over-I2C device: descriptor at register 0x0001, report descriptor, command
/// register and an input queue.
pub struct HidTouchpad {
    pub address: u8,
    pub report_descriptor: Vec<u8>,
    pub reports: VecDeque<Vec<u8>>,
    pub commands: std::rc::Rc<std::cell::RefCell<Vec<[u8; 2]>>>,
    pub max_input: u16,
    pub descriptor_register: u16,
    selected: Vec<u8>,
    current: Vec<u8>,
    position: usize,
}

impl HidTouchpad {
    pub fn new(address: u8, report_descriptor: Vec<u8>) -> Self {
        Self {
            address,
            report_descriptor,
            reports: VecDeque::new(),
            commands: Default::default(),
            max_input: 32,
            descriptor_register: 0x0001,
            selected: Vec::new(),
            current: Vec::new(),
            position: 0,
        }
    }

    fn descriptor(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for word in [
            30u16,
            0x0100,
            self.report_descriptor.len() as u16,
            0x0002, // report descriptor register
            0x0003, // input register
            self.max_input,
            0x0004, // output register
            0x0020,
            0x0005, // command register
            0x0006, // data register
            0x04f3, // vendor
            0x1234, // product
            0x0001,
        ] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend([0, 0, 0, 0]);
        bytes
    }
}

impl Device for HidTouchpad {
    fn address(&self) -> u8 {
        self.address
    }
    fn write(&mut self, bytes: &[u8]) {
        if bytes.len() == 2 {
            let register = u16::from_le_bytes([bytes[0], bytes[1]]);
            self.selected = if register == self.descriptor_register {
                self.descriptor()
            } else if register == 0x0002 {
                self.report_descriptor.clone()
            } else {
                Vec::new()
            };
        } else if bytes.len() == 4 && bytes[0] == 0x05 && bytes[1] == 0x00 {
            // Command register: remember (byte 0, opcode). A reset queues a zero-length
            // report, as real devices do.
            self.commands.borrow_mut().push([bytes[2], bytes[3]]);
            if bytes[3] == 1 {
                self.reports.push_front(Vec::new());
            }
        }
    }
    fn begin_read(&mut self) {
        self.position = 0;
        if !self.selected.is_empty() {
            // A register was just selected: this read returns that register.
            self.current = core::mem::take(&mut self.selected);
        } else {
            // A plain read is an input-report read.
            self.current = match self.reports.pop_front() {
                Some(report) if !report.is_empty() => {
                    let mut bytes = (report.len() as u16 + 2).to_le_bytes().to_vec();
                    bytes.extend(report);
                    bytes
                }
                _ => std::vec![0, 0],
            };
        }
    }
    fn read(&mut self) -> u8 {
        let byte = self.current.get(self.position).copied().unwrap_or(0);
        self.position += 1;
        byte
    }
}
