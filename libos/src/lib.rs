#![no_std]

pub const MAX_MESSAGE: usize = 64;
pub const MAX_STORAGE_MESSAGE: usize = 1024;
pub const USB_MAX_DEVICES: usize = 8;
pub const USB_MAX_SCRATCHPADS: usize = 64;
pub const USB_DMA_PAGES: u64 = 5 + USB_MAX_SCRATCHPADS as u64 + USB_MAX_DEVICES as u64 * 8;

#[repr(u64)]
pub enum Call {
    ChannelCreate = 0,
    Send = 1,
    Receive = 2,
    Reply = 3,
    ThreadCreate = 4,
    MapMemory = 5,
    IrqAttach = 6,
    IoPortGrant = 7,
    Exit = 8,
    DebugWrite = 9,
    FramebufferMap = 10,
    ConsoleTestFinish = 11,
    StorageTestFinish = 12,
    ProcessSpawn = 13,
    ProcessInfo = 14,
    ProcessKill = 15,
    ProcessReap = 16,
    MemoryStats = 17,
    BootProgram = 18,
    SystemControl = 19,
    UsbController = 20,
    UsbDmaPage = 21,
    UsbTicks = 22,
    UsbEnableDma = 23,
    SendReady = 24,
    LogRead = 25,
    I2cController = 26,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    Invalid = 1,
    BadAddress = 2,
    NoMemory = 3,
    Denied = 4,
    Closed = 5,
    Deadlock = 6,
    TooLarge = 7,
    Limit = 8,
    Busy = 9,
}

impl Error {
    pub fn decode(value: i64) -> Self {
        match value {
            2 => Self::BadAddress,
            3 => Self::NoMemory,
            4 => Self::Denied,
            5 => Self::Closed,
            6 => Self::Deadlock,
            7 => Self::TooLarge,
            8 => Self::Limit,
            9 => Self::Busy,
            _ => Self::Invalid,
        }
    }
}

/// Invoke the raw syscall ABI.
///
/// # Safety
/// Buffers the kernel can successfully access must obey Rust's aliasing and
/// lifetime requirements for the entire call. Invalid ranges or permissions may
/// be passed to test rejection. Thread entries must obey the thread ABI and
/// mapping requests must not invalidate live Rust values. SYSCALL clobbers RCX/R11.
pub unsafe fn raw(number: u64, arguments: [u64; 5]) -> (i64, usize) {
    let result: i64;
    let length: usize;
    // SAFETY: the ABI saves all registers except the declared outputs/clobbers,
    // returns with the same stack, and may read/write memory passed by the caller.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => result,
            in("rdi") arguments[0],
            in("rsi") arguments[1],
            inlateout("rdx") arguments[2] => length,
            in("r10") arguments[3],
            in("r8") arguments[4],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    (result, length)
}

fn call(number: Call, arguments: [u64; 5]) -> Result<(usize, usize), Error> {
    // SAFETY: all safe wrappers below supply live buffers for the entire blocking
    // call; pointer-free calls use integer arguments only.
    let (result, length) = unsafe { raw(number as u64, arguments) };
    if result < 0 {
        Err(Error::decode(-result))
    } else {
        Ok((result as usize, length))
    }
}

pub fn channel_create() -> Result<usize, Error> {
    call(Call::ChannelCreate, [0; 5]).map(|result| result.0)
}

pub fn send(channel: usize, message: &[u8], reply: &mut [u8]) -> Result<usize, Error> {
    call(
        Call::Send,
        [
            channel as u64,
            message.as_ptr() as u64,
            message.len() as u64,
            reply.as_mut_ptr() as u64,
            reply.len() as u64,
        ],
    )
    .map(|result| result.0)
}

/// USB-driver-only send: return Busy unless the recipient is already receiving
/// on this channel. Once accepted, wait for its ordinary authenticated reply.
pub fn send_ready(channel: usize, message: &[u8], reply: &mut [u8]) -> Result<usize, Error> {
    call(
        Call::SendReady,
        [
            channel as u64,
            message.as_ptr() as u64,
            message.len() as u64,
            reply.as_mut_ptr() as u64,
            reply.len() as u64,
        ],
    )
    .map(|result| result.0)
}

pub fn receive(channel: usize, message: &mut [u8]) -> Result<(usize, usize), Error> {
    call(
        Call::Receive,
        [
            channel as u64,
            message.as_mut_ptr() as u64,
            message.len() as u64,
            0,
            0,
        ],
    )
}

