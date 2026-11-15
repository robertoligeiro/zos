#![no_std]
#![no_main]

use zos_console::{terminal::Terminal, *};
use zos_libos as os;

static mut TERMINAL: Terminal = Terminal::new();

#[no_mangle]
pub extern "C" fn _start(role: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(CONSOLE));
    assert_eq!(os::io_port_grant(0x60, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(CONSOLE, 1), Err(os::Error::Denied));
    // SAFETY: this deliberately invalid output buffer must be rejected before
    // mapping any device pages, without dereferencing the supplied address.
    let (result, _) = unsafe { os::raw(os::Call::FramebufferMap as u64, [u64::MAX, 48, 0, 0, 0]) };
    assert_eq!(result, -(os::Error::BadAddress as i64));
    let fb = os::framebuffer_map().unwrap();
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(
        os::map_memory(0x2000_0000, true, false),
        Err(os::Error::Invalid)
    );
    // SAFETY: this process exclusively owns the mapped RGB32 boot framebuffer
    // and its static terminal; no other thread is created in this process.
    let (terminal, pixels) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(TERMINAL),
            core::slice::from_raw_parts_mut(fb.address as *mut u32, fb.length / 4),
        )
    };
    terminal.configure(fb.width, fb.height);
    // Boot screen: the logo stays up until the first real message arrives, so the
    // exercise and header below are only drawn when it is dismissed.
    let mut splash_up = zos_console::splash::draw(pixels, fb);
    terminal.colors(0xffcc66, 0x081008);
    terminal.write(b"backspace: ab\x08c\n");
    for _ in 0..55 {
        terminal.write(b"Console scrolling exercise\n");
    }
    assert!(terminal.scrolls > 0);
    os::debug_write("CONSOLE PASS: cursor, colors, backspace, scrolling").unwrap();
    terminal.colors(0x90ee90, 0x081008);
    terminal.clear();
    terminal.write(b"ZOS - Zombie OS\nRing-3 console and input\n\n");
    terminal.colors(0xffcc66, 0x081008);
    if role & 16 != 0 {
        terminal.write(b"Intel / QEMU xHCI diagnostic - storage READ ONLY\n");
        terminal.write(b"No filesystem or shell. Unsupported devices are reported.\n\n");
    } else {
        terminal.write(b"Keyboard and mouse use isolated user-space drivers.\n");
        terminal.colors(0x90ee90, 0x081008);
        terminal.write(b"Edit: arrows, Home/End, Backspace/Delete; Up/Down: history\n");
        terminal.write(b"Ctrl+A/E: start/end; Ctrl+L: clear; mouse: move/click crosshair\n\n");
    }
    if !splash_up {
        terminal.render(pixels, fb);
    }
    let mut bytes = [0; 128];
    loop {
        let (token, length) = os::receive(CONSOLE, &mut bytes).unwrap();
        assert!(token != 0 && length > 0);
        let message = &bytes[..length];
        // Anything but pointer motion or a scroll-count query is real output (the
        // shell banner, a diagnostic, an error): replace the logo with the text.
        if splash_up && !matches!(message, [POINTER, ..] | [SCROLLS]) {
            splash_up = false;
            terminal.invalidate();
        }
        match message {
            [WRITE, text @ ..] => terminal.write(text),
            [COLORS, fr, fg, fb, br, bg, bb] => terminal.colors(
                u32::from_be_bytes([0, *fr, *fg, *fb]),
                u32::from_be_bytes([0, *br, *bg, *bb]),
            ),
            [CLEAR] => terminal.clear(),
            [LINE, cursor, text @ ..] => terminal.line(text, *cursor as usize),
            [POINTER, x0, x1, y0, y1, buttons] => {
                terminal.pointer(
                    i16::from_le_bytes([*x0, *x1]),
                    i16::from_le_bytes([*y0, *y1]),
                    *buttons,
                );
                log(format_args!(
                    "MOUSE: x={} y={} buttons={}",
                    terminal.mouse_x, terminal.mouse_y, buttons
                ));
                if splash_up {
                    // The pointer does not dismiss the logo, and is not drawn over it.
                    os::reply_live(token, &[]).unwrap();
                    continue;
                }
            }
            [SCROLLS] => {
                os::reply(token, &(terminal.scrolls as u64).to_le_bytes()).unwrap();
                continue;
            }
            _ => panic!("Invalid console protocol"),
        }
        terminal.render(pixels, fb);
        os::reply_live(token, &[]).unwrap();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
