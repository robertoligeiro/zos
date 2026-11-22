use super::*;
use x86_64::instructions::port::Port;

/// Intel serial-IO (LPSS) I2C controllers: the input service reads and writes their
/// registers (polled, no DMA) to talk to a built-in HID touchpad.
const I2C_BASE: u64 = 0x2900_0000;
const I2C_STRIDE: u64 = 0x1_0000;
const I2C_MAX: usize = 16;

pub(super) struct Grant {
    pub allowed: bool,
    device: Option<u32>,
    pages: u64,
    started: bool,
    i2c: [Option<u32>; I2C_MAX],
}

impl Grant {
    pub const fn new(allowed: bool) -> Self {
        Self {
            allowed,
            device: None,
            pages: 0,
            started: false,
            i2c: [None; I2C_MAX],
        }
    }

    pub fn stop(&mut self) {
        if let Some(device) = self.device.take() {
            command(device, (read(device, 4) as u16 & !6) | 0x400);
        }
        for device in self.i2c.iter_mut().filter_map(Option::take) {
            command(device, (read(device, 4) as u16 & !6) | 0x400);
        }
    }
}

fn read(device: u32, offset: u32) -> u32 {
    // SAFETY: called only under the single-CPU scheduler lock with IRQs disabled;
    // the kernel exclusively owns PCI configuration mechanism #1.
    unsafe {
        Port::<u32>::new(0xcf8).write(0x8000_0000 | device | offset);
        Port::<u32>::new(0xcfc).read()
    }
}

fn write(device: u32, offset: u32, value: u32) {
    // SAFETY: same serialized PCI access; caller selected the USB controller.
    unsafe {
        Port::<u32>::new(0xcf8).write(0x8000_0000 | device | offset);
        Port::<u32>::new(0xcfc).write(value);
    }
}

fn command(device: u32, value: u16) {
    // SAFETY: write only command, not the adjacent W1C PCI status register.
    unsafe {
        Port::<u32>::new(0xcf8).write(0x8000_0000 | device | 4);
        Port::<u16>::new(0xcfc).write(value);
    }
}

fn discover() -> Result<(u32, u64, u64), Error> {
    let mut found = None;
    for bus in 0..256 {
        for slot in 0..32 {
            let device = bus << 16 | slot << 11;
            if read(device, 0) & 0xffff == 0xffff {
                continue;
            }
            let functions = if read(device, 12) & 0x800000 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let device = device | function << 8;
                if read(device, 0) & 0xffff == 0xffff || read(device, 8) >> 8 != 0x0c0330 {
                    continue;
                }
                let identity = read(device, 0);
                crate::logln!("PCI XHCI FOUND: bdf={:#x} id={:#010x}", device, identity);
                if found.is_none() {
                    found = Some(device);
                } else {
                    crate::logln!("PCI XHCI SKIP: additional controller; using first PCI match");
                }
            }
        }
    }
    let device = found.ok_or(Error::Invalid)?;
    let (physical, length) = bar(device, "XHCI", false)?;
    Ok((device, physical, length))
}

/// Calls `visit` with the address of every PCI function that exists.
fn functions(mut visit: impl FnMut(u32)) {
    for bus in 0..256 {
        for slot in 0..32 {
            let device = bus << 16 | slot << 11;
            if read(device, 0) & 0xffff == 0xffff {
                continue;
            }
            let count = if read(device, 12) & 0x800000 != 0 {
                8
            } else {
                1
            };
            for function in 0..count {
                let device = device | function << 8;
                if read(device, 0) & 0xffff != 0xffff {
                    visit(device);
                }
            }
        }
    }
}

/// Base of the memory BAR at `offset` (64-bit if its type says so); 0 for I/O or empty.
fn bar_base(device: u32, offset: u32) -> u64 {
    let value = read(device, offset);
    if value & 1 != 0 {
        return 0;
    }
    let high = if value & 6 == 4 && offset < 0x24 {
        read(device, offset + 4) as u64
    } else {
        0
    };
    high << 32 | (value & !15) as u64
}

/// The highest 64-bit memory BAR (at or above 4 GiB) any device already uses; 0 if none.
fn highest_high_bar() -> u64 {
    let mut highest = 0;
    functions(|device| {
        let bars = match read(device, 12) >> 16 & 0x7f {
            0 => 6,
            1 => 2,
            _ => 0,
        };
        let mut index = 0;
        while index < bars {
            let value = read(device, 16 + 4 * index);
            let wide = value & 1 == 0 && value & 6 == 4;
            let base = bar_base(device, 16 + 4 * index);
            if (1 << 32..1 << 44).contains(&base) {
                highest = highest.max(base);
            }
            index += if wide { 2 } else { 1 };
        }
    });
    highest
}

