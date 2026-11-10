#![no_std]
#![no_main]

use zos_libos as os;

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let channel = os::channel_create().unwrap();
    assert_eq!(channel, 1);
    os::io_port_grant(0x80, 1).unwrap();
    // SAFETY: the kernel granted exactly this harmless POST/delay port.
    unsafe { core::arch::asm!("out dx, al", in("dx") 0x80_u16, in("al") 0_u8) };
    os::irq_attach(channel, 0).unwrap();
    let mut buffer = [0; os::MAX_MESSAGE];
    let mut requests = 0;
    let mut interrupts = 0;
    loop {
        let (token, length) = os::receive(channel, &mut buffer).unwrap();
        if token == 0 {
            assert_eq!(length, 8);
            interrupts += 1;
            continue;
        }
        if requests == 0 {
            assert_eq!(&buffer[..length], b"start");
            os::thread_create(worker, 0, 20).unwrap();
        } else {
            assert_eq!(&buffer[..length], b"shutdown");
            while interrupts == 0 {
                let (irq_token, _) = os::receive(channel, &mut buffer).unwrap();
                assert_eq!(irq_token, 0);
                interrupts += 1;
            }
        }
        os::reply(token, b"ok").unwrap();
        requests += 1;
        if requests == 2 {
            os::debug_write("server: IRQ notification and port grant verified").unwrap();
            loop {
                let (token, length) = os::receive(channel, &mut buffer).unwrap();
                if token == 0 {
                    continue;
                }
                assert_eq!(&buffer[..length], b"exit");
                break;
            }
            // Deliberately exit while a client awaits a reply.
            os::exit(0);
        }
    }
}

extern "C" fn worker(_: usize) -> ! {
    let channel = os::channel_create().unwrap();
    assert_eq!(channel, 3);
    let mut acknowledgment = [0; 8];
    assert_eq!(os::send(2, b"ready", &mut acknowledgment), Ok(2));
    assert_eq!(os::receive(channel, &mut [0; 1]), Err(os::Error::TooLarge));
    let mut message = [0; os::MAX_MESSAGE];
    let (token, length) = os::receive(channel, &mut message).unwrap();
    assert_eq!(&message[..length], b"ZOS echo");
    assert_eq!(
        os::reply(token, &[0; os::MAX_MESSAGE]),
        Err(os::Error::TooLarge)
    );
    os::reply(token, &message[..length]).unwrap();
    assert_eq!(os::reply(token, b"stale"), Err(os::Error::Invalid));
    os::debug_write("worker: receive/reply and stale token rejection verified").unwrap();
    // The client queues another request before this lower-priority thread exits.
    os::exit(0)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = os::debug_write("SERVER ASSERTION FAILED");
    os::exit(1000 + info.location().map_or(0, |location| location.line() as i64))
}