pub fn reply(token: usize, message: &[u8]) -> Result<(), Error> {
    call(
        Call::Reply,
        [
            token as u64,
            message.as_ptr() as u64,
            message.len() as u64,
            0,
            0,
        ],
    )
    .map(|_| ())
}

pub fn thread_create(
    entry: extern "C" fn(usize) -> !,
    argument: usize,
    priority: u8,
) -> Result<usize, Error> {
    call(
        Call::ThreadCreate,
        [
            entry as usize as u64,
            argument as u64,
            priority as u64,
            0,
            0,
        ],
    )
    .map(|result| result.0)
}

pub fn map_memory(address: u64, writable: bool, executable: bool) -> Result<(), Error> {
    call(
        Call::MapMemory,
        [
            address,
            writable as u64 | ((executable as u64) << 1),
            0,
            0,
            0,
        ],
    )
    .map(|_| ())
}

pub fn irq_attach(channel: usize, irq: u8) -> Result<(), Error> {
    call(Call::IrqAttach, [channel as u64, irq as u64, 0, 0, 0]).map(|_| ())
}

pub fn io_port_grant(first: u16, count: u16) -> Result<(), Error> {
    call(Call::IoPortGrant, [first as u64, count as u64, 0, 0, 0]).map(|_| ())
}

pub fn debug_write(text: &str) -> Result<(), Error> {
    call(
        Call::DebugWrite,
        [text.as_ptr() as u64, text.len() as u64, 0, 0, 0],
    )
    .map(|_| ())
}

#[derive(Debug)]
pub struct UsbController {
    pub address: u64,
    pub length: usize,
}

#[derive(Debug)]
pub struct DmaPage {
    pub address: u64,
    pub physical: u64,
}

/// Available only to the boot input service in USB images.
pub fn usb_controller() -> Result<UsbController, Error> {
    let mut bytes = [0; 16];
    call(
        Call::UsbController,
        [bytes.as_mut_ptr() as u64, 16, 0, 0, 0],
    )?;
    Ok(UsbController {
        address: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
        length: u64::from_le_bytes(bytes[8..].try_into().unwrap()) as usize,
    })
}

/// Register window of an Intel serial-IO (I2C) controller found on PCI.
#[derive(Debug)]
pub struct I2cController {
    pub address: u64,
    pub length: usize,
}

/// The `index`th I2C controller (PCI vendor 8086, class 0x118000), mapped uncached at
/// its own address. `Invalid` means there is no such controller (end of the list);
/// `Denied` means it exists but cannot be used (no BAR, bridge, overlapping RAM).
/// Available only to the boot input service in USB images.
pub fn i2c_controller(index: usize) -> Result<I2cController, Error> {
    let mut bytes = [0; 16];
    call(
        Call::I2cController,
        [bytes.as_mut_ptr() as u64, 16, index as u64, 0, 0],
    )?;
    Ok(I2cController {
        address: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
        length: u64::from_le_bytes(bytes[8..].try_into().unwrap()) as usize,
    })
}

/// One zeroed, physically contiguous 4 KiB page; capped at 64 pages per driver.
pub fn usb_dma_page() -> Result<DmaPage, Error> {
    let mut bytes = [0; 16];
    call(Call::UsbDmaPage, [bytes.as_mut_ptr() as u64, 16, 0, 0, 0])?;
    Ok(DmaPage {
        address: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
        physical: u64::from_le_bytes(bytes[8..].try_into().unwrap()),
    })
}

pub fn usb_ticks() -> Result<u64, Error> {
    call(Call::UsbTicks, [0; 5]).map(|value| value.0 as u64)
}

pub fn usb_enable_dma() -> Result<(), Error> {
    call(Call::UsbEnableDma, [0; 5]).map(|_| ())
}

#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    pub address: u64,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    pub shifts: [u8; 3],
    pub length: usize,
}

pub fn framebuffer_map() -> Result<Framebuffer, Error> {
    let mut bytes = [0_u8; 48];
    call(
        Call::FramebufferMap,
        [bytes.as_mut_ptr() as u64, bytes.len() as u64, 0, 0, 0],
    )?;
    let word = |i| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
    Ok(Framebuffer {
        address: word(0),
        width: word(8) as usize,
        height: word(16) as usize,
        pitch: word(24) as usize,
        shifts: [bytes[32], bytes[33], bytes[34]],
        length: word(40) as usize,
    })
}

/// Available only to the initial TTY in dedicated console acceptance images.
pub fn console_test_finish() -> Result<(), Error> {
    call(Call::ConsoleTestFinish, [0; 5]).map(|_| ())
}