/// One line per PCI function for the boot log: where firmware put everything, which
/// is what is needed to place a device it left unassigned.
fn log_pci_map() {
    functions(|device| {
        let identity = read(device, 0);
        let class = read(device, 8) >> 8;
        let kind = read(device, 12) >> 16 & 0x7f;
        if kind == 1 {
            crate::logln!(
                "PCI {:02x}:{:02x}.{} {:04x}:{:04x} {:06x} bridge mem={:08x} pf={:08x}{:08x}",
                device >> 16,
                device >> 11 & 31,
                device >> 8 & 7,
                identity & 0xffff,
                identity >> 16,
                class,
                read(device, 0x20),
                read(device, 0x28),
                read(device, 0x24)
            );
        } else {
            crate::logln!(
                "PCI {:02x}:{:02x}.{} {:04x}:{:04x} {:06x} b0={:#x} b2={:#x}",
                device >> 16,
                device >> 11 & 31,
                device >> 8 & 7,
                identity & 0xffff,
                identity >> 16,
                class,
                bar_base(device, 16),
                bar_base(device, 24)
            );
        }
    });
}

/// Sizes and validates BAR0 of a type-0 device and leaves memory decoding disabled. With
/// `assign`, a 64-bit BAR the firmware left empty is given an address: 1 GiB above the
/// highest 64-bit BAR in use, in the high MMIO space the firmware reserved for devices.
fn bar(device: u32, label: &str, assign: bool) -> Result<(u64, u64), Error> {
    if read(device, 12) & 0x7f0000 != 0 {
        return Err(Error::Denied);
    }
    let saved = read(device, 4) as u16;
    command(device, (saved & !6) | 0x400);
    let low = read(device, 16);
    let wide = low & 6 == 4;
    if low & 1 != 0 || (!wide && low & 6 != 0) {
        command(device, saved);
        return Err(Error::Invalid);
    }
    let high = if wide { read(device, 20) } else { 0 };
    write(device, 16, u32::MAX);
    if wide {
        write(device, 20, u32::MAX);
    }
    let mask_low = read(device, 16) & !15;
    let mask_high = if wide { read(device, 20) } else { u32::MAX };
    write(device, 16, low);
    if wide {
        write(device, 20, high);
    }
    let mut physical = (high as u64) << 32 | (low & !15) as u64;
    let length = (!((mask_high as u64) << 32 | mask_low as u64)).wrapping_add(1);
    crate::logln!("PCI {} BAR: base={:#x} bytes={}", label, physical, length);
    let mut assigned = false;
    if physical == 0
        && assign
        && wide
        && length.is_power_of_two()
        && (PAGE_SIZE..=65536).contains(&length)
    {
        let highest = highest_high_bar();
        if highest != 0 {
            physical = (highest + (1 << 30) + 0xf_ffff) & !0xf_ffff;
            write(device, 16, (physical as u32 & !15) | (low & 15));
            write(device, 20, (physical >> 32) as u32);
            assigned = true;
            crate::logln!(
                "PCI {} BAR unassigned: placed at {:#x} (highest BAR {:#x})",
                label,
                physical,
                highest
            );
        }
    }
    let Some(end) = physical.checked_add(length) else {
        command(device, saved);
        return Err(Error::Invalid);
    };
    if physical == 0
        || physical % PAGE_SIZE != 0
        || !(PAGE_SIZE..=65536).contains(&length)
        || !length.is_power_of_two()
        || crate::boot::MEMORY_MAP
            .response()
            .unwrap()
            .entries()
            .iter()
            .any(|entry| {
                entry.type_ != limine::memmap::MEMMAP_RESERVED
                    && physical < entry.base.saturating_add(entry.length)
                    && entry.base < end
            })
    {
        if assigned {
            write(device, 16, low);
            write(device, 20, high);
        }
        command(device, saved);
        return Err(Error::Denied);
    }
    crate::logln!(
        "PCI {} PASS: bdf={:#x} BAR={:#x} bytes={}",
        label,
        device,
        physical,
        length
    );
    Ok((physical, length))
}

/// Moves a device out of PCI power state D3 (if its power-management capability says it
/// is there). Serial-IO controllers can be left powered down by firmware.
fn power_on(device: u32) {
    if read(device, 4) >> 16 & 0x10 == 0 {
        return; // no capability list
    }
    let mut pointer = read(device, 0x34) & 0xfc;
    for _ in 0..48 {
        if pointer < 0x40 {
            return;
        }
        let header = read(device, pointer);
        if header & 0xff == 1 {
            let control = read(device, pointer + 4);
            if control & 3 != 0 {
                crate::logln!("PCI I2C {:#x}: D{} -> D0", device, control & 3);
                // Bit 15 is write-1-to-clear PME status; leave it alone.
                write(device, pointer + 4, control & !0x8003);
            }
            return;
        }
        pointer = header >> 8 & 0xfc;
    }
}

