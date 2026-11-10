use limine::memmap::{Entry, MEMMAP_USABLE};

use super::{Error, PAGE_SIZE};

pub struct Frames {
    bitmap: &'static mut [u64],
    entries: &'static [&'static Entry],
    metadata_start: u64,
    metadata_end: u64,
    cursor: usize,
    pub total: usize,
    pub free: usize,
}

impl Frames {
    pub fn new(entries: &'static [&'static Entry], hhdm: u64) -> Self {
        let usable = || entries.iter().copied().filter(|e| e.type_ == MEMMAP_USABLE);
        let end = usable()
            .map(|e| e.base.checked_add(e.length).expect("Memory map overflow"))
            .max()
            .expect("No usable physical memory");
        let words = end.div_ceil(PAGE_SIZE).div_ceil(64) as usize;
        let bytes = (words as u64 * 8).div_ceil(PAGE_SIZE) * PAGE_SIZE;
        let metadata_start = usable()
            .map(|e| (e.base.max(PAGE_SIZE), e.base + e.length))
            .find(|(start, end)| end - start >= bytes)
            .expect("No usable region for frame bitmap")
            .0;
        // SAFETY: this page-aligned usable region is mapped by Limine's HHDM,
        // reserved below, and never handed out by this allocator.
        let bitmap =
            unsafe { core::slice::from_raw_parts_mut((hhdm + metadata_start) as *mut u64, words) };
        bitmap.fill(u64::MAX);
        let mut frames = Self {
            bitmap,
            entries,
            metadata_start,
            metadata_end: metadata_start + bytes,
            cursor: 0,
            total: 0,
            free: 0,
        };
        for entry in usable() {
            assert_eq!(entry.base % PAGE_SIZE, 0, "Unaligned usable memory");
            assert_eq!(entry.length % PAGE_SIZE, 0, "Unaligned usable memory");
            for address in (entry.base..entry.base + entry.length).step_by(PAGE_SIZE as usize) {
                if frames.managed(address) {
                    let index = (address / PAGE_SIZE) as usize;
                    assert_ne!(frames.bitmap[index / 64] & (1 << (index % 64)), 0);
                    frames.bitmap[index / 64] &= !(1 << (index % 64));
                    frames.free += 1;
                }
                frames.total += 1;
            }
        }
        frames
    }

    fn managed(&self, address: u64) -> bool {
        address != 0
            && address % PAGE_SIZE == 0
            && !(self.metadata_start..self.metadata_end).contains(&address)
            && self
                .entries
                .iter()
                .any(|e| e.type_ == MEMMAP_USABLE && (e.base..e.base + e.length).contains(&address))
    }

    pub fn allocate(&mut self) -> Result<u64, Error> {
        for word in self.cursor..self.bitmap.len() {
            let available = !self.bitmap[word];
            if available != 0 {
                let bit = available.trailing_zeros() as usize;
                self.bitmap[word] |= 1 << bit;
                self.cursor = word;
                self.free -= 1;
                return Ok((word * 64 + bit) as u64 * PAGE_SIZE);
            }
        }
        Err(Error::OutOfFrames)
    }

    pub fn release(&mut self, address: u64) -> Result<(), Error> {
        if !self.managed(address) {
            return Err(Error::InvalidFrame);
        }
        let index = (address / PAGE_SIZE) as usize;
        let mask = 1 << (index % 64);
        if self.bitmap[index / 64] & mask == 0 {
            return Err(Error::DoubleFree);
        }
        self.bitmap[index / 64] &= !mask;
        self.cursor = self.cursor.min(index / 64);
        self.free += 1;
        Ok(())
    }
}
