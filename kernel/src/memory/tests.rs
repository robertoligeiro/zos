use alloc::{alloc::alloc, alloc::dealloc, boxed::Box, vec, vec::Vec};
use core::alloc::Layout;

use super::*;

pub fn smoke() {
    let heap_before = heap::used();
    {
        let boxed = Box::new(0x5a5a_f00d_u64);
        let mut values = Vec::new();
        for value in 0..4096_u64 {
            values.push(value ^ *boxed);
        }
        for (index, value) in values.iter().enumerate() {
            assert_eq!(*value, index as u64 ^ *boxed);
        }
        let layout = Layout::from_size_align(8193, 65536).unwrap();
        // SAFETY: layout is valid; the allocation is checked, written within
        // bounds, and deallocated exactly once using the identical layout.
        unsafe {
            let aligned = alloc(layout);
            assert!(!aligned.is_null());
            assert_eq!(aligned as usize % layout.align(), 0);
            core::ptr::write_bytes(aligned, 0x5a, layout.size());
            dealloc(aligned, layout);
        }
    }
    assert_eq!(heap::used(), heap_before);
    crate::logln!("HEAP PASS: Box, Vec growth, alignment, deallocation");

    let before = stats().free_bytes;
    let mut first = AddressSpace::new().unwrap();
    let mut second = AddressSpace::new().unwrap();
    const SHARED_ADDRESS: u64 = 0xffff_d000_0000_0000;
    with(|memory| {
        memory
            .map(memory.kernel_root, SHARED_ADDRESS, Permissions::DATA)
            .unwrap();
        let (_, flags) = memory.mapping(memory.kernel_root, SHARED_ADDRESS).unwrap();
        assert!(!flags.contains(Flags::USER_ACCESSIBLE));
    });
    for space in [&mut first, &mut second] {
        assert_eq!(space.map(0, Permissions::DATA), Err(Error::InvalidAddress));
        assert_eq!(
            space.map(TEST_ADDRESS + 1, Permissions::DATA),
            Err(Error::InvalidAddress)
        );
        assert_eq!(
            space.map(HEAP_START, Permissions::DATA),
            Err(Error::InvalidAddress)
        );
        assert_eq!(space.unmap(TEST_ADDRESS), Err(Error::NotMapped));
        space.map(TEST_ADDRESS, Permissions::DATA).unwrap();
        let mapped_free = stats().free_bytes;
        assert_eq!(
            space.map(TEST_ADDRESS, Permissions::DATA),
            Err(Error::AlreadyMapped)
        );
        assert_eq!(stats().free_bytes, mapped_free);
    }
    with(|memory| {
        let (one, flags) = memory.mapping(first.root, TEST_ADDRESS).unwrap();
        let (two, _) = memory.mapping(second.root, TEST_ADDRESS).unwrap();
        assert_ne!(one, two);
        assert!(flags.contains(Flags::USER_ACCESSIBLE | Flags::WRITABLE | Flags::NO_EXECUTE));
        // SAFETY: the memory lock serializes access to these distinct root pages.
        unsafe {
            for index in 256..512 {
                let kernel = &(&*memory.table(memory.kernel_root))[index];
                for root in [first.root, second.root] {
                    let entry = &(&*memory.table(root))[index];
                    assert_eq!(entry.addr(), kernel.addr());
                    assert!(!entry.flags().contains(Flags::USER_ACCESSIBLE));
                }
            }
        }
    });
    // SAFETY: all executing code, stack, heap and interrupt state are in the
    // shared kernel half. Volatile user accesses are confined to live mappings.
    unsafe {
        first.activate();
        assert_eq!(core::ptr::read_volatile(TEST_ADDRESS as *const u64), 0);
        core::ptr::write_volatile(TEST_ADDRESS as *mut u64, 0x1111);
        core::ptr::write_volatile(SHARED_ADDRESS as *mut u64, 0x3333);
        second.activate();
        assert_eq!(core::ptr::read_volatile(TEST_ADDRESS as *const u64), 0);
        assert_eq!(
            core::ptr::read_volatile(SHARED_ADDRESS as *const u64),
            0x3333
        );
        core::ptr::write_volatile(TEST_ADDRESS as *mut u64, 0x2222);
        first.activate();
        assert_eq!(core::ptr::read_volatile(TEST_ADDRESS as *const u64), 0x1111);
        let shared = Box::new(123_u64);
        second.activate();
        assert_eq!(*shared, 123);
        assert_eq!(core::ptr::read_volatile(TEST_ADDRESS as *const u64), 0x2222);
        drop(shared);
        with(|memory| activate(memory.kernel_root));
    }
    with(|memory| memory.unmap(memory.kernel_root, SHARED_ADDRESS).unwrap());
    first
        .protect(
            TEST_ADDRESS,
            Permissions {
                writable: false,
                executable: true,
            },
        )
        .unwrap();
    with(|memory| {
        let (_, flags) = memory.mapping(first.root, TEST_ADDRESS).unwrap();
        assert!(!flags.intersects(Flags::WRITABLE | Flags::NO_EXECUTE));
    });
    first.protect(TEST_ADDRESS, Permissions::READ_ONLY).unwrap();
    with(|memory| {
        let (_, flags) = memory.mapping(first.root, TEST_ADDRESS).unwrap();
        assert!(!flags.contains(Flags::WRITABLE));
        assert!(flags.contains(Flags::NO_EXECUTE | Flags::USER_ACCESSIBLE));
    });
    first.unmap(TEST_ADDRESS).unwrap();
    assert_eq!(
        first.protect(TEST_ADDRESS, Permissions::DATA),
        Err(Error::NotMapped)
    );
    assert_eq!(first.unmap(TEST_ADDRESS), Err(Error::NotMapped));
    first.map(TEST_ADDRESS, Permissions::DATA).unwrap();
    with(|memory| {
        let (frame, _) = memory.mapping(first.root, TEST_ADDRESS).unwrap();
        for index in 0..512 {
            // SAFETY: the HHDM alias is read only while the mapping is owned and idle.
            assert_eq!(
                unsafe {
                    core::ptr::read_volatile((memory.hhdm + frame + index * 8) as *const u64)
                },
                0
            );
        }
    });
    drop(first);
    drop(second);
    assert_eq!(
        stats().free_bytes,
        before,
        "Address-space teardown leaked frames"
    );
    assert_eq!(heap::used(), heap_before);
    crate::logln!("PAGING PASS: map/unmap/protect, CR3 isolation, shared kernel, teardown");
}

