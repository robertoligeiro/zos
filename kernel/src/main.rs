#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

mod boot;
mod framebuffer;
mod interrupts;
mod memory;
mod process;
mod serial;

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::{hlt, interrupts as cpu_interrupts};

static TICKS: AtomicU64 = AtomicU64::new(0);

#[macro_export]
macro_rules! logln {
    () => { $crate::serial::print(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::serial::print(format_args!("{}\n", format_args!($($arg)*))) };
}

const ACCEPTANCE_FEATURES: u8 = cfg!(feature = "test-boot") as u8
    + cfg!(feature = "test-divide") as u8
    + cfg!(feature = "test-page-fault") as u8
    + cfg!(feature = "test-memory") as u8
    + cfg!(feature = "test-memory-protection") as u8
    + cfg!(feature = "test-memory-unmap") as u8
    + cfg!(feature = "test-memory-execute") as u8
    + cfg!(feature = "test-ipc") as u8;
const _: () = match ACCEPTANCE_FEATURES {
    0 | 1 => (),
    _ => panic!("Select only one QEMU acceptance-test feature at a time"),
};

pub fn halt() -> ! {
    cpu_interrupts::disable();
    loop {
        hlt();
    }
}

pub fn finish_test(success: bool) -> ! {
    if ACCEPTANCE_FEATURES != 0 {
        // SAFETY: this test-only QEMU device is configured at port 0xf4 by the harness.
        unsafe {
            x86_64::instructions::port::Port::<u32>::new(0xf4).write(if success {
                0x10
            } else {
                0x11
            });
        }
    }
    halt()
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    cpu_interrupts::disable();
    serial::init();
    logln!("ZOS - Zombie OS");
    logln!("ZOS booting");
    assert!(
        boot::BASE_REVISION.is_supported(),
        "Unsupported Limine revision"
    );
    framebuffer::init();
    logln!("ZOS - Zombie OS");
    logln!("ZOS booting");
    boot::report();
    interrupts::init();
    logln!("GDT/TSS/IDT ready");
    memory::init();
    interrupts::start_timer();
    logln!("PIT ready: 100 Hz");
    cpu_interrupts::enable();

    while TICKS.load(Ordering::Relaxed) < 100 {
        hlt();
    }
    logln!("TIMER PASS: ticks={}", TICKS.load(Ordering::Relaxed));

    if cfg!(feature = "test-memory") {
        memory::acceptance();
    }
    if cfg!(any(
        feature = "test-memory-protection",
        feature = "test-memory-unmap",
        feature = "test-memory-execute"
    )) {
        memory::fault_acceptance();
    }

    if cfg!(feature = "test-divide") {
        logln!("TEST: deliberate divide by zero");
        // SAFETY: the installed #DE handler terminates this dedicated test image.
        unsafe {
            core::arch::asm!(
                "xor edx, edx",
                "xor eax, eax",
                "xor ecx, ecx",
                "div ecx",
                out("eax") _, out("ecx") _, out("edx") _,
                options(nomem, nostack)
            );
        }
        panic!("Divide-by-zero exception did not fire");
    }

    if cfg!(feature = "test-page-fault") {
        logln!("TEST: deliberate page fault");
        // SAFETY: only this test image dereferences an unmapped canonical user address.
        unsafe {
            core::ptr::read_volatile(0x0000_4000_0000_0000 as *const u64);
        }
        panic!("Page-fault exception did not fire");
    }

    if cfg!(feature = "test-boot") {
        logln!("BOOT ACCEPTANCE PASS");
        finish_test(true);
    }

    process::start()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("KERNEL PANIC: {info}\n"));
    finish_test(false)
}
