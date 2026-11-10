#![no_std]
#![no_main]

use zos_console::{keyboard::Keyboard, mouse::Mouse, *};
use zos_libos as os;

fn read(port: u16) -> u8 {
    let value;
    // SAFETY: only this server can request and receive these two port grants.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}
fn write(port: u16, value: u8) {
    for _ in 0..1_000_000 {
        if read(0x64) & 2 == 0 {
            // SAFETY: the kernel granted the exact port to this thread.
            unsafe {
                core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
            }
            return;
        }
        core::hint::spin_loop();
    }
    panic!("PS/2 input buffer timeout");
}
fn response(mouse: bool) -> u8 {
    for _ in 0..1_000_000 {
        let status = read(0x64);
        assert_eq!(status & 0xc0, 0, "PS/2 controller error");
        if status & 1 != 0 {
            assert_eq!(status & 0x20 != 0, mouse, "Unexpected PS/2 response source");
            return read(0x60);
        }
        core::hint::spin_loop();
    }
    panic!("PS/2 response timeout");
}
fn command(mouse: bool, value: u8) {
    for _ in 0..3 {
        if mouse {
            write(0x64, 0xd4);
        }
        write(0x60, value);
        match response(mouse) {
            0xfa => return,
            0xfe => (),
            _ => panic!("PS/2 command rejected"),
        }
    }
    panic!("PS/2 resend limit");
}

fn initialize() {
    write(0x64, 0xad);
    write(0x64, 0xa7);
    let mut drained = false;
    for _ in 0..256 {
        if read(0x64) & 1 == 0 {
            drained = true;
            break;
        }
        read(0x60);
    }
    assert!(drained, "PS/2 initial drain limit");
    write(0x64, 0x20);
    let configuration = response(false);
    write(0x64, 0x60);
    write(0x60, configuration & !0x73);
    write(0x64, 0xae);
    write(0x64, 0xa8);
    command(false, 0xf5);
    command(false, 0xf0);
    command(false, 1);
    command(true, 0xf5);
    command(true, 0xf6);
    command(true, 0xf2);
    assert_eq!(response(true), 0, "Three-byte PS/2 mouse required");
    write(0x64, 0x60);
    write(0x60, (configuration & !0x70) | 3);
    command(false, 0xf4);
    command(true, 0xf4);
    os::debug_write("PS2 ready: set-1 keyboard IRQ1, 3-byte mouse IRQ12").unwrap();
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(INPUT));
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(os::io_port_grant(0x80, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x60, 2), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(CONSOLE, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(INPUT, 2), Err(os::Error::Denied));
    os::io_port_grant(0x60, 1).unwrap();
    os::io_port_grant(0x64, 1).unwrap();
    os::irq_attach(INPUT, 1).unwrap();
    os::irq_attach(INPUT, 12).unwrap();
    initialize();
    let mut keyboard = Keyboard::default();
    let mut mouse = Mouse::default();
    let mut ready = false;
    let mut bytes = [0; 64];
    loop {
        let (token, length) = os::receive(INPUT, &mut bytes).unwrap();
        if token != 0 {
            assert_eq!(&bytes[..length], b"ready");
            assert!(!ready);
            ready = true;
            os::reply(token, &[]).unwrap();
            continue;
        }
        assert_eq!(length, 8);
        assert!(matches!(
            u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            1 | 12
        ));
        let mut drained = false;
        for _ in 0..256 {
            let status = read(0x64);
            assert_eq!(status & 0xc0, 0, "PS/2 runtime error");
            if status & 1 == 0 {
                drained = true;
                break;
            }
            let byte = read(0x60);
            if status & 0x20 != 0 {
                if let Some(motion) = mouse.feed(byte) {
                    if ready {
                        let x = motion.dx.to_le_bytes();
                        let y = motion.dy.to_le_bytes();
                        os::send(
                            CONSOLE,
                            &[POINTER, x[0], x[1], y[0], y[1], motion.buttons],
                            &mut [],
                        )
                        .unwrap();
                    }
                }
            } else if let Some(key) = keyboard.feed(byte) {
                if ready {
                    os::send(TTY, &key.encode(), &mut []).unwrap();
                }
            }
        }
        assert!(drained, "PS/2 runtime drain limit");
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
