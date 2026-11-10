mod frames;
mod heap;
mod tests;

use frames::Frames;
use spin::Mutex;
use x86_64::{
    instructions::{interrupts::without_interrupts, tlb},
    registers::{
        control::{Cr0, Cr0Flags, Cr3, Cr3Flags},
        model_specific::{Efer, EferFlags},
    },
    structures::paging::{PageTable, PageTableFlags as Flags, PhysFrame},
    PhysAddr, VirtAddr,
};

pub const PAGE_SIZE: u64 = 4096;
const HEAP_START: u64 = 0xffff_c000_0000_0000;
const HEAP_SIZE: usize = 8 * 1024 * 1024;
pub const TEST_ADDRESS: u64 = 0x0040_0000;
static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    OutOfFrames,
    InvalidFrame,
    DoubleFree,
    InvalidAddress,
    AlreadyMapped,
    NotMapped,
    HugeMapping,
}

#[derive(Clone, Copy)]
struct Permissions {
    writable: bool,
    executable: bool,
}

impl Permissions {
    const DATA: Self = Self {
        writable: true,
        executable: false,
    };
    const READ_ONLY: Self = Self {
        writable: false,
        executable: false,
    };

    fn flags(self, user: bool) -> Flags {
        let mut flags = Flags::PRESENT;
        flags.set(Flags::WRITABLE, self.writable);
        flags.set(Flags::NO_EXECUTE, !self.executable);
        flags.set(Flags::USER_ACCESSIBLE, user);
        flags
    }
}

struct Memory {
    frames: Frames,
    hhdm: u64,
    kernel_root: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub usable_bytes: u64,
    pub free_bytes: u64,
    pub used_bytes: u64,
    pub heap_used_bytes: usize,
}

fn with<T>(f: impl FnOnce(&mut Memory) -> T) -> T {
    without_interrupts(|| f(MEMORY.lock().as_mut().expect("Memory not initialized")))
}

impl Memory {
    fn table(&self, frame: u64) -> *mut PageTable {
        (self.hhdm + frame) as *mut PageTable
    }

    fn allocate_zeroed(&mut self) -> Result<u64, Error> {
        let frame = self.frames.allocate()?;
        // SAFETY: the allocator returned exclusive usable RAM mapped by HHDM.
        unsafe { core::ptr::write_bytes((self.hhdm + frame) as *mut u8, 0, PAGE_SIZE as usize) };
        Ok(frame)
    }

    fn clone_kernel_table(&mut self, source: u64, level: usize) -> Result<u64, Error> {
        let destination = self.allocate_zeroed()?;
        let first = if level == 4 { 256 } else { 0 };
        for index in first..512 {
            // SAFETY: the boot tables remain reserved and mapped; destination is
            // a distinct frame. Only this serialized manager mutates new tables.
            let entry = unsafe { (&*self.table(source))[index].clone() };
            if !entry.flags().contains(Flags::PRESENT) {
                continue;
            }
            let mut copied = entry.clone();
            if level > 1 && !entry.flags().contains(Flags::HUGE_PAGE) {
                let child = match self.clone_kernel_table(entry.addr().as_u64(), level - 1) {
                    Ok(child) => child,
                    Err(error) => {
                        self.release_tree(destination, level, false);
                        return Err(error);
                    }
                };
                copied.set_addr(PhysAddr::new(child), entry.flags());
            }
            copied.set_flags(copied.flags() & !Flags::USER_ACCESSIBLE);
            // SAFETY: destination is exclusively owned by this manager.
            unsafe { (&mut *self.table(destination))[index] = copied };
        }
        Ok(destination)
    }

    fn release_tree(&mut self, frame: u64, level: usize, release_data: bool) {
        for index in 0..512 {
            // SAFETY: only detached, privately owned table trees reach here.
            let entry = unsafe { (&*self.table(frame))[index].clone() };
            if !entry.flags().contains(Flags::PRESENT) {
                continue;
            }
            if level > 1 && !entry.flags().contains(Flags::HUGE_PAGE) {
                self.release_tree(entry.addr().as_u64(), level - 1, release_data);
            } else if release_data {
                assert_eq!(level, 1, "Owned user mapping must be 4 KiB");
                self.frames.release(entry.addr().as_u64()).unwrap();
            }
        }
        self.frames.release(frame).unwrap();
    }

