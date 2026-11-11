use super::{
    descriptors::{self, Configuration, Endpoint, Kind},
    hid::BootKeyboard,
    policy, Error, Event, Result,
};
use core::sync::atomic::{fence, Ordering};
use zos_libos as os;

fn check_csw(csw: &[u8; 13], tag: u32) -> Result<()> {
    if csw[..4] != 0x53425355_u32.to_le_bytes()
        || csw[4..8] != tag.to_le_bytes()
        || csw[8..12] != [0; 4]
        || csw[12] > 1
    {
        return Err(Error::Io);
    }
    if csw[12] != 0 {
        return Err(Error::Scsi(csw[12]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_stale_or_malformed_bulk_status() {
        let mut csw = [0; 13];
        csw[..4].copy_from_slice(&0x53425355_u32.to_le_bytes());
        csw[4..8].copy_from_slice(&42_u32.to_le_bytes());
        assert_eq!(check_csw(&csw, 42), Ok(()));
        assert_eq!(check_csw(&csw, 41), Err(Error::Io));
        for (index, byte) in [(0, 0), (8, 1), (12, 2), (12, 255)] {
            let mut bad = csw;
            bad[index] = byte;
            assert_eq!(check_csw(&bad, 42), Err(Error::Io));
        }
        csw[12] = 1;
        assert_eq!(check_csw(&csw, 42), Err(Error::Scsi(1)));
    }
}

struct Page(os::DmaPage);
impl Page {
    fn new() -> Result<Self> {
        let page = os::usb_dma_page().map(Self).map_err(|error| {
            crate::log(format_args!("USB DMA ERROR: {error:?}"));
            Error::Io
        })?;
        policy::dma_page(page.0.physical)?;
        Ok(page)
    }
    fn clear(&self) {
        for offset in (0..4096).step_by(4) {
            self.write(offset, 0);
        }
    }
    fn physical(&self, offset: usize) -> u64 {
        assert!(offset < 4096);
        self.0.physical + offset as u64
    }
    fn read(&self, offset: usize) -> u32 {
        assert!(offset <= 4092 && offset % 4 == 0);
        // SAFETY: a live, driver-owned DMA page; volatile accesses observe device writes.
        unsafe { core::ptr::read_volatile((self.0.address + offset as u64) as *const u32) }
    }
    fn write(&self, offset: usize, value: u32) {
        assert!(offset <= 4092 && offset % 4 == 0);
        // SAFETY: controller ownership is established before publishing each context/TRB.
        unsafe { core::ptr::write_volatile((self.0.address + offset as u64) as *mut u32, value) }
    }
    fn address(&self, offset: usize, value: u64) {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }
    fn put(&self, bytes: &[u8]) {
        assert!(bytes.len() <= 4096);
        for (index, &value) in bytes.iter().enumerate() {
            // SAFETY: this buffer is not in flight while CPU writes it.
            unsafe { core::ptr::write_volatile((self.0.address + index as u64) as *mut u8, value) };
        }
    }
    fn get(&self, bytes: &mut [u8]) {
        assert!(bytes.len() <= 4096);
        fence(Ordering::Acquire);
        for (index, value) in bytes.iter_mut().enumerate() {
            // SAFETY: a completed transfer relinquished the buffer to the CPU.
            *value =
                unsafe { core::ptr::read_volatile((self.0.address + index as u64) as *const u8) };
        }
    }
}

struct Ring {
    page: Page,
    index: usize,
    cycle: u32,
    wraps: usize,
}
impl Ring {
    fn new() -> Result<Self> {
        Ok(Self {
            page: Page::new()?,
            index: 0,
            cycle: 1,
            wraps: 0,
        })
    }
    fn push(&mut self, parameter: u64, status: u32, control: u32) -> u64 {
        let offset = self.index * 16;
        let physical = self.page.physical(offset);
        self.page.address(offset, parameter);
        self.page.write(offset + 8, status);
        fence(Ordering::Release);
        self.page.write(offset + 12, control | self.cycle);
        self.index += 1;
        if self.index == 255 {
            self.page.address(4080, self.page.physical(0));
            self.page.write(4088, 0);
            fence(Ordering::Release);
            self.page
                .write(4092, (6 << 10) | 2 | (control & 16) | self.cycle);
            self.index = 0;
            self.cycle ^= 1;
            self.wraps += 1;
        }
        physical
    }
    fn reset(&mut self) {
        self.page.clear();
        self.index = 0;
        self.cycle = 1;
        self.wraps = 0;
    }
}

struct Pipe {
    endpoint: Endpoint,
    ring: Ring,
    buffer: Page,
    pending: u64,
}
impl Pipe {
    fn new(endpoint: Endpoint) -> Result<Self> {
        Ok(Self {
            endpoint,
            ring: Ring::new()?,
            buffer: Page::new()?,
            pending: 0,
        })
    }
}
struct Device {
    slot: u8,
    port: u8,
    speed: u8,
    input: Page,
    output: Page,
    control: Ring,
    buffer: Page,
    kind: Option<Kind>,
    pipes: [Option<Pipe>; 2],
}

#[derive(Clone, Copy)]
struct Completion {
    parameter: u64,
    status: u32,
    control: u32,
}
impl Completion {
    fn kind(self) -> u32 {
        (self.control >> 10) & 63
    }
    fn slot(self) -> u8 {
        (self.control >> 24) as u8
    }
    fn endpoint(self) -> usize {
        ((self.control >> 16) & 31) as usize
    }
    fn code(self) -> u8 {
        (self.status >> 24) as u8
    }
    fn residual(self) -> usize {
        (self.status & 0xffffff) as usize
    }
}

pub struct Controller {
    read_only: bool,
    registers: os::UsbController,
    operational: usize,
    runtime: usize,
    doorbells: usize,
    context_size: usize,
    dcbaa: Page,
    scratch_array: Option<Page>,
    scratch: [Option<Page>; 32],
    _erst: Page,
    events: Page,
    event_index: usize,
    event_cycle: u32,
    event_wraps: usize,
    commands: Ring,
    devices: [Option<Device>; 3],
    spare: Option<Device>,
    keyboard: BootKeyboard,
    queue: [Option<Event>; 64],
    queued: usize,
    consumed: usize,
    storage: usize,
    tag: u32,
    sectors: u64,
    poisoned: bool,
    transfers: u64,
}

impl Controller {
    fn read_register(registers: &os::UsbController, offset: usize) -> u32 {
        assert!(offset % 4 == 0 && offset + 4 <= registers.length);
        // SAFETY: the kernel mapped only the exclusive controller BAR, uncached/NX.
        unsafe { core::ptr::read_volatile((registers.address + offset as u64) as *const u32) }
    }
    fn read(&self, offset: usize) -> u32 {
        Self::read_register(&self.registers, offset)
    }
    fn write(&self, offset: usize, value: u32) {
        assert!(offset % 4 == 0 && offset + 4 <= self.registers.length);
        fence(Ordering::SeqCst);
        // SAFETY: bounds-checked controller MMIO, exclusively owned by this service.
        unsafe {
            core::ptr::write_volatile((self.registers.address + offset as u64) as *mut u32, value)
        }
    }
    fn address(&self, offset: usize, value: u64) {
        self.write(offset, value as u32);
        self.write(offset + 4, (value >> 32) as u32);
    }
    fn ticks() -> u64 {
        os::usb_ticks().unwrap()
    }
    fn wait_register(&self, offset: usize, mask: u32, expected: u32) -> Result<()> {
        let deadline = Self::ticks() + 500;
        while self.read(offset) & mask != expected {
            if Self::ticks() >= deadline {
                return Err(Error::Timeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }
    pub fn new() -> Result<Self> {
        Self::initialize(false)
    }
    pub fn diagnostic() -> Result<Self> {
        let mut controller = Self::initialize(true)?;
        assert_eq!(
            controller.scsi(&[0x2a; 10], false, &mut [0; 512]),
            Err(Error::ReadOnly)
        );
        assert_eq!(
            controller.scsi(&[0x35; 10], false, &mut []),
            Err(Error::ReadOnly)
        );
        assert_eq!(
            controller.disk_request(&[0x12], &mut [0; 512]),
            Err(Error::ReadOnly)
        );
        crate::diagnostic(format_args!(
            "DIAGNOSTIC READ ONLY PASS: WRITE/FLUSH/RPC denied"
        ));
        Ok(controller)
    }
    fn report(&self, args: core::fmt::Arguments<'_>) {
        if self.read_only {
            crate::diagnostic(args);
        }
    }
    fn initialize(read_only: bool) -> Result<Self> {
        let registers = os::usb_controller().map_err(|_| Error::Io)?;
        let version = Self::read_register(&registers, 0);
        let structural = Self::read_register(&registers, 4);
        let secondary = Self::read_register(&registers, 8);
        let ports = (structural >> 24) as usize;
        if read_only {
            crate::diagnostic(format_args!(
                "XHCI CAP: version={:#x} ports={ports}",
                version >> 16
            ));
            crate::diagnostic(format_args!(
                "XHCI RAW: hcs1={structural:#010x} hcs2={secondary:#010x}"
            ));
        }
        let scratchpads = policy::scratchpads(secondary).inspect_err(|_| {
            if read_only {
                crate::diagnostic(format_args!("XHCI REJECT: scratchpad count exceeds 32"));
            }
        })?;
        if read_only {
            crate::diagnostic(format_args!(
                "XHCI CAP: scratchpads={scratchpads} DMA=32-bit"
            ));
            crate::diagnostic(format_args!("XHCI DMA: allocating controller rings"));
        }
        let mut controller = Self {
            read_only,
            registers,
            operational: 0,
            runtime: 0,
            doorbells: 0,
            context_size: 32,
            dcbaa: Page::new()?,
            scratch_array: None,
            scratch: core::array::from_fn(|_| None),
            _erst: Page::new()?,
            events: Page::new()?,
            event_index: 0,
            event_cycle: 1,
            event_wraps: 0,
            commands: Ring::new()?,
            devices: core::array::from_fn(|_| None),
            spare: None,
            keyboard: BootKeyboard::default(),
            queue: [None; 64],
            queued: 0,
            consumed: 0,
            storage: 0,
            tag: 0,
            sectors: 0,
            poisoned: false,
            transfers: 0,
        };
        controller.operational = (version & 255) as usize;
        controller.runtime = (controller.read(24) & !31) as usize;
        controller.doorbells = (controller.read(20) & !3) as usize;
        if controller.operational < 32
            || structural & 255 < 3
            || !(1..=32).contains(&ports)
            || controller.operational + 0x400 + ports * 16 > controller.registers.length
            || controller.runtime < 32
            || controller.runtime + 0x40 > controller.registers.length
            || controller.doorbells < 32
            || controller.doorbells + 256 * 4 > controller.registers.length
            || (!read_only && scratchpads != 0)
        {
            return Err(Error::Unsupported);
        }
        let capability = controller.read(16);
        controller.context_size = if capability & 4 != 0 { 64 } else { 32 };
        let mut protocols = [0_u8; 33];
        let mut extended = ((capability >> 16) * 4) as usize;
        for _ in 0..64 {
            if extended == 0 {
                break;
            }
            if extended + 8 > controller.registers.length {
                return Err(Error::Invalid);
            }
            let cap = controller.read(extended);
            if cap & 255 == 1 {
                controller.report(format_args!("XHCI ownership: requesting firmware handoff"));
                controller.write(extended, cap | (1 << 24));
                controller.wait_register(extended, 1 << 16, 0)?;
                controller.write(extended + 4, 0);
            } else if cap & 255 == 2 && read_only {
                if extended + 16 > controller.registers.length {
                    return Err(Error::Invalid);
                }
                let revision = (cap >> 24) as u8;
                let range = controller.read(extended + 8);
                let first = (range & 255) as usize;
                let count = ((range >> 8) & 255) as usize;
                let speeds = (range >> 28) as usize;
                if first == 0
                    || first + count > ports + 1
                    || extended + 16 + speeds * 4 > controller.registers.length
                {
                    return Err(Error::Invalid);
                }
                let mut entries = [0; 15];
                for (index, entry) in entries[..speeds].iter_mut().enumerate() {
                    *entry = controller.read(extended + 16 + index * 4);
                }
                if policy::speeds(revision, &entries[..speeds]).is_ok() {
                    for protocol in &mut protocols[first..first + count] {
                        if *protocol != 0 {
                            return Err(Error::Invalid);
                        }
                        *protocol = revision;
                    }
                } else {
                    controller.report(format_args!(
                        "XHCI protocol USB{revision}: speeds unsupported"
                    ));
                }
                controller.report(format_args!(
                    "XHCI protocol USB{revision}: ports {first}+{count}"
                ));
            }
            let next = ((cap >> 8) & 255) as usize * 4;
            if next == 0 {
                extended = 0;
                break;
            }
            extended += next;
        }
        if extended != 0 {
            return Err(Error::Invalid);
        }
        let op = controller.operational;
        controller.write(op, 0);
        controller.wait_register(op + 4, 1, 1)?;
        controller.write(op, 2);
        controller.wait_register(op, 2, 0)?;
        controller.wait_register(op + 4, 1 << 11, 0)?;
        if controller.read(op + 8) & 1 == 0 {
            return Err(Error::Unsupported);
        }
        if capability & 1 == 0 && controller.dcbaa.physical(0) > u32::MAX as u64 {
            return Err(Error::Unsupported);
        }
        if scratchpads != 0 {
            let array = Page::new()?;
            for index in 0..scratchpads {
                let page = Page::new()?;
                array.address(index * 8, page.physical(0));
                controller.scratch[index] = Some(page);
            }
            controller.dcbaa.address(0, array.physical(0));
            controller.scratch_array = Some(array);
        }
        // The old firmware rings have been reset before granting bus mastering.
        // ERSTBA programming itself can cause the controller to read DMA memory.
        os::usb_enable_dma().map_err(|_| Error::Io)?;
        controller.address(op + 0x30, controller.dcbaa.physical(0));
        controller.address(op + 0x18, controller.commands.page.physical(0) | 1);
        controller.write(op + 0x38, 3);
        controller._erst.address(0, controller.events.physical(0));
        controller._erst.write(8, 256);
        controller.write(controller.runtime + 0x28, 1);
        controller.address(controller.runtime + 0x30, controller._erst.physical(0));
        controller.address(controller.runtime + 0x38, controller.events.physical(0));
        controller.write(controller.runtime + 0x20, 0);
        controller.write(op, 1);
        controller.wait_register(op + 4, 1, 0)?;
        crate::log(format_args!(
            "XHCI ready: ports={ports} context={}",
            controller.context_size
        ));
        controller.report(format_args!(
            "XHCI running: context={} scratchpads={scratchpads}",
            controller.context_size
        ));
        let mut count = 0;
        for (port, &protocol) in protocols.iter().enumerate().take(ports + 1).skip(1) {
            let offset = op + 0x400 + (port - 1) * 16;
            let mut status = controller.read(offset);
            if read_only && capability & 8 != 0 && status & (1 << 9) == 0 {
                controller.write(offset, 1 << 9);
                let deadline = Self::ticks() + 20;
                while Self::ticks() < deadline {
                    core::hint::spin_loop();
                }
                status = controller.read(offset);
            }
            if status & 1 == 0 {
                continue;
            }
            if read_only {
                controller.report(format_args!("USB PORT {port}: status={status:#010x}"));
                if protocol == 0 {
                    controller.report(format_args!("USB SKIP port={port}: unsupported protocol"));
                    continue;
                }
            }
            if count == 3 {
                if read_only {
                    controller.report(format_args!("USB SKIP port={port}: three-device limit"));
                    continue;
                }
                return Err(Error::Unsupported);
            }
            controller.write(offset, (status & (1 << 9)) | (1 << 4));
            controller.wait_register(offset, (1 << 4) | 3, 3)?;
            let speed = ((controller.read(offset) >> 10) & 15) as u8;
            match controller.enumerate(count, port as u8, speed) {
                Ok(()) => (),
                Err(Error::Unsupported) if read_only => {
                    controller.report(format_args!("USB SKIP port={port}: unsupported device"));
                    if let Some(device) = controller.devices[count].take() {
                        controller.command(10, 0, device.slot)?;
                        controller.dcbaa.address(device.slot as usize * 8, 0);
                        controller.spare = Some(device);
                    }
                    continue;
                }
                Err(error) => {
                    controller.report(format_args!("USB FAIL port={port}: {error:?}"));
                    return Err(error);
                }
            }
            if read_only && controller.devices[count].as_ref().unwrap().kind == Some(Kind::Storage)
            {
                controller.storage = count;
                controller.initialize_disk()?;
            }
            count += 1;
        }
        if read_only {
            controller.report(format_args!(
                "DIAGNOSTIC ENUMERATION: {count} supported devices"
            ));
            return Ok(controller);
        }
        if count != 3
            || controller
                .devices
                .iter()
                .filter(|d| d.as_ref().unwrap().kind == Some(Kind::Keyboard))
                .count()
                != 1
            || controller
                .devices
                .iter()
                .filter(|d| d.as_ref().unwrap().kind == Some(Kind::Mouse))
                .count()
                != 1
            || controller
                .devices
                .iter()
                .filter(|d| d.as_ref().unwrap().kind == Some(Kind::Storage))
                .count()
                != 1
        {
            return Err(Error::Unsupported);
        }
        controller.storage = controller
            .devices
            .iter()
            .position(|d| d.as_ref().unwrap().kind == Some(Kind::Storage))
            .unwrap();
        controller.initialize_disk()?;
        os::debug_write("USB ready: boot HID keyboard/mouse, bulk-only SCSI storage").unwrap();
        Ok(controller)
    }
    fn next_completion(&mut self) -> Result<Option<Completion>> {
        if self.read(self.operational + 4) & 0x1004 != 0 {
            crate::log(format_args!(
                "XHCI HOST ERROR: status={:#x}",
                self.read(self.operational + 4)
            ));
            return Err(Error::Io);
        }
        let offset = self.event_index * 16;
        let control = self.events.read(offset + 12);
        if control & 1 != self.event_cycle {
            return Ok(None);
        }
        fence(Ordering::Acquire);
        let event = Completion {
            parameter: self.events.read(offset) as u64
                | (self.events.read(offset + 4) as u64) << 32,
            status: self.events.read(offset + 8),
            control,
        };
        self.event_index += 1;
        if self.event_index == 256 {
            self.event_index = 0;
            self.event_cycle ^= 1;
            self.event_wraps += 1;
            if self.event_wraps == 1 {
                os::debug_write("USB EVENT RING WRAP PASS").unwrap();
            }
        }
        self.address(
            self.runtime + 0x38,
            self.events.physical(self.event_index * 16) | 8,
        );
        Ok(Some(event))
    }
    fn wait(&mut self, kind: u32, parameter: u64, slot: u8, endpoint: usize) -> Result<Completion> {
        let deadline = Self::ticks() + 500;
        loop {
            if let Some(event) = self.next_completion()? {
                if event.kind() == kind
                    && event.parameter & !15 == parameter
                    && (kind == 33 || (event.slot() == slot && event.endpoint() == endpoint))
                {
                    if event.code() != 1 && !(kind == 32 && event.code() == 13) {
                        crate::log(format_args!(
                            "USB completion: kind={kind} code={}",
                            event.code()
                        ));
                        return Err(Error::Completion(event.code()));
                    }
                    self.transfers += 1;
                    return Ok(event);
                }
                self.asynchronous(event)?;
            }
            if Self::ticks() >= deadline {
                crate::log(format_args!(
                    "USB timeout: kind={kind} slot={slot} ep={endpoint}"
                ));
                return Err(Error::Timeout);
            }
            core::hint::spin_loop();
        }
    }
    fn command(&mut self, kind: u32, parameter: u64, slot: u8) -> Result<u8> {
        let physical = self
            .commands
            .push(parameter, 0, kind << 10 | (slot as u32) << 24);
        self.write(self.doorbells, 0);
        Ok(self.wait(33, physical, 0, 0)?.slot())
    }
    pub fn test_rings(&mut self) -> Result<()> {
        for _ in 0..260 {
            self.command(23, 0, 0)?;
        }
        if self.commands.wraps == 0 {
            return Err(Error::Invalid);
        }
        os::debug_write("USB COMMAND RING WRAP PASS: 260 completed commands").unwrap();
        Ok(())
    }
    fn context(&self, page: &Page, index: usize, words: [u32; 5]) {
        for (word, value) in words.into_iter().enumerate() {
            page.write(index * self.context_size + word * 4, value);
        }
    }
    fn enumerate(&mut self, index: usize, port: u8, speed: u8) -> Result<()> {
        let packet = match speed {
            1 | 2 => 8,
            3 => 64,
            4 | 5 => 512,
            _ => return Err(Error::Unsupported),
        };
        let slot = self.command(9, 0, 0)?;
        if slot == 0 || slot > 3 {
            return Err(Error::Invalid);
        }
        let device = if let Some(mut spare) = self.spare.take() {
            spare.input.clear();
            spare.output.clear();
            spare.buffer.clear();
            spare.control.reset();
            spare.slot = slot;
            spare.port = port;
            spare.speed = speed;
            spare.kind = None;
            spare
        } else {
            Device {
                slot,
                port,
                speed,
                input: Page::new()?,
                output: Page::new()?,
                control: Ring::new()?,
                buffer: Page::new()?,
                kind: None,
                pipes: core::array::from_fn(|_| None),
            }
        };
        self.dcbaa
            .address(slot as usize * 8, device.output.physical(0));
        device.input.write(4, 3);
        self.context(
            &device.input,
            1,
            [
                (1 << 27) | (speed as u32) << 20,
                (port as u32) << 16,
                0,
                0,
                0,
            ],
        );
        let pointer = device.control.page.physical(0) | 1;
        self.context(
            &device.input,
            2,
            [
                0,
                (packet << 16) | (4 << 3) | 6,
                pointer as u32,
                (pointer >> 32) as u32,
                8,
            ],
        );
        let input = device.input.physical(0);
        self.devices[index] = Some(device);
        self.command(11, input, slot)?;
        let mut descriptor = [0; 18];
        self.control(index, [0x80, 6, 0, 1, 0, 0, 8, 0], &mut descriptor[..8])?;
        if descriptor[0] != 18 || descriptor[1] != 1 || descriptor[4] == 9 {
            return Err(Error::Unsupported);
        }
        let actual_packet = if speed >= 4 {
            if descriptor[7] != 9 {
                return Err(Error::Unsupported);
            }
            512
        } else {
            if !matches!(descriptor[7], 8 | 16 | 32 | 64) {
                return Err(Error::Invalid);
            }
            descriptor[7] as u32
        };
        if actual_packet != packet {
            let device = self.devices[index].as_ref().unwrap();
            device.input.write(4, 2);
            device.input.write(
                2 * self.context_size + 4,
                (actual_packet << 16) | (4 << 3) | 6,
            );
            self.command(13, input, slot)?;
        }
        self.control(index, [0x80, 6, 0, 1, 0, 0, 18, 0], &mut descriptor)?;
        self.report(format_args!(
            "USB DEVICE port={port}: {:04x}:{:04x}",
            u16::from_le_bytes([descriptor[8], descriptor[9]]),
            u16::from_le_bytes([descriptor[10], descriptor[11]])
        ));
        let mut bytes = [0; 512];
        self.control(index, [0x80, 6, 0, 2, 0, 0, 9, 0], &mut bytes[..9])?;
        let length = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
        if !(9..=512).contains(&length) {
            return Err(Error::Unsupported);
        }
        self.control(
            index,
            [0x80, 6, 0, 2, 0, 0, length as u8, (length >> 8) as u8],
            &mut bytes[..length],
        )?;
        let configuration = descriptors::parse(&bytes[..length])?;
        if self.read_only
            && self
                .devices
                .iter()
                .flatten()
                .any(|device| device.kind == Some(configuration.kind))
        {
            return Err(Error::Unsupported);
        }
        self.configure(index, &configuration)?;
        self.control(index, [0, 9, configuration.value, 0, 0, 0, 0, 0], &mut [])?;
        if configuration.kind != Kind::Storage {
            self.control(
                index,
                [0x21, 0x0b, 0, 0, configuration.interface, 0, 0, 0],
                &mut [],
            )?;
            self.control(
                index,
                [0x21, 0x0a, 0, 0, configuration.interface, 0, 0, 0],
                &mut [],
            )?;
            self.post_hid(index);
        } else {
            let mut lun = [0];
            self.control(
                index,
                [0xa1, 0xfe, 0, 0, configuration.interface, 0, 1, 0],
                &mut lun,
            )?;
            if lun[0] != 0 {
                return Err(Error::Unsupported);
            }
        }
        crate::log(format_args!(
            "USB ENUM PASS: slot={slot} port={port} {:?}",
            configuration.kind
        ));
        self.report(format_args!(
            "USB DEVICE READY: port={port} {:?}",
            configuration.kind
        ));
        Ok(())
    }
    fn configure(&mut self, index: usize, config: &Configuration) -> Result<()> {
        if config.kind != Kind::Storage
            && self.devices[index].as_ref().unwrap().speed >= 3
            && config.input.interval > 16
        {
            return Err(Error::Unsupported);
        }
        let mut maximum = config.input.index();
        if let Some(output) = config.output {
            maximum = maximum.max(output.index());
        }
        let device = self.devices[index].as_mut().unwrap();
        device.kind = Some(config.kind);
        device.input.write(0, 0);
        device.input.write(4, 1);
        let (slot, input) = (device.slot, device.input.physical(0));
        let (speed, port) = (device.speed, device.port);
        for (i, endpoint) in [Some(config.input), config.output].into_iter().enumerate() {
            let Some(endpoint) = endpoint else {
                continue;
            };
            let pipe = if let Some(mut pipe) = device.pipes[i].take() {
                pipe.ring.reset();
                pipe.buffer.clear();
                pipe.endpoint = endpoint;
                pipe.pending = 0;
                pipe
            } else {
                Pipe::new(endpoint)?
            };
            let ep = endpoint.index();
            device.input.write(4, device.input.read(4) | 1 << ep);
            let interval = if config.kind == Kind::Storage {
                0
            } else if speed >= 3 {
                endpoint.interval as u32 - 1
            } else {
                32 - (endpoint.interval as u32 * 8 - 1).leading_zeros()
            };
            let pointer = pipe.ring.page.physical(0) | 1;
            let typ = if config.kind != Kind::Storage {
                7
            } else if i == 0 {
                6
            } else {
                2
            };
            let payload = endpoint.packet as u32 * (endpoint.burst as u32 + 1);
            let words = [
                interval << 16,
                (endpoint.packet as u32) << 16 | (endpoint.burst as u32) << 8 | typ << 3 | 6,
                pointer as u32,
                (pointer >> 32) as u32,
                if config.kind == Kind::Storage {
                    512
                } else {
                    8 | payload << 16
                },
            ];
            for (word, value) in words.into_iter().enumerate() {
                device
                    .input
                    .write((ep + 1) * self.context_size + word * 4, value);
            }
            device.pipes[i] = Some(pipe);
        }
        let page = &self.devices[index].as_ref().unwrap().input;
        self.context(
            page,
            1,
            [
                (maximum as u32) << 27 | (speed as u32) << 20,
                (port as u32) << 16,
                0,
                0,
                0,
            ],
        );
        self.command(12, input, slot)?;
        Ok(())
    }
    fn control(&mut self, index: usize, setup: [u8; 8], bytes: &mut [u8]) -> Result<()> {
        let device = self.devices[index].as_mut().unwrap();
        let input = setup[0] & 0x80 != 0;
        if !input {
            device.buffer.put(bytes);
        }
        let trt = if bytes.is_empty() {
            0
        } else if input {
            3
        } else {
            2
        };
        device.control.push(
            u64::from_le_bytes(setup),
            8,
            (2 << 10) | 64 | 16 | (trt << 16),
        );
        if !bytes.is_empty() {
            device.control.push(
                device.buffer.physical(0),
                bytes.len() as u32,
                (3 << 10) | 16 | if input { 1 << 16 } else { 0 },
            );
        }
        let physical = device.control.push(
            0,
            0,
            (4 << 10)
                | 32
                | if bytes.is_empty() || !input {
                    1 << 16
                } else {
                    0
                },
        );
        let slot = device.slot;
        self.write(self.doorbells + slot as usize * 4, 1);
        let event = self.wait(32, physical, slot, 1)?;
        if event.residual() != 0 {
            return Err(Error::Invalid);
        }
        if input {
            self.devices[index].as_ref().unwrap().buffer.get(bytes);
        }
        Ok(())
    }
    fn post_hid(&mut self, index: usize) {
        let device = self.devices[index].as_mut().unwrap();
        let length = if device.kind == Some(Kind::Keyboard) {
            8
        } else {
            3
        };
        let pipe = device.pipes[0].as_mut().unwrap();
        assert_eq!(pipe.pending, 0);
        pipe.pending = pipe
            .ring
            .push(pipe.buffer.physical(0), length, (1 << 10) | 32 | 4);
        if pipe.ring.index == 0 && pipe.ring.wraps == 1 {
            crate::log(format_args!(
                "USB HID RING WRAP PASS: {:?}",
                device.kind.unwrap()
            ));
        }
        let (slot, endpoint) = (device.slot, pipe.endpoint.index());
        self.write(self.doorbells + slot as usize * 4, endpoint as u32);
    }
    fn queue(&mut self, event: Event) -> Result<()> {
        if self.queued - self.consumed == self.queue.len() {
            return Err(Error::Io);
        }
        self.queue[self.queued % 64] = Some(event);
        self.queued += 1;
        Ok(())
    }
    fn asynchronous(&mut self, event: Completion) -> Result<()> {
        if event.kind() == 34 {
            let port = (event.parameter >> 24) as usize;
            if port == 0 || port > (self.read(4) >> 24) as usize {
                return Err(Error::Invalid);
            }
            if self.read_only {
                let offset = self.operational + 0x400 + (port - 1) * 16;
                let status = self.read(offset);
                self.write(offset, (status & (1 << 9)) | (status & 0x00fe0000));
            }
            if self
                .devices
                .iter()
                .flatten()
                .any(|device| device.port as usize == port)
                && self.read(self.operational + 0x400 + (port - 1) * 16) & 1 == 0
            {
                return Err(Error::Io);
            }
            return Ok(());
        }
        if event.kind() != 32 {
            return Err(Error::Invalid);
        }
        let index = self
            .devices
            .iter()
            .position(|d| d.as_ref().is_some_and(|d| d.slot == event.slot()))
            .ok_or(Error::Invalid)?;
        let device = self.devices[index].as_mut().unwrap();
        let pipe = device.pipes[0].as_mut().ok_or(Error::Invalid)?;
        if device.kind == Some(Kind::Storage)
            || pipe.pending != event.parameter & !15
            || event.endpoint() != pipe.endpoint.index()
        {
            return Err(Error::Invalid);
        }
        if event.code() != 1 && event.code() != 13 {
            return Err(Error::Completion(event.code()));
        }
        let kind = device.kind.unwrap();
        let mut bytes = [0; 8];
        let length = if kind == Kind::Keyboard { 8 } else { 3 };
        if event.residual() != 0 {
            return Err(Error::Invalid);
        }
        pipe.buffer.get(&mut bytes[..length]);
        pipe.pending = 0;
        if kind == Kind::Keyboard {
            let mut keys = [None; 6];
            let mut count = 0;
            self.keyboard.report(bytes, |key| {
                keys[count] = Some(key);
                count += 1;
            });
            for key in keys.into_iter().flatten() {
                self.queue(Event::Key(key))?;
            }
        } else {
            let dx = bytes[1] as i8 as i16;
            let dy = -(bytes[2] as i8 as i16);
            crate::log(format_args!(
                "USB HID MOUSE PASS: dx={dx} dy={dy} b={}",
                bytes[0] & 7
            ));
            self.queue(Event::Pointer {
                dx,
                dy,
                buttons: bytes[0] & 7,
            })?;
        }
        self.post_hid(index);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        for _ in 0..256 {
            match self.next_completion()? {
                Some(event) => self.asynchronous(event)?,
                None => return Ok(()),
            }
        }
        Err(Error::Io)
    }
    pub fn event(&self) -> Option<Event> {
        if self.queued == self.consumed {
            return None;
        }
        self.queue[self.consumed % 64]
    }
    pub fn consume_event(&mut self) {
        assert!(self.consumed < self.queued);
        self.queue[self.consumed % 64] = None;
        self.consumed += 1;
        if self.consumed == self.queued {
            self.consumed = 0;
            self.queued = 0;
        }
    }
    fn bulk(&mut self, input: bool, bytes: &mut [u8]) -> Result<()> {
        let device = self.devices[self.storage].as_mut().unwrap();
        let pipe = device.pipes[if input { 0 } else { 1 }].as_mut().unwrap();
        if !input {
            pipe.buffer.put(bytes);
        }
        let physical = pipe.ring.push(
            pipe.buffer.physical(0),
            bytes.len() as u32,
            (1 << 10) | 32 | 4,
        );
        if pipe.ring.index == 0 && pipe.ring.wraps == 1 {
            crate::log(format_args!("USB BULK RING WRAP PASS: input={input}"));
        }
        let (slot, endpoint) = (device.slot, pipe.endpoint.index());
        self.write(self.doorbells + slot as usize * 4, endpoint as u32);
        let event = self.wait(32, physical, slot, endpoint)?;
        if event.residual() != 0 {
            return Err(Error::Io);
        }
        if input {
            self.devices[self.storage].as_ref().unwrap().pipes[0]
                .as_ref()
                .unwrap()
                .buffer
                .get(bytes);
        }
        Ok(())
    }
    fn scsi(&mut self, command: &[u8], input: bool, bytes: &mut [u8]) -> Result<()> {
        policy::scsi(self.read_only, command, input, bytes.len())?;
        self.tag = self.tag.checked_add(1).ok_or(Error::Io)?;
        let mut cbw = [0; 31];
        cbw[..4].copy_from_slice(&0x43425355_u32.to_le_bytes());
        cbw[4..8].copy_from_slice(&self.tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        cbw[12] = if input { 0x80 } else { 0 };
        cbw[14] = command.len() as u8;
        cbw[15..15 + command.len()].copy_from_slice(command);
        self.bulk(false, &mut cbw)?;
        if !bytes.is_empty() {
            self.bulk(input, bytes)?;
        }
        let mut csw = [0; 13];
        self.bulk(true, &mut csw)?;
        check_csw(&csw, self.tag)
    }

    fn initialize_disk(&mut self) -> Result<()> {
        let mut inquiry = [0; 36];
        self.scsi(&[0x12, 0, 0, 0, 36, 0], true, &mut inquiry)?;
        if inquiry[0] & 31 != 0 {
            return Err(Error::Unsupported);
        }
        match self.scsi(&[0; 6], false, &mut []) {
            Ok(()) => (),
            Err(Error::Scsi(1)) => {
                let mut sense = [0; 18];
                self.scsi(&[3, 0, 0, 0, 18, 0], true, &mut sense)?;
                if sense[2] & 15 != 6 {
                    return Err(Error::Scsi(1));
                }
                self.scsi(&[0; 6], false, &mut [])?;
            }
            Err(error) => return Err(error),
        }
        let mut capacity = [0; 8];
        self.scsi(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], true, &mut capacity)?;
        let (sectors, block) = policy::capacity(capacity, self.read_only)?;
        self.sectors = sectors;
        if self.read_only {
            self.report(format_args!("USB DISK: sectors={sectors} block={block}"));
            if block == 512 {
                let mut sector = [0; 512];
                let mut read = [0; 10];
                read[0] = 0x28;
                read[8] = 1;
                self.scsi(&read, true, &mut sector)?;
                self.report(format_args!(
                    "USB READ PASS: sector0 signature={:02x}{:02x}",
                    sector[510], sector[511]
                ));
            }
            return Ok(());
        }
        os::debug_write("USB SCSI PASS: inquiry, readiness, 32768 sectors of 512 bytes").unwrap();
        Ok(())
    }
    pub fn disk_request(&mut self, request: &[u8], reply: &mut [u8; 512]) -> Result<usize> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if self.poisoned {
            return Err(Error::Io);
        }
        let result = match request {
            [0x13] => {
                reply[..4].copy_from_slice(&(self.sectors as u32).to_le_bytes());
                Ok(4)
            }
            [0x12] => self
                .scsi(&[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], false, &mut [])
                .map(|_| 0),
            [op @ (0x10 | 0x11), address @ ..] => {
                let input = *op == 0x10;
                if address.len() != if input { 4 } else { 516 } {
                    return Err(Error::Invalid);
                }
                let sector = u32::from_le_bytes(address[..4].try_into().unwrap());
                if u64::from(sector) >= self.sectors {
                    return Err(Error::Invalid);
                }
                let mut command = [0; 10];
                command[0] = if input { 0x28 } else { 0x2a };
                command[2..6].copy_from_slice(&sector.to_be_bytes());
                command[8] = 1;
                if !input {
                    reply.copy_from_slice(&address[4..]);
                }
                self.scsi(&command, input, reply)
                    .map(|_| if input { 512 } else { 0 })
            }
            _ => Err(Error::Invalid),
        };
        if let Err(error) = result {
            crate::log(format_args!("USB DISK ERROR: {error:?}"));
            if error != Error::Invalid {
                self.poisoned = true;
            }
        }
        result
    }
}
