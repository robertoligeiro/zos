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
pub extern "C" fn _start(role: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(INPUT));
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(os::io_port_grant(0x80, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x60, 2), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(CONSOLE, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(INPUT, 2), Err(os::Error::Denied));
    if role & 16 != 0 {
        diagnostic_input();
    }
    if role & 8 != 0 {
        usb_input(role & 1 != 0);
    }
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

fn diagnostic_input() -> ! {
    use zos_console::usb::{Controller, Error, Event};
    diagnostic(format_args!("ZOS USB DIAGNOSTIC: storage writes disabled"));
    os::io_port_grant(0x60, 1).unwrap();
    os::io_port_grant(0x64, 1).unwrap();
    let mut legacy_bus = LegacyBus;
    let mut legacy = match ps2::initialize(&mut legacy_bus) {
        Ok(devices) => {
            diagnostic(format_args!(
                "INTEGRATED PS2: keyboard={:?} mouse={:?}",
                devices.keyboard, devices.mouse
            ));
            Some(devices)
        }
        Err(error) => {
            diagnostic(format_args!(
                "INTEGRATED PS2 UNAVAILABLE: {error:?}; I2C/SPI HID not implemented"
            ));
            None
        }
    };
    let mut keyboard = Keyboard::default();
    let mut mouse = Mouse::default();
    let result = Controller::diagnostic();
    let mut controller = match result {
        Ok(controller) => controller,
        Err(error) => {
            if let Error::DmaAddress(physical) = error {
                diagnostic(format_args!(
                    "XHCI REJECT: DMA page {physical:#x} exceeds 32-bit range"
                ));
            }
            diagnostic(format_args!(
                "DIAGNOSTIC FAILED: {error:?}; photograph this screen"
            ));
            os::exit(1)
        }
    };
    os::irq_attach(INPUT, 0).unwrap();
    diagnostic(format_args!(
        "DIAGNOSTIC READY: type keys / move mouse; no shell"
    ));
    let mut bytes = [0; 64];
    loop {
        let (token, length) = os::receive(INPUT, &mut bytes).unwrap();
        if token != 0 {
            os::reply_live(token, &[4]).unwrap();
            continue;
        }
        assert_eq!(length, 8);
        if let Some(devices) = &legacy {
            let result = ps2::drain(&mut legacy_bus, |auxiliary, byte| {
                if auxiliary {
                    if devices.mouse.is_ok() {
                        if let Some(motion) = mouse.feed(byte) {
                            let (x, y) = (motion.dx.to_le_bytes(), motion.dy.to_le_bytes());
                            os::send(
                                CONSOLE,
                                &[POINTER, x[0], x[1], y[0], y[1], motion.buttons],
                                &mut [],
                            )
                            .unwrap();
                        }
                    }
                } else if devices.keyboard.is_ok() {
                    if let Some(key) = keyboard.feed(byte) {
                        diagnostic_key(key);
                    }
                }
            });
            if let Err(error) = result {
                diagnostic(format_args!("INTEGRATED PS2 OFFLINE: {error:?}"));
                legacy = None;
            }
        }
        if let Err(error) = controller.poll() {
            diagnostic(format_args!(
                "DIAGNOSTIC OFFLINE: {error:?}; restart required"
            ));
            os::exit(2)
        }
        while let Some(event) = controller.event() {
            match event {
                Event::Key(key) => {
                    diagnostic_key(key);
                }
                Event::Pointer { dx, dy, buttons } => {
                    let (x, y) = (dx.to_le_bytes(), dy.to_le_bytes());
                    os::send(
                        CONSOLE,
                        &[POINTER, x[0], x[1], y[0], y[1], buttons],
                        &mut [],
                    )
                    .unwrap();
                }
            }
            controller.consume_event();
        }
    }
}

struct LegacyBus;
impl ps2::Bus for LegacyBus {
    fn read(&mut self, port: u16) -> u8 {
        read(port)
    }
    fn write(&mut self, port: u16, value: u8) {
        // SAFETY: diagnostic input holds grants for the two i8042 ports only.
        unsafe {
            core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
        }
    }
    fn ticks(&self) -> u64 {
        os::usb_ticks().unwrap()
    }
}

fn diagnostic_key(key: keyboard::Key) {
    use keyboard::Key;
    match key {
        Key::Character(byte) => output(&[byte]),
        Key::Enter => output(b"\n"),
        Key::Clear => os::send(CONSOLE, &[CLEAR], &mut []).map(|_| ()).unwrap(),
        _ => (),
    }
    log(format_args!("DIAGNOSTIC KEY: {key:?}"));
}

fn usb_input(test: bool) -> ! {
    use zos_console::usb::{Controller, Error, Event};
    assert_eq!(os::io_port_grant(0x60, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x64, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x170, 8), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(INPUT, 1), Err(os::Error::Denied));
    assert_eq!(os::irq_attach(INPUT, 12), Err(os::Error::Denied));
    let mut controller = Controller::new().unwrap_or_else(|error| {
        log(format_args!("USB INITIALIZATION FAILURE: {error:?}"));
        panic!("USB initialization failed");
    });
    assert_eq!(os::usb_controller().unwrap_err(), os::Error::Denied);
    if test {
        let (bad_address, _) = unsafe {
            // SAFETY: intentionally invalid output must be rejected before allocation.
            os::raw(os::Call::UsbDmaPage as u64, [u64::MAX, 16, 0, 0, 0])
        };
        assert_eq!(bad_address, -(os::Error::BadAddress as i64));
        for _ in 0..os::USB_DMA_PAGES {
            match os::usb_dma_page() {
                Ok(_) => (),
                Err(error) => {
                    assert_eq!(error, os::Error::Limit);
                    break;
                }
            }
        }
        assert_eq!(os::usb_dma_page().unwrap_err(), os::Error::Limit);
        assert_eq!(os::usb_enable_dma(), Err(os::Error::Denied));
        assert_eq!(
            os::map_memory(0x2800_0000, true, false),
            Err(os::Error::Invalid)
        );
        let (nx, _) = unsafe {
            // SAFETY: an NX device address is a deliberately invalid thread entry.
            os::raw(os::Call::ThreadCreate as u64, [0x2800_0000, 0, 1, 0, 0])
        };
        assert_eq!(nx, -(os::Error::BadAddress as i64));
        controller.test_rings().unwrap();
        os::debug_write("USB DMA CAPABILITY PASS: bad pointer, quota, NX MMIO").unwrap();
    }
    os::irq_attach(INPUT, 0).unwrap();
    os::debug_write("USB CAPABILITIES PASS: controller/DMA only; no PS2/ATA ports").unwrap();
    let mut ready = false;
    let mut backpressure = false;
    let mut request = [0; os::MAX_STORAGE_MESSAGE];
    let mut reply = [0; 513];
    loop {
        let (token, length) = os::receive(INPUT, &mut request).unwrap();
        if token == 0 {
            assert_eq!(length, 8);
            assert_eq!(u64::from_le_bytes(request[..8].try_into().unwrap()), 0);
        } else if token & 0xffff == TTY && &request[..length] == b"ready" && !ready {
            ready = true;
            os::reply(token, &[]).unwrap();
        } else {
            let result = if token & 0xffff == 4 {
                controller.disk_request(&request[..length], (&mut reply[1..]).try_into().unwrap())
            } else {
                reply[0] = 7;
                os::reply_live(token, &reply[..1]).unwrap();
                continue;
            };
            let count = match result {
                Ok(count) => {
                    reply[0] = 0;
                    count + 1
                }
                Err(error) => {
                    reply[0] = if error == Error::Invalid { 1 } else { 10 };
                    1
                }
            };
            os::reply_live(token, &reply[..count]).unwrap();
        }
        controller.poll().unwrap_or_else(|error| {
            log(format_args!("USB RUNTIME FAILURE: {error:?}"));
            panic!("USB controller failed");
        });
        while let Some(event) = controller.event() {
            if !ready {
                controller.consume_event();
                continue;
            }
            let sent = match event {
                Event::Key(key) => os::send_ready(TTY, &key.encode(), &mut []),
                Event::Pointer { dx, dy, buttons } => {
                    let (x, y) = (dx.to_le_bytes(), dy.to_le_bytes());
                    os::send_ready(
                        CONSOLE,
                        &[POINTER, x[0], x[1], y[0], y[1], buttons],
                        &mut [],
                    )
                }
            };
            match sent {
                Ok(_) => controller.consume_event(),
                Err(os::Error::Busy) => {
                    if test && !backpressure && matches!(event, Event::Key(_)) {
                        os::debug_write("USB INPUT BACKPRESSURE PASS: queued HID, live storage")
                            .unwrap();
                        backpressure = true;
                    }
                    break;
                }
                Err(error) => panic!("USB input forwarding failed: {error:?}"),
            }
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let line = info.location().map_or(0, |location| location.line());
    log(format_args!("INPUT SERVICE FAILURE: line={line}"));
    fail(info)
}
