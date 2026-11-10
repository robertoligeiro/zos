#![no_std]
#[cfg(test)]
extern crate std;

pub mod client;
pub mod parser;

use zos_storage::{vfs, Error, Result};
pub const PROCMGR: usize = 7;
pub const MANAGER_TID: usize = 8;
pub const PROGRAMS: [&[u8]; 10] = [
    b"echo", b"cat", b"cp", b"true", b"false", b"spin", b"block", b"fault", b"leak", b"uname",
];

pub fn path(cwd: &[u8], input: &[u8]) -> Result<vfs::Path> {
    if input.starts_with(b"/") {
        return vfs::normalize(input);
    }
    let mut bytes = [0; vfs::PATH];
    let separator = usize::from(cwd != b"/");
    let length = cwd.len() + separator + input.len();
    if length > bytes.len() {
        return Err(Error::TooLarge);
    }
    bytes[..cwd.len()].copy_from_slice(cwd);
    if separator != 0 {
        bytes[cwd.len()] = b'/';
    }
    bytes[cwd.len() + separator..length].copy_from_slice(input);
    vfs::normalize(&bytes[..length])
}

pub fn write_all(fd: u64, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let count =
            zos_storage::protocol::write(fd, &bytes[..bytes.len().min(zos_storage::PAYLOAD)])?;
        if count == 0 || count > bytes.len() {
            return Err(Error::Io);
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

pub fn console(bytes: &[u8]) {
    zos_console::output(bytes);
    for part in bytes.chunks(48) {
        let mut line = [0; 64];
        line[..11].copy_from_slice(b"SHELL OUT: ");
        for (to, &from) in line[11..].iter_mut().zip(part) {
            *to = if from == b'\n' || (32..=126).contains(&from) {
                from
            } else {
                b'.'
            };
        }
        zos_libos::debug_write(core::str::from_utf8(&line[..11 + part.len()]).unwrap()).unwrap();
    }
}

pub fn fail(info: &core::panic::PanicInfo) -> ! {
    zos_libos::debug_write("SESSION SERVICE FAILURE").unwrap();
    zos_console::log(format_args!(
        "SESSION failure at line {}",
        info.location().map_or(0, |l| l.line())
    ));
    zos_libos::exit(5000 + info.location().map_or(0, |l| l.line() as i64))
}

pub fn kernel(error: zos_libos::Error) -> Error {
    match error {
        zos_libos::Error::Denied => Error::Denied,
        zos_libos::Error::Invalid | zos_libos::Error::BadAddress => Error::Invalid,
        zos_libos::Error::TooLarge => Error::TooLarge,
        zos_libos::Error::Limit | zos_libos::Error::NoMemory => Error::NoSpace,
        _ => Error::Io,
    }
}
