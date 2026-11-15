use super::gpt::Window;
use super::{Error, Result};

pub const MAX_SCRATCHPADS: usize = zos_libos::USB_MAX_SCRATCHPADS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Low,
    Full,
    High,
    Super,
}

impl Speed {
    pub fn packet(self) -> u32 {
        match self {
            Self::Low | Self::Full => 8,
            Self::High => 64,
            Self::Super => 512,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protocol {
    pub revision: u8,
    pub slot_type: u8,
    pub speeds: [Option<Speed>; 16],
}

impl Protocol {
    pub fn speed(self, id: u8) -> Result<Speed> {
        self.speeds
            .get(id as usize)
            .copied()
            .flatten()
            .ok_or(Error::Unsupported)
    }
}

pub fn dma_page(physical: u64) -> Result<()> {
    if physical > u32::MAX as u64 - 4095 {
        return Err(Error::DmaAddress(physical));
    }
    Ok(())
}

pub fn scratchpads(structural: u32) -> Result<usize> {
    let count = (((structural >> 21) & 31) << 5 | (structural >> 27) & 31) as usize;
    if count > MAX_SCRATCHPADS {
        return Err(Error::Unsupported);
    }
    Ok(count)
}

pub fn speeds(revision: u8, slot_type: u8, entries: &[u32]) -> Result<Protocol> {
    if !matches!(revision, 2 | 3) {
        return Err(Error::Unsupported);
    }
    let mut protocol = Protocol {
        revision,
        slot_type,
        speeds: [None; 16],
    };
    if entries.is_empty() {
        if revision == 2 {
            protocol.speeds[1] = Some(Speed::Full);
            protocol.speeds[2] = Some(Speed::Low);
            protocol.speeds[3] = Some(Speed::High);
        } else {
            protocol.speeds[4] = Some(Speed::Super);
        }
        return Ok(protocol);
    }
    let mut index = 0;
    while index < entries.len() {
        let entry = entries[index];
        let id = (entry & 15) as usize;
        let rate = (entry >> 16) as u64 * 1000_u64.pow((entry >> 4) & 3);
        if id == 0 || protocol.speeds[id].is_some() {
            return Err(Error::Invalid);
        }
        let speed = match (revision, rate) {
            (2, 1_500_000) => Speed::Low,
            (2, 12_000_000) => Speed::Full,
            (2, 480_000_000) => Speed::High,
            (3, 5_000_000_000 | 10_000_000_000 | 20_000_000_000) => Speed::Super,
            _ => {
                // Vendor-defined rates (for example Intel's USB4-tunnel lane
                // speeds) stay unmapped: only a device that actually attaches
                // at such a speed ID is rejected, not the whole protocol.
                index += if revision == 3 && (entry >> 6) & 3 == 2 {
                    2
                } else {
                    1
                };
                continue;
            }
        };
        match (entry >> 6) & 3 {
            0 => (),
            2 if revision == 3 => {
                let tx = *entries.get(index + 1).ok_or(Error::Invalid)?;
                let tx_rate = (tx >> 16) as u64 * 1000_u64.pow((tx >> 4) & 3);
                if tx & 15 != id as u32 || (tx >> 6) & 3 != 3 {
                    return Err(Error::Invalid);
                }
                if !matches!(tx_rate, 5_000_000_000 | 10_000_000_000 | 20_000_000_000) {
                    return Err(Error::Unsupported);
                }
                index += 1;
            }
            _ => return Err(Error::Invalid),
        }
        protocol.speeds[id] = Some(speed);
        index += 1;
    }
    Ok(protocol)
}

/// Gate for every SCSI command sent to the stick. Read-only mode (diagnostics,
/// and volume discovery) never writes. Writable mode allows a short list of
/// commands and lets WRITE(10) touch only sectors inside the ZOS `window`.
pub fn scsi(
    read_only: bool,
    window: Option<Window>,
    command: &[u8],
    input: bool,
    length: usize,
) -> Result<()> {
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
    } else {
        let one_block = command.len() == 10
            && length == 512
            && u16::from_be_bytes([command[7], command[8]]) == 1;
        let allowed = match command.first() {
            Some(0) => command.len() == 6 && !input && length == 0,
            Some(3) => command.len() == 6 && input && length == 18,
            Some(0x12) => command.len() == 6 && input && length == 36,
            Some(0x25) => command.len() == 10 && input && length == 8,
            Some(0x28) => one_block && input,
            Some(0x2a) => {
                one_block
                    && !input
                    && window.is_some_and(|window| {
                        let sector = u32::from_be_bytes(command[2..6].try_into().unwrap());
                        window.contains(u64::from(sector))
                    })
            }
            // SYNCHRONIZE CACHE for the whole device: a flush, never a write.
            Some(0x35) => {
                command.len() == 10
                    && !input
                    && length == 0
                    && command[2..9].iter().all(|&byte| byte == 0)
            }
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
    // Writable mode needs 512-byte blocks; the volume's place on the disk is
    // decided separately (see gpt.rs), not by the disk's total size.
    if !read_only && block != 512 {
        return Err(Error::Unsupported);
    }
    Ok((sectors, block))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_pages_must_fit_entirely_in_the_32_bit_range() {
        assert_eq!(dma_page(0xffff_f000), Ok(()));
        for address in [0xffff_f001, 0x1_0000_0000, u64::MAX] {
            assert_eq!(dma_page(address), Err(Error::DmaAddress(address)));
        }
    }

    #[test]
    fn scratchpad_high_low_fields_and_quota() {
        assert_eq!(scratchpads(0), Ok(0));
        assert_eq!(scratchpads(1 << 27), Ok(1));
        assert_eq!(scratchpads(31 << 27), Ok(31));
        assert_eq!(scratchpads(1 << 21), Ok(32));
        assert_eq!(scratchpads((1 << 21) | (1 << 27)), Ok(33));
        assert_eq!(scratchpads(0x1420_0054), Ok(34));
        assert_eq!(scratchpads((1 << 21) | (3 << 27)), Ok(35));
        assert_eq!(scratchpads(2 << 21), Ok(64));
        assert_eq!(scratchpads((2 << 21) | (1 << 27)), Err(Error::Unsupported));
        assert_eq!(scratchpads(0xfbe0_0000), Err(Error::Unsupported));
    }

    #[test]
    fn protocol_tables_map_rates_not_assumed_ids() {
        assert_eq!(speeds(2, 0, &[]).unwrap().speed(2), Ok(Speed::Low));
        assert_eq!(speeds(3, 0, &[]).unwrap().speed(4), Ok(Speed::Super));
        let protocol = speeds(3, 7, &[6 | (3 << 4) | (10 << 16)]).unwrap();
        assert_eq!(protocol.slot_type, 7);
        assert_eq!(protocol.speed(6), Ok(Speed::Super));
        assert_eq!(protocol.speed(4), Err(Error::Unsupported));
        let full = speeds(2, 0, &[7 | (2 << 4) | (12 << 16)]).unwrap();
        assert_eq!(full.speed(7), Ok(Speed::Full));
    }

    #[test]
    fn photographed_intel_table_keeps_standard_speeds_and_ignores_vendor_rates() {
        // Raw PSI dwords photographed on the Intel 8086:4ded laptop controller.
        let entries = [
            327988, 655669, 81789222, 163578151, 327156008, 95486249, 191037738, 382075179,
        ];
        let usb3 = speeds(3, 0, &entries).unwrap();
        assert_eq!(usb3.speed(4), Ok(Speed::Super));
        assert_eq!(usb3.speed(5), Ok(Speed::Super));
        for id in 6..=11 {
            assert_eq!(usb3.speed(id), Err(Error::Unsupported));
        }
        assert_eq!(usb3.speed(1), Err(Error::Unsupported));
    }

    #[test]
    fn unknown_paired_vendor_rates_skip_both_entries() {
        let rx = 6 | (2 << 4) | (2 << 6) | (1248 << 16);
        let tx = 6 | (2 << 4) | (3 << 6) | (1248 << 16);
        let standard = 4 | (3 << 4) | (5 << 16);
        let protocol = speeds(3, 0, &[rx, tx, standard]).unwrap();
        assert_eq!(protocol.speed(6), Err(Error::Unsupported));
        assert_eq!(protocol.speed(4), Ok(Speed::Super));
    }

    #[test]
    fn asymmetric_protocol_speeds_require_an_adjacent_matching_pair() {
        let rx = 6 | (3 << 4) | (2 << 6) | (10 << 16);
        let tx = 6 | (3 << 4) | (3 << 6) | (5 << 16);
        assert_eq!(speeds(3, 0, &[rx, tx]).unwrap().speed(6), Ok(Speed::Super));
        for entries in [&[rx][..], &[tx], &[rx, tx + 1], &[rx, rx], &[0]] {
            assert!(speeds(3, 0, entries).is_err());
        }
        let symmetric = 4 | (3 << 4) | (5 << 16);
        assert!(speeds(3, 0, &[symmetric, symmetric]).is_err());
        assert!(speeds(3, 0, &[4 | (3 << 4) | (1 << 6) | (5 << 16)]).is_err());
    }

    #[test]
    fn diagnostic_transport_denies_every_non_read_command() {
        for opcode in 0..=255 {
            let mut command = [0; 10];
            command[0] = opcode;
            if matches!(opcode, 0x25 | 0x28) {
                continue;
            }
            assert_eq!(scsi(true, None, &command, false, 0), Err(Error::ReadOnly));
        }
        assert_eq!(scsi(true, None, &[0; 6], false, 0), Ok(()));
        assert_eq!(scsi(true, None, &[0x12, 0, 0, 0, 36, 0], true, 36), Ok(()));
        let mut read = [0; 10];
        read[0] = 0x28;
        assert_eq!(scsi(true, None, &read, true, 512), Ok(()));
        assert_eq!(scsi(true, None, &read, false, 512), Err(Error::ReadOnly));
        assert_eq!(scsi(true, None, &read, true, 513), Err(Error::ReadOnly));
        read[0] = 0x2a;
        assert_eq!(scsi(true, None, &read, false, 512), Err(Error::ReadOnly));
        let window = Window::new(1000).ok();
        assert_eq!(scsi(true, window, &read, false, 512), Err(Error::ReadOnly));
        assert_eq!(
            scsi(true, None, &[0x35; 10], false, 0),
            Err(Error::ReadOnly)
        );
    }

    fn block_command(opcode: u8, sector: u32, count: u16) -> [u8; 10] {
        let mut command = [0; 10];
        command[0] = opcode;
        command[2..6].copy_from_slice(&sector.to_be_bytes());
        command[7..9].copy_from_slice(&count.to_be_bytes());
        command
    }

    #[test]
    fn writes_are_confined_to_the_zos_window() {
        let window = Window::new(16384);
        let write = |sector| {
            scsi(
                false,
                window.ok(),
                &block_command(0x2a, sector, 1),
                false,
                512,
            )
        };
        assert_eq!(write(16384), Ok(()));
        assert_eq!(write(16384 + 32767), Ok(()));
        for outside in [0, 1, 2, 2048, 16383, 16384 + 32768, u32::MAX] {
            assert_eq!(write(outside), Err(Error::ReadOnly), "sector {outside}");
        }
        // No window yet (volume discovery): nothing may be written.
        let discovery = |sector| scsi(false, None, &block_command(0x2a, sector, 1), false, 512);
        assert_eq!(discovery(16384), Err(Error::ReadOnly));
        // Only single-block writes, in the right direction and size.
        let multi = block_command(0x2a, 16384, 2);
        assert_eq!(
            scsi(false, window.ok(), &multi, false, 1024),
            Err(Error::ReadOnly)
        );
        let single = block_command(0x2a, 16384, 1);
        assert_eq!(
            scsi(false, window.ok(), &single, true, 512),
            Err(Error::ReadOnly)
        );
        assert_eq!(
            scsi(false, window.ok(), &single, false, 0),
            Err(Error::ReadOnly)
        );
        let zero = block_command(0x2a, 16384, 0);
        assert_eq!(
            scsi(false, window.ok(), &zero, false, 512),
            Err(Error::ReadOnly)
        );
        // Reads may reach the partition table anywhere, one block at a time.
        assert_eq!(
            scsi(false, None, &block_command(0x28, 1, 1), true, 512),
            Ok(())
        );
        assert_eq!(
            scsi(false, None, &block_command(0x28, 1, 2), true, 1024),
            Err(Error::ReadOnly)
        );
    }

    #[test]
    fn writable_mode_allows_only_known_commands() {
        let window = Window::new(16384).ok();
        let allowed = [0x00, 0x03, 0x12, 0x25, 0x28, 0x2a, 0x35];
        for opcode in 0..=255_u8 {
            if allowed.contains(&opcode) {
                continue;
            }
            // Includes FORMAT UNIT (04), WRITE(6) (0a), WRITE(16) (8a), UNMAP (42),
            // WRITE SAME (41, 93) and VERIFY (2f): all must be refused.
            for (length, input) in [(0, false), (512, false), (512, true), (8, true), (36, true)] {
                for command in [
                    &block_command(opcode, 16384, 1)[..],
                    &[opcode, 0, 0, 0, 0, 0][..],
                ] {
                    assert_eq!(
                        scsi(false, window, command, input, length),
                        Err(Error::ReadOnly),
                        "opcode {opcode:#x}"
                    );
                }
            }
        }
        let flush = [0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(scsi(false, window, &flush, false, 0), Ok(()));
        // A ranged flush is not whole-device and is refused.
        let ranged = block_command(0x35, 7, 0);
        assert_eq!(scsi(false, window, &ranged, false, 0), Err(Error::ReadOnly));
    }

    #[test]
    fn diagnostics_accept_real_capacity_without_changing_normal_geometry() {
        let mut bytes = [0; 8];
        bytes[..4].copy_from_slice(&67_108_863_u32.to_be_bytes());
        bytes[4..].copy_from_slice(&512_u32.to_be_bytes());
        assert_eq!(capacity(bytes, true), Ok((67_108_864, 512)));
        // Writable mode accepts a large stick; the ZOS window is found by GPT.
        assert_eq!(capacity(bytes, false), Ok((67_108_864, 512)));
        bytes[4..].copy_from_slice(&4096_u32.to_be_bytes());
        assert_eq!(capacity(bytes, false), Err(Error::Unsupported));
        bytes[4..].copy_from_slice(&512_u32.to_be_bytes());
        bytes[..4].copy_from_slice(&32767_u32.to_be_bytes());
        assert_eq!(capacity(bytes, false), Ok((32768, 512)));
        bytes[4..].fill(0);
        assert_eq!(capacity(bytes, true), Err(Error::Unsupported));
        bytes[..4].fill(255);
        assert_eq!(capacity(bytes, true), Err(Error::Unsupported));
    }
}
