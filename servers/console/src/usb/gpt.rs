//! Locates the ZOS data volume on a USB disk through its GPT partition table.
//!
//! ZOS only ever writes inside the partition whose type GUID is `ZOS_DATA_GUID`,
//! so the boot files and any other partition on the same stick stay untouched.

use super::{Error, Result};

/// Sectors of the ZOS simple filesystem (16 MiB of 512-byte sectors).
pub const ZOS_SECTORS: u64 = 32768;

/// GPT partition type of the ZOS data volume,
/// f2ac67b5-91fa-4c46-a324-95b456c3829d, in on-disk (mixed-endian) byte order.
pub const ZOS_DATA_GUID: [u8; 16] = [
    0xb5, 0x67, 0xac, 0xf2, 0xfa, 0x91, 0x46, 0x4c, 0xa3, 0x24, 0x95, 0xb4, 0x56, 0xc3, 0x82, 0x9d,
];

const ENTRY_SIZE: usize = 128;
const MAX_ENTRIES: u32 = 128;

/// The only disk sectors ZOS may write: `base .. base + sectors`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub base: u64,
    pub sectors: u64,
}

impl Window {
    /// A window starting at `base` that covers the full ZOS volume; the
    /// 10-byte SCSI commands in use address at most 2^32 sectors.
    pub fn new(base: u64) -> Result<Self> {
        if base.checked_add(ZOS_SECTORS).ok_or(Error::Invalid)? > u64::from(u32::MAX) {
            return Err(Error::Unsupported);
        }
        Ok(Self {
            base,
            sectors: ZOS_SECTORS,
        })
    }
    pub fn contains(self, sector: u64) -> bool {
        sector >= self.base && sector - self.base < self.sectors
    }
}

/// Incremental CRC-32 (IEEE 802.3, as GPT uses).
#[derive(Clone, Copy)]
pub struct Crc32(u32);

impl Crc32 {
    pub fn new() -> Self {
        Self(0xffff_ffff)
    }
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.0 ^= u32::from(byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(self.0 & 1));
            }
        }
    }
    pub fn finish(self) -> u32 {
        !self.0
    }
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// Validated primary GPT header (LBA 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub entries_lba: u64,
    pub count: u32,
    array_crc: u32,
}

impl Header {
    pub fn parse(sector: &[u8; 512], disk_sectors: u64) -> Result<Self> {
        if &sector[..8] != b"EFI PART" || le32(sector, 8) != 0x0001_0000 {
            return Err(Error::Unsupported);
        }
        let size = le32(sector, 12) as usize;
        if !(92..=512).contains(&size) {
            return Err(Error::Invalid);
        }
        let mut copy = *sector;
        copy[16..20].fill(0);
        if crc32(&copy[..size]) != le32(sector, 16) || le64(sector, 24) != 1 {
            return Err(Error::Invalid);
        }
        let entries_lba = le64(sector, 72);
        let count = le32(sector, 80);
        if count == 0
            || count > MAX_ENTRIES
            || le32(sector, 84) as usize != ENTRY_SIZE
            || entries_lba < 2
            || entries_lba + u64::from(Self::sectors(count)) > disk_sectors
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            entries_lba,
            count,
            array_crc: le32(sector, 88),
        })
    }
    /// Sectors occupied by the partition entry array.
    pub fn sectors_needed(&self) -> u32 {
        Self::sectors(self.count)
    }
    fn sectors(count: u32) -> u32 {
        (count * ENTRY_SIZE as u32).div_ceil(512)
    }
}

/// Feeds the entry array one sector at a time, checking its CRC and collecting
/// the single ZOS data partition.
pub struct Scan {
    header: Header,
    crc: Crc32,
    remaining: usize,
    found: Option<(u64, u64)>,
    matches: u32,
}

