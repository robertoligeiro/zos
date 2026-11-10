use core::arch::global_asm;
use x86_64::{
    registers::{
        control::{Cr0, Cr0Flags, Cr4, Cr4Flags},
        model_specific::{Efer, EferFlags, LStar, SFMask, Star},
        rflags::RFlags,
    },
    structures::gdt::SegmentSelector,
    VirtAddr,
};

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub kind: u64,
    pub rip: u64,
    pub cs: u64,
    pub flags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct FxState(pub [u8; 512]);

#[repr(C, align(16))]
pub struct Context {
    pub fx: FxState,
    pub frame: Frame,
}

impl Context {
    pub fn new(entry: u64, stack: u64, argument: u64) -> Self {
        let mut fx = FxState([0; 512]);
        fx.0[..2].copy_from_slice(&0x037f_u16.to_le_bytes());
        fx.0[24..28].copy_from_slice(&0x1f80_u32.to_le_bytes());
        Self {
            fx,
            frame: Frame {
                r15: 0,
                r14: 0,
                r13: 0,
                r12: 0,
                r11: 0,
                r10: 0,
                r9: 0,
                r8: 0,
                rsi: 0,
                rdi: argument,
                rbp: 0,
                rdx: 0,
                rcx: 0,
                rbx: 0,
                rax: 0,
                kind: 0,
                rip: entry,
                cs: 0x23,
                flags: 0x202,
                rsp: stack,
                ss: 0x1b,
            },
        }
    }
}

#[no_mangle]
static mut ZOS_KERNEL_RSP: u64 = 0;

extern "C" {
    pub fn zos_timer_entry();
    pub fn zos_keyboard_entry();
    pub fn zos_mouse_entry();
    fn zos_syscall_entry();
    fn zos_restore(context: *const Context) -> !;
    fn zos_idle_entry() -> !;
}

pub fn init() {
    assert_eq!(core::mem::size_of::<Frame>(), 168);
    assert_eq!(core::mem::offset_of!(Context, frame), 512);
    Star::write(
        SegmentSelector(0x23),
        SegmentSelector(0x1b),
        SegmentSelector(0x08),
        SegmentSelector(0x10),
    )
    .unwrap();
    LStar::write(VirtAddr::new(zos_syscall_entry as usize as u64));
    SFMask::write(
        RFlags::INTERRUPT_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::TRAP_FLAG
            | RFlags::ALIGNMENT_CHECK
            | RFlags::NESTED_TASK
            | RFlags::IOPL_LOW
            | RFlags::IOPL_HIGH,
    );
    // SAFETY: single-core initialization enables the architectural syscall and
    // SSE mechanisms used by every context; all other control flags are preserved.
    unsafe {
        Efer::update(|flags| flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
        Cr0::update(|flags| {
            flags.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            flags.insert(Cr0Flags::MONITOR_COPROCESSOR);
        });
        Cr4::update(|flags| {
            // FXSAVE covers x87/SSE, not AVX or other XSAVE-managed state.
            flags.remove(Cr4Flags::OSXSAVE);
            flags.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE);
        });
    }
    crate::logln!("RING3 ready: SYSCALL/SYSRET, per-thread kernel stacks, FXSAVE");
}

pub fn kernel_stack(top: u64) {
    assert!(!x86_64::instructions::interrupts::are_enabled());
    // SAFETY: interrupts stay disabled until returning to user mode. SYSCALL
    // reads this supervisor-only single-CPU slot before touching any stack.
    unsafe { core::ptr::addr_of_mut!(ZOS_KERNEL_RSP).write(top) };
}

pub unsafe fn resume(context: *const Context) -> ! {
    // SAFETY: the scheduler supplies a live, validated context after activating
    // its address space and TSS stack; assembly restores the complete user state.
    unsafe { zos_restore(context) }
}

pub fn idle() -> ! {
    // SAFETY: switches off a thread's stack before it can be reaped, onto a
    // permanent shared kernel stack. Interrupts remain disabled across the jump.
    unsafe { zos_idle_entry() }
}

global_asm!(
    r#"
    .section .bss
    .balign 16
zos_idle_stack:
    .skip 32768
zos_idle_stack_top:
zos_user_rsp:
    .quad 0
    .section .text
    .macro PUSH_REGS
    push rax
    push rbx
    push rcx
    push rdx
    push rbp
    push rdi
    push rsi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    .endm
    .macro POP_REGS
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rsi
    pop rdi
    pop rbp
    pop rdx
    pop rcx
    pop rbx
    pop rax
    .endm

    .global zos_syscall_entry
zos_syscall_entry:
    mov [rip + zos_user_rsp], rsp
    mov rsp, [rip + ZOS_KERNEL_RSP]
    push 0x1b
    push qword ptr [rip + zos_user_rsp]
    push r11
    push 0x23
    push rcx
    push 1
    PUSH_REGS
    mov edx, 1
    jmp zos_trap_common

    .global zos_timer_entry
zos_timer_entry:
    test byte ptr [rsp + 8], 3
    jz zos_boot_timer
    push 0
    PUSH_REGS
    xor edx, edx
    jmp zos_trap_common

    .macro DEVICE_IRQ name, boot, kind
    .global \name
\name:
    test byte ptr [rsp + 8], 3
    jz \boot
    push 0
    PUSH_REGS
    mov edx, \kind
    jmp zos_trap_common
    .endm
    DEVICE_IRQ zos_keyboard_entry, zos_boot_keyboard, 2
    DEVICE_IRQ zos_mouse_entry, zos_boot_mouse, 13

zos_trap_common:
    cld
    sub rsp, 520
    mov rdi, rsp
    xor eax, eax
    mov ecx, 64
    rep stosq
    fxsave64 [rsp]
    lea rdi, [rsp + 520]
    mov rsi, rsp
    call zos_trap
    mov rdi, rax

    .global zos_restore
zos_restore:
    fxrstor64 [rdi]
    lea rsp, [rdi + 512]
    cmp qword ptr [rsp + 120], 1
    je zos_sysret
    POP_REGS
    add rsp, 8
    iretq
zos_sysret:
    POP_REGS
    mov rcx, [rsp + 8]
    mov r11, [rsp + 24]
    mov rsp, [rsp + 32]
    sysretq

    .global zos_idle_entry
zos_idle_entry:
    lea rsp, [rip + zos_idle_stack_top]
    call zos_idle
    ud2
"#
);
