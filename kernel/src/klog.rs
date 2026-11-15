//! In-memory copy of everything the kernel and the user services log.
//!
//! Every line written to the serial port also lands here, so the process manager
//! can hand it to the shell (`dmesg`) and it can be saved to the stick for
//! diagnosis. The buffer keeps the most recent `SIZE` bytes; absolute offsets only
//! ever grow, which lets a reader continue where it stopped.

use core::sync::atomic::{AtomicU64, Ordering};

pub const SIZE: usize = 32768;

static mut BUFFER: [u8; SIZE] = [0; SIZE];
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Appends one byte. This kernel runs on one CPU and logs with interrupts
/// disabled (or from a fault handler that has just disabled them), so writers
/// cannot interleave.
pub fn push(byte: u8) {
    let written = WRITTEN.load(Ordering::Relaxed);
    // SAFETY: single-writer by the argument above; readers only copy bytes below
    // the published `WRITTEN` count.
    unsafe {
        (*core::ptr::addr_of_mut!(BUFFER))[(written % SIZE as u64) as usize] = byte;
    }
    WRITTEN.store(written + 1, Ordering::Release);
}

/// Copies log bytes starting at absolute `offset` into `out`. An offset older than
/// the oldest byte still held starts at the oldest byte. Returns the absolute
/// offset actually used and the number of bytes copied.
pub fn read(offset: u64, out: &mut [u8]) -> (u64, usize) {
    let written = WRITTEN.load(Ordering::Acquire);
    let oldest = written.saturating_sub(SIZE as u64);
    let start = offset.max(oldest);
    if start >= written {
        return (written, 0);
    }
    let count = ((written - start) as usize).min(out.len());
    for (index, byte) in out[..count].iter_mut().enumerate() {
        // SAFETY: indices below `written` were published by `push`.
        *byte = unsafe {
            (*core::ptr::addr_of!(BUFFER))[((start + index as u64) % SIZE as u64) as usize]
        };
    }
    (start, count)
}