impl Scan {
    pub fn new(header: Header) -> Self {
        Self {
            header,
            crc: Crc32::new(),
            remaining: header.count as usize * ENTRY_SIZE,
            found: None,
            matches: 0,
        }
    }
    pub fn feed(&mut self, sector: &[u8; 512]) -> Result<()> {
        let used = self.remaining.min(512);
        if used == 0 {
            return Err(Error::Invalid);
        }
        self.crc.update(&sector[..used]);
        for entry in sector[..used].chunks_exact(ENTRY_SIZE) {
            if entry[..16] == ZOS_DATA_GUID {
                self.matches += 1;
                self.found = Some((le64(entry, 32), le64(entry, 40)));
            }
        }
        self.remaining -= used;
        Ok(())
    }
    pub fn finish(self, disk_sectors: u64) -> Result<Window> {
        if self.remaining != 0 || self.crc.finish() != self.header.array_crc {
            return Err(Error::Invalid);
        }
        // Several ZOS partitions would make the write target ambiguous.
        let (first, last) = match (self.matches, self.found) {
            (1, Some(range)) => range,
            _ => return Err(Error::Unsupported),
        };
        if first < 2 || last < first || last >= disk_sectors || last - first + 1 < ZOS_SECTORS {
            return Err(Error::Unsupported);
        }
        Window::new(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISK: u64 = 131_072;

    fn entry(kind: [u8; 16], first: u64, last: u64) -> [u8; 128] {
        let mut bytes = [0; 128];
        bytes[..16].copy_from_slice(&kind);
        bytes[16] = 1;
        bytes[32..40].copy_from_slice(&first.to_le_bytes());
        bytes[40..48].copy_from_slice(&last.to_le_bytes());
        bytes
    }

    fn table(entries: &[[u8; 128]], count: u32) -> (std::vec::Vec<u8>, [u8; 512]) {
        let mut array = std::vec![0; count as usize * 128];
        for (index, entry) in entries.iter().enumerate() {
            array[index * 128..(index + 1) * 128].copy_from_slice(entry);
        }
        let mut header = [0; 512];
        header[..8].copy_from_slice(b"EFI PART");
        header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
        header[12..16].copy_from_slice(&92_u32.to_le_bytes());
        header[24..32].copy_from_slice(&1_u64.to_le_bytes());
        header[72..80].copy_from_slice(&2_u64.to_le_bytes());
        header[80..84].copy_from_slice(&count.to_le_bytes());
        header[84..88].copy_from_slice(&128_u32.to_le_bytes());
        header[88..92].copy_from_slice(&crc32(&array).to_le_bytes());
        let crc = crc32(&header[..92]);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        (array, header)
    }

    fn scan(array: &[u8], header: &[u8; 512]) -> Result<Window> {
        let mut scan = Scan::new(Header::parse(header, DISK)?);
        let mut padded = array.to_vec();
        padded.resize(array.len().div_ceil(512) * 512, 0);
        for sector in padded.chunks_exact(512) {
            scan.feed(sector.try_into().unwrap())?;
        }
        scan.finish(DISK)
    }

    const OTHER: [u8; 16] = [7; 16];

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let mut split = Crc32::new();
        split.update(b"1234");
        split.update(b"56789");
        assert_eq!(split.finish(), 0xcbf4_3926);
    }

    #[test]
    fn guid_literal_matches_the_documented_value() {
        let text = "f2ac67b5-91fa-4c46-a324-95b456c3829d".replace('-', "");
        let mut expected = [0; 16];
        for (index, byte) in expected.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap();
        }
        // The first three fields are stored little-endian on disk.
        expected[..4].reverse();
        expected[4..6].reverse();
        expected[6..8].reverse();
        assert_eq!(ZOS_DATA_GUID, expected);
    }

    #[test]
    fn finds_the_zos_partition_among_others() {
        let (array, header) = table(
            &[
                entry(OTHER, 2048, 8191),
                entry(ZOS_DATA_GUID, 16384, 16384 + 32767),
            ],
            128,
        );
        let window = scan(&array, &header).unwrap();
        assert_eq!(
            window,
            Window {
                base: 16384,
                sectors: ZOS_SECTORS
            }
        );
        assert!(window.contains(16384) && window.contains(16384 + 32767));
        assert!(!window.contains(16383) && !window.contains(16384 + 32768));
    }

    #[test]
    fn partial_last_sector_and_small_tables_work() {
        let (array, header) = table(&[entry(ZOS_DATA_GUID, 4096, 4096 + 40000)], 4);
        assert_eq!(scan(&array, &header).unwrap().base, 4096);
        // Five entries use 640 bytes: the second sector is only partly covered.
        let (array, header) = table(&[entry(ZOS_DATA_GUID, 4096, 40000)], 5);
        assert_eq!(scan(&array, &header).unwrap().base, 4096);
    }

    #[test]
    fn missing_duplicate_small_or_out_of_range_partitions_are_refused() {
        let cases: [&[[u8; 128]]; 5] = [
            &[entry(OTHER, 2048, 90000)],
            &[
                entry(ZOS_DATA_GUID, 4096, 40000),
                entry(ZOS_DATA_GUID, 50000, 90000),
            ],
            &[entry(ZOS_DATA_GUID, 4096, 4096 + 32766)],
            &[entry(ZOS_DATA_GUID, 4096, DISK)],
            &[entry(ZOS_DATA_GUID, 1, 40000)],
        ];
        for entries in cases {
            let (array, header) = table(entries, 128);
            assert_eq!(scan(&array, &header), Err(Error::Unsupported));
        }
    }

    #[test]
    fn corrupted_headers_and_arrays_are_refused() {
        let (array, header) = table(&[entry(ZOS_DATA_GUID, 4096, 40000)], 128);
        assert!(scan(&array, &header).is_ok());
        let mut bad = header;
        bad[0] = b'X';
        assert_eq!(Header::parse(&bad, DISK), Err(Error::Unsupported));
        let mut bad = header;
        bad[80] ^= 1;
        assert_eq!(Header::parse(&bad, DISK), Err(Error::Invalid));
        let mut bad = header;
        bad[24] = 2;
        assert_eq!(Header::parse(&bad, DISK), Err(Error::Invalid));
        let mut damaged = array.clone();
        damaged[4096] ^= 1;
        assert_eq!(scan(&damaged, &header), Err(Error::Invalid));
        assert_eq!(
            Header::parse(&header, 20),
            Err(Error::Invalid),
            "entry array beyond the end of the disk"
        );
    }

    #[test]
    fn window_must_fit_ten_byte_scsi_addresses() {
        assert!(Window::new(u64::from(u32::MAX) - ZOS_SECTORS).is_ok());
        assert_eq!(
            Window::new(u64::from(u32::MAX) - ZOS_SECTORS + 1),
            Err(Error::Unsupported)
        );
        assert_eq!(Window::new(u64::MAX), Err(Error::Invalid));
    }
}
