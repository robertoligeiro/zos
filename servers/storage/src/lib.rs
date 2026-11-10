#![no_std]

pub mod fs;
pub mod protocol;
pub mod vfs;

pub const BLKDEV: usize = 4;
pub const FILESYSTEM: usize = 5;
pub const VFS: usize = 6;
pub const SECTOR: usize = 512;
pub const SECTORS: u32 = 32768;
pub const BLOCK: usize = 4096;
pub const BLOCKS: usize = SECTORS as usize / 8;
pub const INODES: usize = 64;
pub const NAME: usize = 24;
pub const DIRECT: usize = 16;
pub const MAX_FILE: usize = BLOCK * DIRECT;
pub const PAYLOAD: usize = 1000;

pub fn fail(info: &core::panic::PanicInfo) -> ! {
    zos_libos::debug_write("STORAGE SERVICE FAILURE").unwrap();
    let line = info.location().map_or(0, |location| location.line());
    zos_console::log(format_args!("STORAGE failure at line {line}"));
    zos_libos::exit(4000 + line as i64)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Error {
    Invalid = 1,
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NoSpace,
    Denied,
    BadFd,
    NotEmpty,
    Io,
    Corrupt,
    TooLarge,
}
pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub fn decode(byte: u8) -> Result<Self> {
        Ok(match byte {
            1 => Self::Invalid,
            2 => Self::NotFound,
            3 => Self::Exists,
            4 => Self::NotDir,
            5 => Self::IsDir,
            6 => Self::NoSpace,
            7 => Self::Denied,
            8 => Self::BadFd,
            9 => Self::NotEmpty,
            10 => Self::Io,
            11 => Self::Corrupt,
            12 => Self::TooLarge,
            _ => return Err(Self::Corrupt),
        })
    }
}

pub trait Disk {
    fn read(&mut self, sector: u32, bytes: &mut [u8; SECTOR]) -> Result<()>;
    fn write(&mut self, sector: u32, bytes: &[u8; SECTOR]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

pub struct BlockClient;
impl Disk for BlockClient {
    fn read(&mut self, sector: u32, bytes: &mut [u8; SECTOR]) -> Result<()> {
        let mut request = [0; 5];
        request[1..].copy_from_slice(&sector.to_le_bytes());
        let mut reply = [0; SECTOR + 1];
        let count = protocol::rpc(BLKDEV, &request, &mut reply)?;
        if count != SECTOR {
            return Err(Error::Corrupt);
        }
        bytes.copy_from_slice(&reply[1..]);
        Ok(())
    }
    fn write(&mut self, sector: u32, bytes: &[u8; SECTOR]) -> Result<()> {
        let mut request = [1; SECTOR + 5];
        request[1..5].copy_from_slice(&sector.to_le_bytes());
        request[5..].copy_from_slice(bytes);
        protocol::rpc(BLKDEV, &request, &mut [0]).map(|_| ())
    }
    fn flush(&mut self) -> Result<()> {
        protocol::rpc(BLKDEV, &[2], &mut [0]).map(|_| ())
    }
}
