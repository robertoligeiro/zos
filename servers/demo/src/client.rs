#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU64, Ordering};
use zos_libos as os;

static NOISE_STARTED: AtomicU64 = AtomicU64::new(0);
static FIRST: AtomicU64 = AtomicU64::new(0);
static SECOND: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let channel = os::channel_create().unwrap();
    assert_eq!(channel, 2);
    assert_eq!(os::irq_attach(channel, 0), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x80, 1), Err(os::Error::Denied));
    assert_eq!(os::send(9999, b"bad", &mut []), Err(os::Error::Invalid));
    assert_eq!(os::receive(1, &mut [0; 8]), Err(os::Error::Denied));
    assert_eq!(
        os::send(channel, b"cycle", &mut []),
        Err(os::Error::Deadlock)
    );
    assert_eq!(os::reply(1, b"forged"), Err(os::Error::Invalid));
    assert_eq!(os::send(1, &[0; 65], &mut []), Err(os::Error::TooLarge));
    assert_eq!(os::thread_create(noise, 0, 0), Err(os::Error::Denied));
    // SAFETY: these deliberately invalid pointers are never dereferenced by
    // this process; the kernel must reject them before copying or blocking.
    unsafe {
        assert_eq!(
            os::raw(os::Call::Send as u64, [1, 0, 8, 0, 0]).0,
            -(os::Error::BadAddress as i64)
        );
        assert_eq!(
            os::raw(os::Call::Send as u64, [1, u64::MAX - 3, 8, 0, 0]).0,
            -(os::Error::BadAddress as i64)
        );
        assert_eq!(os::raw(999, [0; 5]).0, -(os::Error::Invalid as i64));
        assert_eq!(
            os::raw(
                os::Call::Send as u64,
                [1, b"x".as_ptr() as u64, 1, b"read-only".as_ptr() as u64, 1]
            )
            .0,
            -(os::Error::BadAddress as i64)
        );
        assert_eq!(
            os::raw(
                os::Call::ThreadCreate as u64,
                [0xffff_c000_0000_0000, 0, 10, 0, 0]
            )
            .0,
            -(os::Error::BadAddress as i64)
        );
    }
    assert_eq!(
        os::map_memory(0xffff_c000_0000_0000, true, false),
        Err(os::Error::Invalid)
    );
    assert_eq!(
        os::map_memory(0x1000_0000, true, true),
        Err(os::Error::Invalid)
    );
    os::map_memory(0x1000_0000, true, false).unwrap();
    os::map_memory(0x1000_1000, true, false).unwrap();
    // SAFETY: MapMemory just created this writable page in our own address space.
    unsafe { core::ptr::write_volatile(0x1000_0000 as *mut u64, 0x1234) };
    assert_eq!(
        os::map_memory(0x1000_0000, true, false),
        Err(os::Error::Invalid)
    );
    os::debug_write("client: syscall validation and denied capabilities verified").unwrap();
    // SAFETY: both pages are ours, mapped writable, and have no other references.
    // The reply spans the boundary so kernel copy-out must handle both frames.
    let reply = unsafe { core::slice::from_raw_parts_mut(0x1000_0ffc as *mut u8, 16) };
    assert!(reply.iter().all(|&byte| byte == 0));
    reply[..8].copy_from_slice(b"boundary");
    os::debug_write(core::str::from_utf8(&reply[..8]).unwrap()).unwrap();
    assert_eq!(os::send(1, b"start", reply), Ok(2));
    let mut ready = [0; 16];
    let (token, length) = os::receive(channel, &mut ready).unwrap();
    assert_eq!(&ready[..length], b"ready");
    os::reply(token, b"ok").unwrap();
    os::thread_create(noise, 0, 10).unwrap();
    assert_eq!(os::send(3, b"ZOS echo", reply), Ok(8));
    assert_eq!(&reply[..8], b"ZOS echo");
    assert_eq!(
        NOISE_STARTED.load(Ordering::SeqCst),
        0,
        "Priority inversion"
    );
    os::debug_write("client: cross-process echo and priority inheritance verified").unwrap();
    assert_eq!(os::send(3, b"close", &mut []), Err(os::Error::Closed));
    assert_eq!(os::send(3, b"closed", &mut []), Err(os::Error::Closed));
    os::thread_create(peer, 1, 10).unwrap();
    os::thread_create(peer, 2, 10).unwrap();
    assert_eq!(os::send(1, b"shutdown", reply), Ok(2));
    assert_eq!(os::send(1, b"exit", &mut []), Err(os::Error::Closed));
    os::debug_write("client: cross-page buffers and channel closure verified").unwrap();
    os::exit(0)
}

extern "C" fn noise(_: usize) -> ! {
    NOISE_STARTED.store(1, Ordering::SeqCst);
    os::exit(0)
}

extern "C" fn peer(which: usize) -> ! {
    let (own, other) = if which == 1 {
        (&FIRST, &SECOND)
    } else {
        (&SECOND, &FIRST)
    };
    let pattern = 0x1234_5678_9abc_0000_u64 + which as u64;
    let observed: u64;
    // SAFETY: both flags are aligned, shared process-local atomic storage.
    // The syscall-free loop requires timer preemption to start the other peer.
    // XMM0 must retain this thread's distinct value across that context switch.
    unsafe {
        core::arch::asm!(
            "movq xmm0, {pattern}",
            "mov qword ptr [{own}], 1",
            "2:",
            "cmp qword ptr [{other}], 0",
            "je 2b",
            "movq {observed}, xmm0",
            pattern = in(reg) pattern,
            own = in(reg) own.as_ptr(),
            other = in(reg) other.as_ptr(),
            observed = lateout(reg) observed,
            out("xmm0") _,
            options(nostack),
        );
    }
    assert_eq!(observed, pattern, "SSE state was not preserved");
    os::debug_write("peer: timer preemption and SSE context verified").unwrap();
    os::exit(0)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    let _ = os::debug_write("CLIENT ASSERTION FAILED");
    os::exit(2000 + info.location().map_or(0, |location| location.line() as i64))
}
