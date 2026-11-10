use crate::memory::{AddressSpace, Permissions, PAGE_SIZE};

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    InvalidElf,
    InvalidSegment,
    Memory,
}

fn number(bytes: &[u8], offset: usize, width: usize) -> Result<u64, Error> {
    let data = bytes
        .get(offset..offset.checked_add(width).ok_or(Error::InvalidElf)?)
        .ok_or(Error::InvalidElf)?;
    let mut value = [0; 8];
    value[..width].copy_from_slice(data);
    Ok(u64::from_le_bytes(value))
}

#[derive(Clone, Copy)]
struct Segment {
    offset: usize,
    address: u64,
    file_size: usize,
    end: u64,
    writable: bool,
    executable: bool,
}

fn header(bytes: &[u8]) -> Result<(u64, usize, usize), Error> {
    if bytes.get(..7) != Some(b"\x7fELF\x02\x01\x01")
        || number(bytes, 16, 2)? != 2
        || number(bytes, 18, 2)? != 62
        || number(bytes, 20, 4)? != 1
        || number(bytes, 52, 2)? != 64
        || number(bytes, 54, 2)? != 56
    {
        return Err(Error::InvalidElf);
    }
    let offset = number(bytes, 32, 8)? as usize;
    let count = number(bytes, 56, 2)? as usize;
    if count == 0
        || count > 32
        || offset < 64
        || offset
            .checked_add(count * 56)
            .is_none_or(|end| end > bytes.len())
    {
        return Err(Error::InvalidElf);
    }
    Ok((number(bytes, 24, 8)?, offset, count))
}

fn segment(bytes: &[u8], offset: usize) -> Result<Option<Segment>, Error> {
    let kind = number(bytes, offset, 4)?;
    if kind == 2 || kind == 3 {
        return Err(Error::InvalidSegment); // No dynamic interpreter or relocations.
    }
    if kind != 1 {
        return Ok(None);
    }
    let flags = number(bytes, offset + 4, 4)?;
    let file_offset = number(bytes, offset + 8, 8)?;
    let address = number(bytes, offset + 16, 8)?;
    let file_size = number(bytes, offset + 32, 8)?;
    let memory_size = number(bytes, offset + 40, 8)?;
    if memory_size == 0 {
        if file_size != 0 {
            return Err(Error::InvalidSegment);
        }
        return Ok(None);
    }
    let end = address
        .checked_add(memory_size)
        .ok_or(Error::InvalidSegment)?;
    let file_end = file_offset
        .checked_add(file_size)
        .ok_or(Error::InvalidSegment)?;
    if flags & !7 != 0
        || flags & 4 == 0
        || flags & 3 == 3
        || address < 0x10000
        || address % PAGE_SIZE != 0
        || end > 0x1000_0000
        || file_size > memory_size
        || file_end > bytes.len() as u64
        || file_offset % PAGE_SIZE != 0
        || number(bytes, offset + 48, 8)? != PAGE_SIZE
    {
        return Err(Error::InvalidSegment);
    }
    Ok(Some(Segment {
        offset: file_offset as usize,
        address,
        file_size: file_size as usize,
        end,
        writable: flags & 2 != 0,
        executable: flags & 1 != 0,
    }))
}

pub fn load(bytes: &[u8]) -> Result<(AddressSpace, u64), Error> {
    let (entry, offset, count) = header(bytes)?;
    let mut segments = [None; 32];
    let mut pages = 0;
    let mut valid_entry = false;
    for index in 0..count {
        let Some(current) = segment(bytes, offset + index * 56)? else {
            continue;
        };
        let end = current.end.div_ceil(PAGE_SIZE) * PAGE_SIZE;
        for previous in segments[..index].iter().flatten() {
            let previous: &Segment = previous;
            if current.address < previous.end.div_ceil(PAGE_SIZE) * PAGE_SIZE
                && previous.address < end
            {
                return Err(Error::InvalidSegment);
            }
        }
        pages += (end - current.address) / PAGE_SIZE;
        if pages > 2048 {
            return Err(Error::InvalidSegment);
        }
        valid_entry |= current.executable && (current.address..current.end).contains(&entry);
        segments[index] = Some(current);
    }
    if !valid_entry {
        return Err(Error::InvalidElf);
    }
    let mut space = AddressSpace::new().map_err(|_| Error::Memory)?;
    for segment in segments.into_iter().flatten() {
        for page in (segment.address..segment.end).step_by(PAGE_SIZE as usize) {
            space
                .map(page, Permissions::DATA)
                .map_err(|_| Error::Memory)?;
        }
        space
            .write_user(
                segment.address,
                &bytes[segment.offset..segment.offset + segment.file_size],
            )
            .map_err(|_| Error::Memory)?;
        for page in (segment.address..segment.end).step_by(PAGE_SIZE as usize) {
            space
                .protect(
                    page,
                    Permissions {
                        writable: segment.writable,
                        executable: segment.executable,
                    },
                )
                .map_err(|_| Error::Memory)?;
        }
    }
    Ok((space, entry))
}

pub fn self_test(valid: &[u8]) {
    assert!(matches!(load(&[]), Err(Error::InvalidElf)));
    assert!(load(&valid[..valid.len().min(63)]).is_err());
    let mut changed = alloc::vec::Vec::from(valid);
    changed[18] = 3;
    assert!(load(&changed).is_err());
    changed.copy_from_slice(valid);
    let (_, offset, count) = header(valid).unwrap();
    for index in 0..count {
        let location = offset + index * 56;
        if number(valid, location, 4).unwrap() == 1 && number(valid, location + 40, 8).unwrap() > 0
        {
            changed[location + 4..location + 8].copy_from_slice(&7_u32.to_le_bytes());
            assert!(matches!(load(&changed), Err(Error::InvalidSegment)));
            changed.copy_from_slice(valid);
            changed[location + 16..location + 24]
                .copy_from_slice(&0xffff_c000_0000_0000_u64.to_le_bytes());
            assert!(matches!(load(&changed), Err(Error::InvalidSegment)));
            changed.copy_from_slice(valid);
            changed[location + 32..location + 40].copy_from_slice(&u64::MAX.to_le_bytes());
            assert!(matches!(load(&changed), Err(Error::InvalidSegment)));
            break;
        }
    }
    crate::logln!("ELF VALIDATION PASS: truncated, wrong machine, W+X, kernel range, overflow");
}
