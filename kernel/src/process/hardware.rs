use super::*;
use x86_64::instructions::port::Port;

pub(super) struct Grant {
    pub allowed: bool,
    device: Option<u32>,
    pages: u64,
    started: bool,
}

impl Grant {
    pub const fn new(allowed: bool) -> Self {
        Self {
            allowed,
            device: None,
            pages: 0,
            started: false,
        }
    }

    pub fn stop(&mut self) {
        if let Some(device) = self.device.take() {
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
                if cfg!(feature = "usb-diagnostic")
                    && identity != 0x000d_1b36
                    && identity != 0x4ded_8086
                {
                    crate::logln!("PCI XHCI SKIP: unsupported controller");
                    continue;
                }
                if found.replace(device).is_some() {
                    return Err(Error::Limit);
                }
            }
        }
    }
    let device = found.ok_or(Error::Invalid)?;
    let identity = read(device, 0);
    if (identity != 0x000d_1b36 && !(cfg!(feature = "usb-diagnostic") && identity == 0x4ded_8086))
        || read(device, 12) & 0x7f0000 != 0
    {
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
    let physical = (high as u64) << 32 | (low & !15) as u64;
    let length = (!((mask_high as u64) << 32 | mask_low as u64)).wrapping_add(1);
    crate::logln!("PCI XHCI BAR: base={:#x} bytes={}", physical, length);
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
        command(device, saved);
        return Err(Error::Denied);
    }
    crate::logln!(
        "PCI XHCI PASS: bdf={:#x} BAR={:#x} bytes={}",
        device,
        physical,
        length
    );
    Ok((device, physical, length))
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
        } else {
            if process.usb.device.is_none() {
                return Err(Error::Denied);
            }
            if process.usb.pages == 64 {
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
