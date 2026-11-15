#![no_std]

#[cfg(test)]
extern crate std;

pub mod editor;
pub mod i2c;
pub mod keyboard;
pub mod mouse;
pub mod ps2;
pub mod splash;
pub mod terminal;
pub mod usb;

pub const CONSOLE: usize = 1;
pub const INPUT: usize = 2;
pub const TTY: usize = 3;
pub const WRITE: u8 = 0;
pub const COLORS: u8 = 1;
pub const CLEAR: u8 = 2;
pub const LINE: u8 = 3;
pub const POINTER: u8 = 4;
pub const SCROLLS: u8 = 5;
/// `[ALT, key]`: an Alt+key shortcut (see `keyboard::Key::Alt`), sent by the input
/// service to the console channel for the window manager. The text console ignores it.
pub const ALT: u8 = 6;

pub fn output(text: &[u8]) {
    for part in text.chunks(63) {
        let mut message = [WRITE; 64];
        message[1..part.len() + 1].copy_from_slice(part);
        zos_libos::send(CONSOLE, &message[..part.len() + 1], &mut []).unwrap();
    }
}

pub fn fail(info: &core::panic::PanicInfo) -> ! {
    zos_libos::debug_write("CONSOLE SERVICE FAILURE").unwrap();
    zos_libos::exit(3000 + info.location().map_or(0, |location| location.line() as i64))
}

pub fn log(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    struct Buffer {
        bytes: [u8; 64],
        length: usize,
    }

    impl Write for Buffer {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            let end = self.length + text.len();
            if end > self.bytes.len() {
                return Err(core::fmt::Error);
            }
            self.bytes[self.length..end].copy_from_slice(text.as_bytes());
            self.length = end;
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: [0; 64],
        length: 0,
    };
    buffer.write_fmt(args).unwrap();
    zos_libos::debug_write(core::str::from_utf8(&buffer.bytes[..buffer.length]).unwrap()).unwrap();
}

pub fn diagnostic(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    struct Writer {
        bytes: [u8; 128],
        length: usize,
    }
    impl Write for Writer {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            if self.length + text.len() > self.bytes.len() {
                return Err(core::fmt::Error);
            }
            self.bytes[self.length..self.length + text.len()].copy_from_slice(text.as_bytes());
            self.length += text.len();
            Ok(())
        }
    }
    let mut buffer = Writer {
        bytes: [0; 128],
        length: 0,
    };
    buffer.write_fmt(args).unwrap();
    let bytes = &buffer.bytes[..buffer.length];
    output(bytes);
    output(b"\n");
    for part in bytes.chunks(64) {
        zos_libos::debug_write(core::str::from_utf8(part).unwrap()).unwrap();
    }
}