    fn indices(address: u64) -> Result<[usize; 4], Error> {
        if address % PAGE_SIZE != 0 || VirtAddr::try_new(address).is_err() || address == 0 {
            return Err(Error::InvalidAddress);
        }
        Ok([39, 30, 21, 12].map(|shift| ((address >> shift) & 511) as usize))
    }

    fn leaf(
        &mut self,
        root: u64,
        address: u64,
        create: bool,
    ) -> Result<*mut x86_64::structures::paging::page_table::PageTableEntry, Error> {
        let indices = Self::indices(address)?;
        let user = address < 0x0000_8000_0000_0000;
        let mut table = root;
        let mut created = [(0_u64, 0_usize, 0_u64); 3];
        let mut count = 0;
        for index in indices[..3].iter().copied() {
            // SAFETY: root and all present children are owned or shared kernel
            // tables; no references survive a mutation or a frame allocation.
            let entry = unsafe { (&*self.table(table))[index].clone() };
            if entry.flags().contains(Flags::HUGE_PAGE) {
                return Err(Error::HugeMapping);
            }
            if entry.flags().contains(Flags::PRESENT) {
                table = entry.addr().as_u64();
            } else if create {
                let child = match self.allocate_zeroed() {
                    Ok(child) => child,
                    Err(error) => {
                        for &(parent, slot, frame) in created[..count].iter().rev() {
                            // SAFETY: these empty ancestors were allocated by
                            // this walk and have not been exposed to any caller.
                            unsafe { (&mut *self.table(parent))[slot].set_unused() };
                            self.frames.release(frame).unwrap();
                        }
                        return Err(error);
                    }
                };
                let mut flags = Flags::PRESENT | Flags::WRITABLE;
                flags.set(Flags::USER_ACCESSIBLE, user);
                // SAFETY: the parent slot was absent and child is zeroed.
                unsafe {
                    (&mut *self.table(table))[index].set_addr(PhysAddr::new(child), flags);
                }
                created[count] = (table, index, child);
                count += 1;
                table = child;
            } else {
                return Err(Error::NotMapped);
            }
        }
        // SAFETY: this pointer is used only while holding the memory lock.
        Ok(unsafe { &mut (&mut *self.table(table))[indices[3]] })
    }

    fn map(&mut self, root: u64, address: u64, permissions: Permissions) -> Result<(), Error> {
        Self::indices(address)?;
        let frame = self.allocate_zeroed()?;
        let result = (|| {
            let leaf = self.leaf(root, address, true)?;
            // SAFETY: leaf points into our serialized page-table tree.
            if unsafe { !(*leaf).is_unused() } {
                return Err(Error::AlreadyMapped);
            }
            // SAFETY: the new leaf exclusively owns frame. Kernel mappings are
            // supervisor-only; lower-half mappings are explicitly user-accessible.
            unsafe {
                (*leaf).set_addr(
                    PhysAddr::new(frame),
                    permissions.flags(address < 0x0000_8000_0000_0000),
                );
            }
            tlb::flush(VirtAddr::new(address));
            Ok(())
        })();
        if result.is_err() {
            self.frames.release(frame).unwrap();
        }
        result
    }

    fn protect(&mut self, root: u64, address: u64, permissions: Permissions) -> Result<(), Error> {
        let leaf = self.leaf(root, address, false)?;
        // SAFETY: the lock serializes changes; the caller owns this mapping.
        unsafe {
            if !(*leaf).flags().contains(Flags::PRESENT) {
                return Err(Error::NotMapped);
            }
            (*leaf).set_flags(permissions.flags(address < 0x0000_8000_0000_0000));
        }
        tlb::flush(VirtAddr::new(address));
        Ok(())
    }

