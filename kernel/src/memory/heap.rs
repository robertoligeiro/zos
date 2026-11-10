use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use linked_list_allocator::Heap;
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;

struct KernelHeap(Mutex<Heap>);

#[global_allocator]
static HEAP: KernelHeap = KernelHeap(Mutex::new(Heap::empty()));

// SAFETY: every access is serialized with interrupts disabled. The mapped heap
// is exclusively owned by this allocator and remains mapped for kernel lifetime.
unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| {
            HEAP.0
                .lock()
                .allocate_first_fit(layout)
                .map_or(core::ptr::null_mut(), NonNull::as_ptr)
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        without_interrupts(|| {
            // SAFETY: GlobalAlloc's caller supplies a live allocation and its
            // original layout; the heap lock prevents concurrent list mutation.
            unsafe {
                HEAP.0
                    .lock()
                    .deallocate(NonNull::new(pointer).expect("Null heap free"), layout);
            }
        });
    }
}

pub fn init(start: u64, size: usize) {
    without_interrupts(|| {
        // SAFETY: memory::init mapped this entire aligned region read/write,
        // exclusively for the heap, and calls this function exactly once.
        unsafe { HEAP.0.lock().init(start as *mut u8, size) };
    });
}

pub fn used() -> usize {
    without_interrupts(|| HEAP.0.lock().used())
}

#[alloc_error_handler]
fn allocation_failure(layout: Layout) -> ! {
    panic!("Kernel heap exhausted: {layout:?}")
}
