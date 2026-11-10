#![no_std]

pub const MAX_MESSAGE: usize = 64;

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

pub fn exit(status: i64) -> ! {
    let _ = call(Call::Exit, [status as u64, 0, 0, 0, 0]);
    loop {
        core::hint::spin_loop();
    }
}
