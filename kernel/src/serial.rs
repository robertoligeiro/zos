use core::fmt::{self, Write};
use x86_64::instructions::{interrupts, port::Port};

const COM1: u16 = 0x3f8;

pub fn init() {
    // SAFETY: this single-core kernel owns COM1; interrupts remain disabled.
    unsafe {
        Port::<u8>::new(COM1 + 1).write(0x00);
        Port::<u8>::new(COM1 + 3).write(0x80);
        Port::<u8>::new(COM1).write(0x01);
        Port::<u8>::new(COM1 + 1).write(0x00);
        Port::<u8>::new(COM1 + 3).write(0x03);
        Port::<u8>::new(COM1 + 2).write(0xc7);
        Port::<u8>::new(COM1 + 4).write(0x0b);
    }
}

struct Serial;

impl Write for Serial {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

impl Serial {
    fn write_byte(&mut self, byte: u8) {
        // SAFETY: COM1 is initialized before logging; this writer never holds a lock.
        unsafe {
            while Port::<u8>::new(COM1 + 5).read() & 0x20 == 0 {
                core::hint::spin_loop();
            }
            Port::<u8>::new(COM1).write(byte);
        }
    }
}

pub fn print(args: fmt::Arguments<'_>) {
    interrupts::without_interrupts(|| {
        Serial.write_fmt(args).expect("Serial formatting failed");
        crate::framebuffer::print(args);
    });
}

pub fn emergency(args: fmt::Arguments<'_>) {
    interrupts::disable();
    // A CPU fault can interrupt a locked framebuffer writer; serial has no lock.
    Serial.write_fmt(args).expect("Serial formatting failed");
    crate::framebuffer::print(args);
}