/// The `index`th Intel serial-IO candidate (vendor 8086, class 0x118000 "signal
/// processing" as on Skylake-era chipsets, or 0x0c8000 "serial bus, other" as on
/// newer ones: I2C, SPI, UART and other blocks; the driver recognizes I2C by its
/// register signature and skips the rest). `Invalid`
/// past the last one, `Denied` when that one has no usable register window.
fn discover_i2c(index: usize) -> Result<(u32, u64, u64), Error> {
    if index == 0 {
        log_pci_map();
    }
    let mut seen = 0;
    for bus in 0..256 {
        for slot in 0..32 {
            let device = bus << 16 | slot << 11;
            if read(device, 0) & 0xffff == 0xffff {
                continue;
            }
            let functions = if read(device, 12) & 0x800000 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let device = device | function << 8;
                let identity = read(device, 0);
                let class = read(device, 8) >> 8;
                if identity & 0xffff != 0x8086 || !matches!(class, 0x118000 | 0x0c8000) {
                    continue;
                }
                if seen != index {
                    seen += 1;
                    continue;
                }
                crate::logln!(
                    "PCI I2C {}: bdf={:#x} id={:#010x} class={:#08x}",
                    index,
                    device,
                    identity,
                    class
                );
                power_on(device);
                let (physical, length) = bar(device, "I2C", true)?;
                return Ok((device, physical, length));
            }
        }
    }
    Err(Error::Invalid)
}

impl Scheduler {
    pub(super) fn hardware(&mut self, id: usize, frame: Frame) -> Result<Option<usize>, Error> {
        let mut process = self.threads[id].process.lock();
        if !process.usb.allowed {
            return Err(Error::Denied);
        }
        if frame.rax == Call::UsbTicks as u64 {
            return Ok(Some(
                crate::TICKS.load(core::sync::atomic::Ordering::Relaxed) as usize,
            ));
        }
        if frame.rax == Call::UsbEnableDma as u64 {
            let device = process.usb.device.ok_or(Error::Denied)?;
            if process.usb.started || process.usb.pages == 0 {
                return Err(Error::Denied);
            }
            process.usb.started = true;
            command(device, read(device, 4) as u16 | 6 | 0x400);
            return Ok(Some(0));
        }
        if frame.rsi != 16 {
            return Err(Error::Invalid);
        }
        if frame.rax == Call::I2cController as u64 && process.usb.device.is_none() {
            // Only after the USB controller is up: its driver (the input service) has
            // finished starting before any I2C probing begins.
            return Err(Error::Denied);
        }
        process
            .space
            .check(frame.rdi, 16, true)
            .map_err(|_| Error::BadAddress)?;
        let (first, second) = if frame.rax == Call::UsbController as u64 {
            if process.usb.device.is_some() {
                return Err(Error::Denied);
            }
            let (device, physical, length) = discover()?;
            process.usb.device = Some(device);
            let base = process
                .space
                .map_device(physical, length)
                .map_err(Self::hardware_memory_error)?;
            command(device, (read(device, 4) as u16 | 2 | 0x400) & !5);
            (base, length)
        } else if frame.rax == Call::I2cController as u64 {
            let index = frame.rdx as usize;
            if index >= I2C_MAX || process.usb.i2c[index].is_some() || crate::boot::no_i2c() {
                return Err(Error::Invalid);
            }
            let (device, physical, length) = discover_i2c(index)?;
            let base = process
                .space
                .map_device_at(I2C_BASE + index as u64 * I2C_STRIDE, physical, length)
                .map_err(Self::hardware_memory_error)?;
            process.usb.i2c[index] = Some(device);
            // Memory decoding on, bus mastering and legacy I/O off (the driver is polled).
            command(device, (read(device, 4) as u16 | 2 | 0x400) & !5);
            (base, length)
        } else {
            if process.usb.device.is_none() {
                return Err(Error::Denied);
            }
            if process.usb.pages == zos_libos::USB_DMA_PAGES {
                return Err(Error::Limit);
            }
            let base = 0x3000_0000 + process.usb.pages * PAGE_SIZE;
            let physical = process
                .space
                .map_dma_page(base)
                .map_err(Self::hardware_memory_error)?;
            process.usb.pages += 1;
            (base, physical)
        };
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&first.to_le_bytes());
        bytes[8..].copy_from_slice(&second.to_le_bytes());
        process
            .space
            .write_user(frame.rdi, &bytes)
            .map_err(|_| Error::BadAddress)?;
        Ok(Some(0))
    }

    fn hardware_memory_error(error: crate::memory::Error) -> Error {
        match error {
            crate::memory::Error::OutOfFrames => Error::NoMemory,
            _ => Error::Invalid,
        }
    }
}
