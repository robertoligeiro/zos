#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![deny(unsafe_op_in_unsafe_fn)]

mod boot;
mod framebuffer;
mod interrupts;
mod serial;

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::{hlt, interrupts as cpu_interrupts};

static TICKS: AtomicU64 = AtomicU64::new(0);

#[macro_export]
macro_rules! logln {
    () => { $crate::serial::print(format_args!("\n")) };
    ($($arg:tt)*) => { $crate::serial::print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[cfg(any(
    all(feature = "test-boot", feature = "test-divide"),
    all(feature = "test-boot", feature = "test-page-fault"),
    all(feature = "test-divide", feature = "test-page-fault")
))]
compile_error!("Select only one QEMU acceptance-test feature at a time");

pub fn halt() -> ! {
    cpu_interrupts::disable();
    loop {
        hlt();
    }
}

pub fn finish_test(success: bool) -> ! {
    #[cfg(any(
        feature = "test-boot",
        feature = "test-divide",
        feature = "test-page-fault"
    ))]
    // SAFETY: this test-only QEMU device is configured at port 0xf4 by the harness.
    unsafe {
        x86_64::instructions::port::Port::<u32>::new(0xf4).write(if success {
            0x10
        } else {
            0x11
        });
    }
    let _ = success;
    halt()
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    cpu_interrupts::disable();
    serial::init();
    logln!("ZOS - Zombie OS");
    logln!("ZOS booting");
    assert!(boot::BASE_REVISION.is_supported(), "Unsupported Limine revision");
    framebuffer::init();
    logln!("ZOS - Zombie OS");
    logln!("ZOS booting");
    boot::report();
    interrupts::init();
    logln!("GDT/TSS/IDT ready");
    interrupts::start_timer();
    logln!("PIT ready: 100 Hz");
    cpu_interrupts::enable();

    while TICKS.load(Ordering::Relaxed) < 100 {
        hlt();
    }
    logln!("TIMER PASS: ticks={}", TICKS.load(Ordering::Relaxed));

    #[cfg(feature = "test-divide")]
    {
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

    #[cfg(feature = "test-page-fault")]
    {
        logln!("TEST: deliberate page fault");
        // SAFETY: only this test image dereferences an unmapped canonical user address.
        unsafe {
            core::ptr::read_volatile(0x0000_4000_0000_0000 as *const u64);
        }
        panic!("Page-fault exception did not fire");
    }

    #[cfg(feature = "test-boot")]
    {
        logln!("BOOT ACCEPTANCE PASS");
        finish_test(true);
    }

    #[cfg(not(any(
        feature = "test-boot",
        feature = "test-divide",
        feature = "test-page-fault"
    )))]
    {
        logln!("ZOS ready: boot checkpoint; no user-space shell yet.");
        loop {
            hlt();
        }
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial::emergency(format_args!("KERNEL PANIC: {info}\n"));
    finish_test(false)
}
