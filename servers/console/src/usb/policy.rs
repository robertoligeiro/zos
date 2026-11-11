use super::{Error, Result};

pub fn scratchpads(structural: u32) -> Result<usize> {
    let count = (((structural >> 21) & 31) << 5 | (structural >> 27) & 31) as usize;
    if count > 32 {
        return Err(Error::Unsupported);
    }
    Ok(count)
}

pub fn speeds(revision: u8, entries: &[u32]) -> Result<()> {
    if !matches!(revision, 2 | 3) {
        return Err(Error::Unsupported);
    }
    for &entry in entries {
        let id = entry & 15;
        let rate = (entry >> 16) as u64 * 1000_u64.pow((entry >> 4) & 3);
        let expected = match (revision, id) {
            (2, 1) => 12_000_000,
            (2, 2) => 1_500_000,
            (2, 3) => 480_000_000,
            (3, 4) => 5_000_000_000,
            (3, 5) => 10_000_000_000,
            _ => return Err(Error::Unsupported),
        };
        if entry & 0xc0 != 0 || rate != expected {
            return Err(Error::Unsupported);
        }
    }
    Ok(())
}

pub fn scsi(read_only: bool, command: &[u8], input: bool, length: usize) -> Result<()> {
    if read_only {
        let allowed = match command.first() {
            Some(0) => command.len() == 6 && !input && length == 0,
            Some(3) => command.len() == 6 && input && length == 18,
            Some(0x12) => command.len() == 6 && input && length == 36,
            Some(0x25) => command.len() == 10 && input && length == 8,
            Some(0x28) => command.len() == 10 && input && length == 512,
            _ => false,
        };
        if !allowed {
            return Err(Error::ReadOnly);
        }
    }
    if command.is_empty() || command.len() > 16 || length > 512 {
        return Err(Error::Invalid);
    }
    Ok(())
}

pub fn capacity(bytes: [u8; 8], read_only: bool) -> Result<(u64, u32)> {
    let last = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let block = u32::from_be_bytes(bytes[4..].try_into().unwrap());
    if last == u32::MAX || block == 0 {
        return Err(Error::Unsupported);
    }
    let sectors = last as u64 + 1;
    if !read_only && (sectors != 32768 || block != 512) {
        return Err(Error::Unsupported);
    }
    Ok((sectors, block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratchpad_high_low_fields_and_quota() {
        assert_eq!(scratchpads(0), Ok(0));
        assert_eq!(scratchpads(1 << 27), Ok(1));
        assert_eq!(scratchpads(31 << 27), Ok(31));
        assert_eq!(scratchpads(1 << 21), Ok(32));
        assert_eq!(scratchpads((1 << 21) | (1 << 27)), Err(Error::Unsupported));
        assert_eq!(scratchpads(0xfbe0_0000), Err(Error::Unsupported));
    }

    #[test]
    fn protocol_speed_tables_require_recognized_symmetric_ids() {
        assert_eq!(speeds(2, &[]), Ok(()));
        assert_eq!(speeds(3, &[4 | (3 << 4) | (5 << 16)]), Ok(()));
        assert_eq!(speeds(3, &[5 | (3 << 4) | (10 << 16)]), Ok(()));
        assert_eq!(
            speeds(3, &[6 | (3 << 4) | (5 << 16)]),
            Err(Error::Unsupported)
        );
        assert_eq!(
            speeds(3, &[4 | (3 << 4) | (10 << 16)]),
            Err(Error::Unsupported)
        );
        assert_eq!(
            speeds(3, &[4 | (3 << 4) | (1 << 6) | (5 << 16)]),
            Err(Error::Unsupported)
        );
    }

    #[test]
    fn diagnostic_transport_denies_every_non_read_command() {
        for opcode in 0..=255 {
            let mut command = [0; 10];
            command[0] = opcode;
            if matches!(opcode, 0x25 | 0x28) {
                continue;
            }
            assert_eq!(scsi(true, &command, false, 0), Err(Error::ReadOnly));
        }
        assert_eq!(scsi(true, &[0; 6], false, 0), Ok(()));
        assert_eq!(scsi(true, &[0x12, 0, 0, 0, 36, 0], true, 36), Ok(()));
        let mut read = [0; 10];
        read[0] = 0x28;
        assert_eq!(scsi(true, &read, true, 512), Ok(()));
        assert_eq!(scsi(true, &read, false, 512), Err(Error::ReadOnly));
        assert_eq!(scsi(true, &read, true, 513), Err(Error::ReadOnly));
        read[0] = 0x2a;
        assert_eq!(scsi(true, &read, false, 512), Err(Error::ReadOnly));
        assert_eq!(scsi(false, &read, false, 512), Ok(()));
        assert_eq!(scsi(true, &[0x35; 10], false, 0), Err(Error::ReadOnly));
    }

    #[test]
    fn diagnostics_accept_real_capacity_without_changing_normal_geometry() {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&67_108_863_u32.to_be_bytes());
        bytes[4..].copy_from_slice(&512_u32.to_be_bytes());
        assert_eq!(capacity(bytes, true), Ok((67_108_864, 512)));
        assert_eq!(capacity(bytes, false), Err(Error::Unsupported));
        bytes[..4].copy_from_slice(&32767_u32.to_be_bytes());
        assert_eq!(capacity(bytes, false), Ok((32768, 512)));
        bytes[4..].fill(0);
        assert_eq!(capacity(bytes, true), Err(Error::Unsupported));
        bytes[..4].fill(255);
        assert_eq!(capacity(bytes, true), Err(Error::Unsupported));
    }
}
