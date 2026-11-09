use limine::request::{
    ExecutableAddressRequest, FirmwareTypeRequest, FramebufferRequest, HhdmRequest, MemmapRequest,
    StackSizeRequest,
};

#[used]
#[link_section = ".limine_requests_start"]
static START: limine::RequestsStartMarker = limine::RequestsStartMarker::new();

// Revision 3 works with the pinned Limine 9 binary distribution.
#[used]
#[link_section = ".limine_requests"]
pub static BASE_REVISION: limine::BaseRevision = limine::BaseRevision::with_revision(3);

#[used]
#[link_section = ".limine_requests"]
pub static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".limine_requests"]
pub static MEMORY_MAP: MemmapRequest = MemmapRequest::new();

#[used]
#[link_section = ".limine_requests"]
pub static HHDM: HhdmRequest = HhdmRequest::new();

#[used]
#[link_section = ".limine_requests"]
pub static EXECUTABLE: ExecutableAddressRequest = ExecutableAddressRequest::new();

#[used]
#[link_section = ".limine_requests"]
static FIRMWARE: FirmwareTypeRequest = FirmwareTypeRequest::new();

#[used]
#[link_section = ".limine_requests"]
static STACK: StackSizeRequest = StackSizeRequest::new(64 * 1024);

#[used]
#[link_section = ".limine_requests_end"]
static END: limine::RequestsEndMarker = limine::RequestsEndMarker::new();

pub fn report() {
    let memory = MEMORY_MAP.response().expect("Limine memory map missing");
    let hhdm = HHDM.response().expect("Limine HHDM missing");
    let executable = EXECUTABLE
        .response()
        .expect("Limine executable address missing");
    let firmware = FIRMWARE.response().expect("Limine firmware type missing");
    let mode = match firmware.firmware_type {
        0 => "BIOS",
        2 => "UEFI",
        other => panic!("Unsupported firmware type: {other}"),
    };
    crate::logln!("FIRMWARE: {mode}");
    crate::logln!("Memory-map entries: {}", memory.entries().len());
    crate::logln!("HHDM: {:#x}", hhdm.offset);
    crate::logln!(
        "Kernel: physical={:#x} virtual={:#x}",
        executable.physical_base,
        executable.virtual_base
    );
}
