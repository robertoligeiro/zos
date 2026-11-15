//! HID over I2C (Microsoft's protocol, version 1.00): descriptor, reset, power and
//! polled input reports. Register addresses are 16 bits, little endian.

use super::{Bus, Error, Result};

pub const DESCRIPTOR_LENGTH: usize = 30;
/// Where the HID descriptor's address usually lives (it comes from ACPI on real
/// systems, which ZOS does not read; these two cover nearly every touchpad).
const DESCRIPTOR_REGISTERS: [u16; 2] = [0x0001, 0x0020];
pub const MAX_REPORT_DESCRIPTOR: usize = 1024;
pub const MAX_INPUT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub report_descriptor_length: u16,
    pub report_descriptor_register: u16,
    pub input_register: u16,
    pub max_input_length: u16,
    pub command_register: u16,
    pub vendor: u16,
    pub product: u16,
}

fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

impl Descriptor {
    pub fn parse(bytes: &[u8; DESCRIPTOR_LENGTH]) -> Result<Self> {
        let descriptor = Self {
            report_descriptor_length: word(bytes, 4),
            report_descriptor_register: word(bytes, 6),
            input_register: word(bytes, 8),
            max_input_length: word(bytes, 10),
            command_register: word(bytes, 16),
            vendor: word(bytes, 20),
            product: word(bytes, 22),
        };
        if word(bytes, 0) as usize != DESCRIPTOR_LENGTH
            || word(bytes, 2) != 0x0100
            || descriptor.report_descriptor_length == 0
            || descriptor.report_descriptor_length as usize > MAX_REPORT_DESCRIPTOR
            || !(3..=MAX_INPUT as u16).contains(&descriptor.max_input_length)
        {
            return Err(Error::Protocol);
        }
        Ok(descriptor)
    }
}

pub struct HidDevice {
    pub address: u8,
    pub descriptor: Descriptor,
    pub descriptor_register: u16,
}

fn register(bus: &mut impl Bus, address: u8, register: u16, out: &mut [u8]) -> Result<()> {
    bus.transfer(address, &register.to_le_bytes(), out)
}

impl HidDevice {
    /// Asks the device at `address` for its HID descriptor; `Err` for anything that is
    /// not a HID-over-I2C device (including a device that does not answer).
    pub fn identify(bus: &mut impl Bus, address: u8) -> Result<Self> {
        let mut last = Error::Protocol;
        for &descriptor_register in &DESCRIPTOR_REGISTERS {
            let mut bytes = [0u8; DESCRIPTOR_LENGTH];
            match register(bus, address, descriptor_register, &mut bytes) {
                Ok(()) => match Descriptor::parse(&bytes) {
                    Ok(descriptor) => {
                        return Ok(Self {
                            address,
                            descriptor,
                            descriptor_register,
                        })
                    }
                    Err(error) => last = error,
                },
                // A device that does not acknowledge will not at the other register.
                Err(Error::Nack) => return Err(Error::Nack),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    /// Reads the report descriptor in pieces; returns its length.
    pub fn report_descriptor(
        &self,
        bus: &mut impl Bus,
        out: &mut [u8; MAX_REPORT_DESCRIPTOR],
    ) -> Result<usize> {
        let length = self.descriptor.report_descriptor_length as usize;
        let request = self.descriptor.report_descriptor_register.to_le_bytes();
        // One transaction: the register write, then the whole descriptor.
        bus.transfer(self.address, &request, &mut out[..length])?;
        Ok(length)
    }

    fn command(&self, bus: &mut impl Bus, low: u8, opcode: u8) -> Result<()> {
        let register = self.descriptor.command_register.to_le_bytes();
        bus.transfer(
            self.address,
            &[register[0], register[1], low, opcode],
            &mut [],
        )
    }

    /// SET_POWER(on) then RESET, then drains the reset acknowledgement.
    pub fn start(&self, bus: &mut impl Bus) -> Result<()> {
        self.command(bus, 0x00, 0x08)?;
        self.command(bus, 0x00, 0x01)?;
        let until = bus.ticks() + 12;
        while bus.ticks() < until {
            core::hint::spin_loop();
        }
        // The device answers a reset with a zero-length report; consume it.
        let mut length = [0u8; 2];
        let _ = bus.transfer(self.address, &[], &mut length);
        Ok(())
    }

    /// Reads one input report. `Ok(None)` when the device has nothing to say (a zero
    /// length). The report is returned without its two length bytes.
    pub fn poll<'a>(
        &self,
        bus: &mut impl Bus,
        buffer: &'a mut [u8; MAX_INPUT],
    ) -> Result<Option<&'a [u8]>> {
        let size = self.descriptor.max_input_length as usize;
        bus.transfer(self.address, &[], &mut buffer[..size])?;
        let length = word(buffer, 0) as usize;
        if length == 0 || length == 0xffff {
            return Ok(None);
        }
        if length < 2 || length > size {
            return Err(Error::Protocol);
        }
        Ok(Some(&buffer[2..length]))
    }
}
