use core::sync::atomic::Ordering;
use spin::Lazy;
use x86_64::{
    instructions::{
        port::Port,
        segmentation::{Segment, CS, DS, ES, SS},
        tables::load_tss,
    },
    registers::control::Cr2,
    structures::{
        gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector},
        idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode},
        tss::TaskStateSegment,
    },
    VirtAddr,
};

const DOUBLE_FAULT_IST: u16 = 0;
const TIMER_VECTOR: u8 = 32;

#[repr(align(16))]
struct Stack {
    _bytes: [u8; 32 * 1024],
}
static mut DOUBLE_FAULT_STACK: Stack = Stack {
    _bytes: [0; 32 * 1024],
};
static mut PRIVILEGE_STACK: Stack = Stack {
    _bytes: [0; 32 * 1024],
};

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let double_fault = core::ptr::addr_of!(DOUBLE_FAULT_STACK) as u64;
    let privilege = core::ptr::addr_of!(PRIVILEGE_STACK) as u64;
    tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] =
        VirtAddr::new(double_fault + core::mem::size_of::<Stack>() as u64);
    tss.privilege_stack_table[0] = VirtAddr::new(privilege + core::mem::size_of::<Stack>() as u64);
    tss
});

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut table = GlobalDescriptorTable::new();
    let code = table.append(Descriptor::kernel_code_segment());
    let data = table.append(Descriptor::kernel_data_segment());
    table.append(Descriptor::user_data_segment());
    table.append(Descriptor::user_code_segment());
    let tss = table.append(Descriptor::tss_segment(&TSS));
    (table, Selectors { code, data, tss })
});

macro_rules! exception {
    ($handler:ident, $name:literal) => {
        extern "x86-interrupt" fn $handler(frame: InterruptStackFrame) {
            fatal($name, &frame, None);
        }
    };
}

macro_rules! exception_with_code {
    ($handler:ident, $name:literal) => {
        extern "x86-interrupt" fn $handler(frame: InterruptStackFrame, code: u64) {
            fatal($name, &frame, Some(code));
        }
    };
}

exception!(divide, "DIVIDE ERROR");
exception!(debug, "DEBUG");
exception!(nmi, "NMI");
exception!(breakpoint, "BREAKPOINT");
exception!(overflow, "OVERFLOW");
exception!(bound, "BOUND RANGE");
exception!(invalid_opcode, "INVALID OPCODE");
exception!(device_unavailable, "DEVICE NOT AVAILABLE");
exception!(x87, "X87 FLOATING POINT");
exception!(simd, "SIMD FLOATING POINT");
exception!(virtualization, "VIRTUALIZATION");
exception_with_code!(invalid_tss, "INVALID TSS");
exception_with_code!(segment_missing, "SEGMENT NOT PRESENT");
exception_with_code!(stack_fault, "STACK SEGMENT FAULT");
exception_with_code!(general_protection, "GENERAL PROTECTION");
exception_with_code!(alignment, "ALIGNMENT CHECK");
exception_with_code!(security, "SECURITY EXCEPTION");

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divide);
    idt.debug.set_handler_fn(debug);
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.overflow.set_handler_fn(overflow);
    idt.bound_range_exceeded.set_handler_fn(bound);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
    idt.device_not_available.set_handler_fn(device_unavailable);
    // SAFETY: IST 0 has a dedicated, static, aligned stack in our loaded TSS.
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault)
            .set_stack_index(DOUBLE_FAULT_IST);
    }
    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_missing);
    idt.stack_segment_fault.set_handler_fn(stack_fault);
    idt.general_protection_fault
        .set_handler_fn(general_protection);
    idt.page_fault.set_handler_fn(page_fault);
    idt.x87_floating_point.set_handler_fn(x87);
    idt.alignment_check.set_handler_fn(alignment);
    idt.machine_check.set_handler_fn(machine_check);
    idt.simd_floating_point.set_handler_fn(simd);
    idt.virtualization.set_handler_fn(virtualization);
    idt.security_exception.set_handler_fn(security);
    idt[TIMER_VECTOR].set_handler_fn(timer);
    idt[39].set_handler_fn(spurious_master);
    idt[47].set_handler_fn(spurious_slave);
    idt
});