    fn unmap(&mut self, root: u64, address: u64) -> Result<(), Error> {
        let indices = Self::indices(address)?;
        let leaf = self.leaf(root, address, false)?;
        // SAFETY: this owned 4 KiB mapping can be detached under the lock.
        let frame = unsafe {
            if !(*leaf).flags().contains(Flags::PRESENT) {
                return Err(Error::NotMapped);
            }
            let frame = (*leaf).addr().as_u64();
            (*leaf).set_unused();
            frame
        };
        tlb::flush(VirtAddr::new(address));
        self.frames.release(frame)?;
        let mut path = [(0_u64, 0_usize, 0_u64); 3];
        let mut table = root;
        for (depth, index) in indices[..3].iter().copied().enumerate() {
            // SAFETY: the ancestors remain attached until pruning below.
            let child = unsafe { (&*self.table(table))[index].addr().as_u64() };
            path[depth] = (table, index, child);
            table = child;
        }
        for (parent, index, child) in path.into_iter().rev() {
            // Kernel PML4 slots are permanent so every process sees future maps.
            if parent == root && index >= 256 {
                break;
            }
            // SAFETY: child is an owned table with no concurrent mutations.
            if unsafe { (*self.table(child)).iter().any(|e| !e.is_unused()) } {
                break;
            }
            // SAFETY: the empty child is detached before returning its frame.
            unsafe { (&mut *self.table(parent))[index].set_unused() };
            self.frames.release(child)?;
        }
        Ok(())
    }

    fn mapping(&mut self, root: u64, address: u64) -> Result<(u64, Flags), Error> {
        let leaf = self.leaf(root, address, false)?;
        // SAFETY: the memory lock prevents concurrent PTE changes.
        unsafe {
            if !(*leaf).flags().contains(Flags::PRESENT) {
                return Err(Error::NotMapped);
            }
            Ok(((*leaf).addr().as_u64(), (*leaf).flags()))
        }
    }
}

struct AddressSpace {
    root: u64,
}

impl AddressSpace {
    fn new() -> Result<Self, Error> {
        with(|memory| {
            let root = memory.allocate_zeroed()?;
            // SAFETY: root is distinct and private. Only upper-level entries are
            // copied; their supervisor-only subtrees remain kernel-owned/shared.
            unsafe {
                for index in 256..512 {
                    (&mut *memory.table(root))[index] =
                        (&*memory.table(memory.kernel_root))[index].clone();
                }
            }
            Ok(Self { root })
        })
    }

    fn user_address(address: u64) -> Result<(), Error> {
        Memory::indices(address)?;
        if address >= 0x0000_8000_0000_0000 {
            return Err(Error::InvalidAddress);
        }
        Ok(())
    }

    fn map(&mut self, address: u64, permissions: Permissions) -> Result<(), Error> {
        Self::user_address(address)?;
        with(|memory| memory.map(self.root, address, permissions))
    }

    fn protect(&mut self, address: u64, permissions: Permissions) -> Result<(), Error> {
        Self::user_address(address)?;
        with(|memory| memory.protect(self.root, address, permissions))
    }

    fn unmap(&mut self, address: u64) -> Result<(), Error> {
        Self::user_address(address)?;
        with(|memory| memory.unmap(self.root, address))
    }

    // SAFETY: callers must execute entirely in the shared kernel half, with no
    // live references into the previous lower half, until restoring that root.
    unsafe fn activate(&self) {
        // SAFETY: every AddressSpace contains a live root with shared kernel maps.
        unsafe { activate(self.root) };
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        with(|memory| {
            assert_ne!(Cr3::read().0.start_address().as_u64(), self.root);
            for index in 0..256 {
                // SAFETY: inactive lower-half trees are exclusively process-owned.
                let entry = unsafe { (&*memory.table(self.root))[index].clone() };
                if entry.flags().contains(Flags::PRESENT) {
                    memory.release_tree(entry.addr().as_u64(), 3, true);
                }
            }
            memory.frames.release(self.root).unwrap();
        });
    }
}

unsafe fn activate(root: u64) {
    // SAFETY: the caller guarantees root is live and preserves executing kernel,
    // stack, interrupt handlers, HHDM, and heap mappings. CR3 flushes user TLBs.
    unsafe {
        Cr3::write(
            PhysFrame::from_start_address(PhysAddr::new(root)).unwrap(),
            Cr3Flags::empty(),
        );
    }
}