pub fn storage_test_finish() -> Result<(), Error> {
    call(Call::StorageTestFinish, [0; 5]).map(|_| ())
}

pub const PROCESS_NAME: usize = 24;
pub const PROCESS_SLOTS: usize = 32;

#[derive(Clone, Copy, Debug)]
pub struct ProcessInfo {
    pub pid: u64,
    pub state: u8,
    pub system: bool,
    pub memory: u64,
    pub status: i64,
    pub name: [u8; PROCESS_NAME],
}
impl ProcessInfo {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 50
            || bytes[8] > 4
            || bytes[9] > 1
            || bytes[26..]
                .iter()
                .any(|b| *b != 0 && !(32..=126).contains(b))
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            pid: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            state: bytes[8],
            system: bytes[9] != 0,
            memory: u64::from_le_bytes(bytes[10..18].try_into().unwrap()),
            status: i64::from_le_bytes(bytes[18..26].try_into().unwrap()),
            name: bytes[26..50].try_into().unwrap(),
        })
    }
    pub fn name(&self) -> &str {
        let length = self
            .name
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(PROCESS_NAME);
        core::str::from_utf8(&self.name[..length]).expect("Validated process name")
    }
}

pub fn process_spawn(image: &[u8], name: &[u8]) -> Result<u64, Error> {
    call(
        Call::ProcessSpawn,
        [
            image.as_ptr() as u64,
            image.len() as u64,
            name.as_ptr() as u64,
            name.len() as u64,
            0,
        ],
    )
    .map(|r| r.0 as u64)
}
pub fn process_info(slot: usize) -> Result<ProcessInfo, Error> {
    let mut bytes = [0; 50];
    call(
        Call::ProcessInfo,
        [slot as u64, bytes.as_mut_ptr() as u64, 50, 0, 0],
    )?;
    ProcessInfo::decode(&bytes)
}
pub fn process_kill(pid: u64) -> Result<(), Error> {
    call(Call::ProcessKill, [pid, 0, 0, 0, 0]).map(|_| ())
}
pub fn process_reap(pid: u64) -> Result<(), Error> {
    call(Call::ProcessReap, [pid, 0, 0, 0, 0]).map(|_| ())
}
pub fn memory_stats() -> Result<[u64; 4], Error> {
    let mut bytes = [0; 32];
    call(Call::MemoryStats, [bytes.as_mut_ptr() as u64, 32, 0, 0, 0])?;
    Ok(core::array::from_fn(|i| {
        u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap())
    }))
}
/// Copies bundled program `index` (0 is the multi-call command binary, 1 and up the
/// desktop applications) into `bytes`; `Invalid` when there is no such program (an
/// older boot menu entry without the application modules).
pub fn boot_program_at(index: usize, bytes: &mut [u8]) -> Result<usize, Error> {
    call(
        Call::BootProgram,
        [
            bytes.as_mut_ptr() as u64,
            bytes.len() as u64,
            index as u64,
            0,
            0,
        ],
    )
    .map(|r| r.0)
}
pub fn boot_program(bytes: &mut [u8]) -> Result<usize, Error> {
    boot_program_at(0, bytes)
}
pub fn system_control(action: u64) -> Result<(), Error> {
    call(Call::SystemControl, [action, 0, 0, 0, 0]).map(|_| ())
}
/// Process-manager only. Copies kernel and service log text starting at absolute
/// `offset` into `buffer` (at most 1024 bytes). Returns the offset to continue
/// from and the number of bytes copied.
pub fn log_read(offset: u64, buffer: &mut [u8]) -> Result<(u64, usize), Error> {
    if buffer.len() > 1024 {
        return Err(Error::TooLarge);
    }
    let mut bytes = [0; 1032];
    let (total, _) = call(
        Call::LogRead,
        [
            bytes.as_mut_ptr() as u64,
            (8 + buffer.len()) as u64,
            offset,
            0,
            0,
        ],
    )?;
    let count = total.checked_sub(8).ok_or(Error::Invalid)?;
    buffer[..count].copy_from_slice(&bytes[8..8 + count]);
    Ok((u64::from_le_bytes(bytes[..8].try_into().unwrap()), count))
}

pub fn reply_live(token: usize, bytes: &[u8]) -> Result<(), Error> {
    match reply(token, bytes) {
        Err(Error::Invalid) => debug_write("IPC reply abandoned: client exited"),
        result => result,
    }
}

pub fn exit(status: i64) -> ! {
    let _ = call(Call::Exit, [status as u64, 0, 0, 0, 0]);
    loop {
        core::hint::spin_loop();
    }
}
