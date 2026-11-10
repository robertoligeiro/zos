#![no_std]
#![no_main]

#[no_mangle]
pub extern "C" fn _start(role: usize) -> ! {
    // SAFETY: each disposable process deliberately faults. None has access to
    // kernel memory, interrupt control, or I/O ports; the kernel must contain it.
    unsafe {
        match role {
            0 => core::ptr::write_volatile(0xffff_c000_0000_0000 as *mut u64, 0),
            1 => core::arch::asm!("cli", options(nomem, nostack)),
            2 => core::arch::asm!("ud2", options(noreturn)),
            3 => core::arch::asm!("out dx, al", in("dx") 0x80_u16, in("al") 0_u8),
            _ => core::arch::asm!(
                "mov rsp, 0x800000000000",
                "syscall",
                in("rax") zos_libos::Call::Exit as u64,
                in("rdi") 103_u64,
                options(noreturn),
            ),
        }
    }
    zos_libos::exit(102)
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    zos_libos::exit(103)
}