pub fn init() {
    assert!(!x86_64::instructions::interrupts::are_enabled());
    let entries = crate::boot::MEMORY_MAP.response().unwrap().entries();
    let hhdm = crate::boot::HHDM.response().unwrap().offset;
    let frames = Frames::new(entries, hhdm);
    let mut memory = Memory {
        frames,
        hhdm,
        kernel_root: 0,
    };
    let boot_root = Cr3::read().0.start_address().as_u64();
    memory.kernel_root = memory.clone_kernel_table(boot_root, 4).unwrap();
    for index in 256..512 {
        // SAFETY: the new root is inactive and exclusively owned.
        if unsafe { (&*memory.table(memory.kernel_root))[index].is_unused() } {
            let child = memory.allocate_zeroed().unwrap();
            // SAFETY: permanent supervisor-only upper slots allow all process
            // roots to share current and future kernel mappings.
            unsafe {
                (&mut *memory.table(memory.kernel_root))[index]
                    .set_addr(PhysAddr::new(child), Flags::PRESENT | Flags::WRITABLE);
            }
        }
    }
    // SAFETY: x86-64 QEMU's selected CPU supports NX. These preserve all other
    // boot settings and enforce read-only pages even for supervisor writes.
    unsafe {
        Efer::update(|flags| flags.insert(EferFlags::NO_EXECUTE_ENABLE));
        Cr0::update(|flags| flags.insert(Cr0Flags::WRITE_PROTECT));
        activate(memory.kernel_root);
    }
    assert_ne!(Cr3::read().0.start_address().as_u64(), boot_root);
    crate::logln!("PAGING ready: ZOS-owned root, shared supervisor kernel half, WP/NX");
    for address in (HEAP_START..HEAP_START + HEAP_SIZE as u64).step_by(PAGE_SIZE as usize) {
        memory
            .map(memory.kernel_root, address, Permissions::DATA)
            .expect("Cannot map kernel heap");
    }
    assert!(MEMORY.lock().replace(memory).is_none());
    heap::init(HEAP_START, HEAP_SIZE);
    crate::logln!("HEAP ready: {} bytes", HEAP_SIZE);
    tests::smoke();
    let stats = stats();
    crate::logln!(
        "MEMORY ready: usable={} free={} used={} heap_used={} bytes",
        stats.usable_bytes,
        stats.free_bytes,
        stats.used_bytes,
        stats.heap_used_bytes
    );
}

pub fn stats() -> Stats {
    let (usable_bytes, free_bytes) = with(|memory| {
        (
            memory.frames.total as u64 * PAGE_SIZE,
            memory.frames.free as u64 * PAGE_SIZE,
        )
    });
    Stats {
        usable_bytes,
        free_bytes,
        used_bytes: usable_bytes - free_bytes,
        heap_used_bytes: heap::used(),
    }
}

pub fn acceptance() -> ! {
    tests::stress();
    crate::logln!("MEMORY ACCEPTANCE PASS");
    crate::finish_test(true)
}

pub fn fault_acceptance() -> ! {
    let mut space = AddressSpace::new().unwrap();
    space.map(TEST_ADDRESS, Permissions::DATA).unwrap();
    // SAFETY: the space shares our executing kernel and owns the test mapping.
    unsafe {
        space.activate();
        core::ptr::write_volatile(TEST_ADDRESS as *mut u8, 0xc3);
    }
    if cfg!(feature = "test-memory-protection") {
        space.protect(TEST_ADDRESS, Permissions::READ_ONLY).unwrap();
        crate::logln!("TEST: write to read-only page at {:#x}", TEST_ADDRESS);
        // SAFETY: this dedicated image expects a protection-violation #PF.
        unsafe { core::ptr::write_volatile(TEST_ADDRESS as *mut u8, 0) };
    } else if cfg!(feature = "test-memory-unmap") {
        space.unmap(TEST_ADDRESS).unwrap();
        crate::logln!("TEST: read unmapped page at {:#x}", TEST_ADDRESS);
        // SAFETY: this dedicated image expects a non-present #PF after TLB flush.
        unsafe { core::ptr::read_volatile(TEST_ADDRESS as *const u8) };
    } else if cfg!(feature = "test-memory-execute") {
        crate::logln!("TEST: execute NX page at {:#x}", TEST_ADDRESS);
        // SAFETY: this dedicated image expects an instruction-fetch #PF. The
        // byte would be a valid `ret` instruction if NX were incorrectly disabled.
        unsafe {
            let execute: extern "C" fn() = core::mem::transmute(TEST_ADDRESS as usize);
            execute();
        }
    }
    panic!("Expected memory protection exception did not fire");
}