pub fn init() {
    GDT.0.load();
    // SAFETY: all selectors refer to the static GDT just loaded; TSS is static.
    unsafe {
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
    IDT.load();
}

fn io_wait() {
    // SAFETY: port 0x80 is the legacy POST/delay port on the target x86 platform.
    unsafe { Port::<u8>::new(0x80).write(0) };
}

pub fn start_timer() {
    // SAFETY: interrupts are disabled; the single-core kernel owns the 8259 PICs/PIT.
    unsafe {
        for (port, byte) in [
            (0x20, 0x11),
            (0xa0, 0x11),
            (0x21, 32),
            (0xa1, 40),
            (0x21, 4),
            (0xa1, 2),
            (0x21, 1),
            (0xa1, 1),
            (0x21, 0xfe),
            (0xa1, 0xff),
        ] {
            Port::<u8>::new(port).write(byte);
            io_wait();
        }
        let divisor: u16 = 11932;
        Port::<u8>::new(0x43).write(0x36);
        Port::<u8>::new(0x40).write(divisor as u8);
        Port::<u8>::new(0x40).write((divisor >> 8) as u8);
    }
}

extern "x86-interrupt" fn timer(_: InterruptStackFrame) {
    crate::TICKS.fetch_add(1, Ordering::Relaxed);
    // SAFETY: this handler services master-PIC IRQ0 and must acknowledge it.
    unsafe { Port::<u8>::new(0x20).write(0x20) };
}

extern "x86-interrupt" fn spurious_master(_: InterruptStackFrame) {}

extern "x86-interrupt" fn spurious_slave(_: InterruptStackFrame) {
    // SAFETY: spurious IRQ15 requires an EOI only on the master's cascade IRQ.
    unsafe { Port::<u8>::new(0x20).write(0x20) };
}

fn fatal(name: &str, frame: &InterruptStackFrame, code: Option<u64>) -> ! {
    crate::serial::emergency(format_args!(
        "EXCEPTION: {name}, error={code:?}\n{frame:#?}\n"
    ));
    #[cfg(feature = "test-divide")]
    if name == "DIVIDE ERROR" {
        crate::serial::emergency(format_args!("DIVIDE ACCEPTANCE PASS\n"));
        crate::finish_test(true);
    }
    crate::finish_test(false)
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    fatal("DOUBLE FAULT", &frame, Some(code))
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    fatal("MACHINE CHECK", &frame, None)
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let address = Cr2::read().expect("Noncanonical CR2");
    crate::serial::emergency(format_args!(
        "EXCEPTION: PAGE FAULT, address={address:?}, error={code:?}\n{frame:#?}\n"
    ));
    #[cfg(feature = "test-page-fault")]
    if address.as_u64() == 0x0000_4000_0000_0000
        && !code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
    {
        crate::serial::emergency(format_args!("PAGE FAULT ACCEPTANCE PASS\n"));
        crate::finish_test(true);
    }
    if address.as_u64() == crate::memory::TEST_ADDRESS {
        let expected = if cfg!(feature = "test-memory-protection") {
            Some((
                PageFaultErrorCode::PROTECTION_VIOLATION | PageFaultErrorCode::CAUSED_BY_WRITE,
                "MEMORY PROTECTION ACCEPTANCE PASS",
            ))
        } else if cfg!(feature = "test-memory-unmap") {
            Some((PageFaultErrorCode::empty(), "MEMORY UNMAP ACCEPTANCE PASS"))
        } else if cfg!(feature = "test-memory-execute") {
            Some((
                PageFaultErrorCode::PROTECTION_VIOLATION | PageFaultErrorCode::INSTRUCTION_FETCH,
                "MEMORY EXECUTE ACCEPTANCE PASS",
            ))
        } else {
            None
        };
        if let Some((expected_code, marker)) = expected {
            if code == expected_code {
                crate::serial::emergency(format_args!("{marker}\n"));
                crate::finish_test(true);
            }
        }
    }
    crate::finish_test(false)
}
