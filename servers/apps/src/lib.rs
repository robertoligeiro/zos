//! ZOS desktop applications. The logic of each (page layout, arithmetic) is plain
//! host-tested code here; the binaries only connect it to the desktop protocol.
#![no_std]

#[cfg(test)]
extern crate std;

pub mod calc;
pub mod html;
pub mod nav;

/// A readable debug line (at most 64 bytes) for the serial log.
pub fn log(args: core::fmt::Arguments<'_>) {
    use core::fmt::Write;
    // Truncates instead of failing: an over-long line must never take a program down.
    let mut line = zos_console::i2c::Line::new();
    let _ = line.write_fmt(args);
    let _ = zos_libos::debug_write(line.as_str());
}

pub fn fail(info: &core::panic::PanicInfo) -> ! {
    let line = info.location().map_or(0, |location| location.line());
    zos_console::log(format_args!("APP failure at line {line}"));
    zos_libos::exit(6000 + line as i64)
}