pub fn stress() {
    const COUNT: usize = 100_000;
    let before = stats();
    assert!(before.free_bytes / PAGE_SIZE > COUNT as u64);
    let mut frames = Vec::with_capacity(COUNT);
    with(|memory| {
        assert_eq!(memory.frames.release(0), Err(Error::InvalidFrame));
        assert_eq!(
            memory
                .frames
                .release(crate::boot::EXECUTABLE.response().unwrap().physical_base),
            Err(Error::InvalidFrame)
        );
        for index in 0..COUNT {
            let frame = memory.frames.allocate().unwrap();
            assert_eq!(frame % PAGE_SIZE, 0);
            // SAFETY: each frame is live, exclusively allocated, and HHDM-mapped.
            unsafe {
                let pointer = (memory.hhdm + frame) as *mut u64;
                pointer.write_volatile(index as u64);
                pointer.add(511).write_volatile(!(index as u64));
            }
            frames.push(frame);
        }
        assert_eq!(
            memory.frames.free as u64 * PAGE_SIZE,
            before.free_bytes - COUNT as u64 * PAGE_SIZE
        );
        for (index, frame) in frames.iter().copied().enumerate() {
            // SAFETY: these frames remain allocated until checked and released.
            unsafe {
                let pointer = (memory.hhdm + frame) as *const u64;
                assert_eq!(pointer.read_volatile(), index as u64);
                assert_eq!(pointer.add(511).read_volatile(), !(index as u64));
            }
            memory.frames.release(frame).unwrap();
            assert_eq!(memory.frames.release(frame), Err(Error::DoubleFree));
        }
    });
    assert_eq!(stats().free_bytes, before.free_bytes);
    drop(frames);
    crate::logln!("FRAMES PASS: 100000 simultaneous pages, contents, free, double-free rejection");

    let mut space = AddressSpace::new().unwrap();
    let available = (stats().free_bytes / PAGE_SIZE) as usize;
    let mut held = Vec::with_capacity(available);
    with(|memory| {
        for _ in 0..available {
            held.push(memory.frames.allocate().unwrap());
        }
        assert_eq!(memory.frames.allocate(), Err(Error::OutOfFrames));
    });
    assert!(matches!(AddressSpace::new(), Err(Error::OutOfFrames)));
    assert_eq!(
        space.map(TEST_ADDRESS, Permissions::DATA),
        Err(Error::OutOfFrames)
    );
    for _ in 0..3 {
        let frame = held.pop().unwrap();
        with(|memory| memory.frames.release(frame).unwrap());
    }
    let free = stats().free_bytes;
    // One data frame and two page tables fit, but the final table must fail.
    assert_eq!(
        space.map(TEST_ADDRESS, Permissions::DATA),
        Err(Error::OutOfFrames)
    );
    assert_eq!(
        stats().free_bytes,
        free,
        "Failed map leaked intermediate tables"
    );
    let frame = held.pop().unwrap();
    with(|memory| memory.frames.release(frame).unwrap());
    space.map(TEST_ADDRESS, Permissions::DATA).unwrap();
    space.unmap(TEST_ADDRESS).unwrap();
    with(|memory| {
        for frame in held.drain(..) {
            memory.frames.release(frame).unwrap();
        }
    });
    drop(held);
    drop(space);
    assert_eq!(stats().free_bytes, before.free_bytes);
    crate::logln!("FRAMES OOM PASS: exhaustion, partial-map rollback, recovery");

    let used = heap::used();
    let mut blocks = Vec::with_capacity(28);
    for index in 0..28 {
        blocks.push(vec![index as u8; 256 * 1024 + index * 16]);
    }
    for index in (0..28).step_by(2) {
        assert_eq!(blocks[index][0], index as u8);
        blocks[index].clear();
        blocks[index].shrink_to_fit();
    }
    drop(blocks);
    // Requires coalescing of the freed small blocks rather than a bump allocator.
    let large = vec![0x5a_u8; HEAP_SIZE / 2];
    assert!(large.iter().all(|byte| *byte == 0x5a));
    drop(large);
    assert_eq!(heap::used(), used);
    let excessive = Layout::from_size_align(HEAP_SIZE + PAGE_SIZE as usize, 4096).unwrap();
    // SAFETY: this valid but oversized allocation must return null without
    // mutating the free list; there is no allocation to deallocate.
    assert!(unsafe { alloc(excessive) }.is_null());
    assert_eq!(heap::used(), used);
    crate::logln!("HEAP STRESS PASS: fragmentation, coalescing, large allocation, no leaks");
}
