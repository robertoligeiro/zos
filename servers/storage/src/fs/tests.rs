extern crate std;
use super::*;
use crate::vfs::{self, Vfs, CREATE, READ, TRUNCATE, WRITE};
use std::{collections::BTreeMap, vec, vec::Vec};

#[derive(Clone)]
struct MemoryDisk {
    durable: Vec<u8>,
    cache: BTreeMap<u32, [u8; SECTOR]>,
    fail: Option<usize>,
    actions: usize,
    tear: bool,
}
impl MemoryDisk {
    fn formatted() -> Self {
        let mut disk = Self {
            durable: vec![0; SECTORS as usize * SECTOR],
            cache: BTreeMap::new(),
            fail: None,
            actions: 0,
            tear: false,
        };
        disk.write(0, &superblock()).unwrap();
        let metadata = Metadata::empty();
        let body = metadata.encode();
        for (i, part) in body.chunks_exact(SECTOR).enumerate() {
            disk.write(BANKS[0] + 1 + i as u32, part.try_into().unwrap())
                .unwrap();
        }
        disk.write(BANKS[0], &bank_header(1, &body)).unwrap();
        disk.flush().unwrap();
        disk.actions = 0;
        disk
    }
    fn crash(mut self) -> Self {
        self.cache.clear();
        self.fail = None;
        self.actions = 0;
        self
    }
    fn contents(fs: &mut Filesystem<Self>, path: &[u8]) -> Result<Vec<u8>> {
        let id = fs.lookup(path)?;
        let size = fs.stat(id)?.size as usize;
        let mut bytes = vec![0; size];
        assert_eq!(fs.read(id, 0, &mut bytes)?, size);
        Ok(bytes)
    }
}
impl Disk for MemoryDisk {
    fn read(&mut self, sector: u32, bytes: &mut [u8; SECTOR]) -> Result<()> {
        if sector >= SECTORS {
            return Err(Error::Invalid);
        }
        if let Some(value) = self.cache.get(&sector) {
            *bytes = *value;
        } else {
            let start = sector as usize * SECTOR;
            bytes.copy_from_slice(&self.durable[start..start + SECTOR]);
        }
        Ok(())
    }
    fn write(&mut self, sector: u32, bytes: &[u8; SECTOR]) -> Result<()> {
        if sector >= SECTORS {
            return Err(Error::Invalid);
        }
        self.actions += 1;
        if self.fail == Some(self.actions) {
            if self.tear {
                let start = sector as usize * SECTOR;
                self.durable[start..start + SECTOR / 2].copy_from_slice(&bytes[..SECTOR / 2]);
            }
            return Err(Error::Io);
        }
        self.cache.insert(sector, *bytes);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        self.actions += 1;
        if self.fail == Some(self.actions) {
            if self.tear {
                for (sector, bytes) in &self.cache {
                    let start = *sector as usize * SECTOR;
                    self.durable[start..start + SECTOR / 2].copy_from_slice(&bytes[..SECTOR / 2]);
                }
            }
            return Err(Error::Io);
        }
        for (sector, bytes) in &self.cache {
            let start = *sector as usize * SECTOR;
            self.durable[start..start + SECTOR].copy_from_slice(bytes);
        }
        self.cache.clear();
        Ok(())
    }
}

#[test]
fn hierarchy_cross_block_sparse_io_and_reboot() {
    let mut fs = Filesystem::mount(MemoryDisk::formatted()).unwrap();
    fs.create(b"/home", 1).unwrap();
    let id = fs.create(b"/home/file", 2).unwrap();
    assert_eq!(fs.create(b"/home/file", 2), Err(Error::Exists));
    assert_eq!(fs.create(b"/home/file/child", 2), Err(Error::NotDir));
    assert_eq!(fs.read(0, 0, &mut [0]), Err(Error::IsDir));
    let bytes: Vec<u8> = (0..9001).map(|index| (index % 251) as u8).collect();
    fs.write(id, 511, &bytes).unwrap();
    fs.sync().unwrap();
    let mut reboot = Filesystem::mount(fs.into_disk().crash()).unwrap();
    let contents = MemoryDisk::contents(&mut reboot, b"/home/file").unwrap();
    assert_eq!(&contents[..511], &[0; 511]);
    assert_eq!(&contents[511..], &bytes);
    assert_eq!(reboot.read(id, usize::MAX, &mut [0; 3]), Ok(0));
    assert_eq!(reboot.write(id, MAX_FILE, &[1]), Err(Error::TooLarge));
    assert_eq!(reboot.write(id, usize::MAX, &[1]), Err(Error::TooLarge));
    assert_eq!(reboot.remove(b"/home"), Err(Error::NotEmpty));
    reboot.rename(b"/home/file", b"/renamed").unwrap();
    assert_eq!(reboot.lookup(b"/home/file"), Err(Error::NotFound));
    assert_eq!(reboot.readdir(0, 0).unwrap().unwrap().1.kind, 1);
    reboot.remove(b"/home").unwrap();
    reboot.truncate(id).unwrap();
    assert!(MemoryDisk::contents(&mut reboot, b"/renamed")
        .unwrap()
        .is_empty());
}

#[test]
fn every_write_and_flush_crash_recovers_an_entire_checkpoint() {
    let mut base = Filesystem::mount(MemoryDisk::formatted()).unwrap();
    base.create(b"/a", 1).unwrap();
    let id = base.create(b"/a/file", 2).unwrap();
    base.write(id, 0, &[19; 5000]).unwrap();
    base.write(id, 0, &[23; 5000]).unwrap();
    let baseline = base.into_disk().crash();
    for operation in 0..5 {
        let mutate = |fs: &mut Filesystem<MemoryDisk>| -> Result<()> {
            match operation {
                0 => fs.write(id, 0, &[47; 9000]).map(|_| ()),
                1 => fs.rename(b"/a/file", b"/a/new"),
                2 => fs.remove(b"/a/file"),
                3 => fs.create(b"/a/new", 2).map(|_| ()),
                _ => fs.truncate(id),
            }
        };
        let mut old = Filesystem::mount(baseline.clone()).unwrap();
        let old_metadata = old.metadata.encode();
        let old_data = MemoryDisk::contents(&mut old, b"/a/file");
        let mut success = Filesystem::mount(baseline.clone()).unwrap();
        mutate(&mut success).unwrap();
        let new_metadata = success.metadata.encode();
        let new_data = MemoryDisk::contents(&mut success, b"/a/file");
        let actions = success.disk.actions;
        for tear in [false, true] {
            for point in 1..=actions {
                let mut fs = Filesystem::mount(baseline.clone()).unwrap();
                fs.disk.fail = Some(point);
                fs.disk.tear = tear;
                assert_eq!(mutate(&mut fs), Err(Error::Io));
                assert_eq!(fs.lookup(b"/"), Err(Error::Io));
                let mut reboot = Filesystem::mount(fs.into_disk().crash()).unwrap();
                let metadata = reboot.metadata.encode();
                let data = MemoryDisk::contents(&mut reboot, b"/a/file");
                assert!(
                    (metadata == old_metadata && data == old_data)
                        || (metadata == new_metadata && data == new_data),
                    "partial checkpoint: operation={operation}, point={point}, tear={tear}"
                );
            }
        }
    }
}

#[test]
fn previous_checkpoint_data_is_not_reused_before_replacement() {
    let mut fs = Filesystem::mount(MemoryDisk::formatted()).unwrap();
    let id = fs.create(b"/file", 2).unwrap();
    fs.write(id, 0, &[31; 5000]).unwrap();
    fs.write(id, 0, &[71; 5000]).unwrap();
    let latest = fs.bank;
    fs.disk.fail = Some(fs.disk.actions + 1);
    fs.disk.tear = true;
    assert_eq!(fs.write(id, 0, &[91; 5000]), Err(Error::Io));
    let mut disk = fs.into_disk().crash();
    disk.durable[BANKS[latest] as usize * SECTOR + 508] ^= 1;
    let mut reboot = Filesystem::mount(disk).unwrap();
    assert_eq!(
        MemoryDisk::contents(&mut reboot, b"/file").unwrap(),
        vec![31; 5000]
    );
}

#[test]
fn corruption_geometry_bitmap_cycles_and_duplicate_blocks_are_rejected() {
    let disk = MemoryDisk::formatted();
    let mut damaged = disk.clone();
    damaged.durable[16] ^= 1;
    assert!(matches!(Filesystem::mount(damaged), Err(Error::Corrupt)));
    let mut fs = Filesystem::mount(disk).unwrap();
    let directory = fs.create(b"/dir", 1).unwrap();
    let a = fs.create(b"/dir/a", 2).unwrap();
    let b = fs.create(b"/dir/b", 2).unwrap();
    fs.write(a, 0, &[1; 6000]).unwrap();
    fs.write(b, 0, &[2; 6000]).unwrap();
    let mut cycle = fs.metadata.clone();
    cycle.nodes[directory].parent = directory as u16;
    assert_eq!(cycle.validate(), Err(Error::Corrupt));
    let mut duplicate = fs.metadata.clone();
    duplicate.nodes[b].blocks[0] = duplicate.nodes[a].blocks[0];
    assert_eq!(duplicate.validate(), Err(Error::Corrupt));
    let mut bitmap = fs.metadata.clone();
    bitmap.bitmap[30] ^= 1;
    assert_eq!(bitmap.validate(), Err(Error::Corrupt));
    let mut disk = fs.into_disk().crash();
    for bank in BANKS {
        disk.durable[bank as usize * SECTOR + 508] ^= 1;
    }
    assert!(matches!(Filesystem::mount(disk), Err(Error::Corrupt)));
}

#[test]
fn inode_and_file_limits_do_not_change_committed_state() {
    let mut fs = Filesystem::mount(MemoryDisk::formatted()).unwrap();
    let id = fs.create(b"/full", 2).unwrap();
    fs.write(id, 0, &[7; MAX_FILE]).unwrap();
    for index in 2..INODES {
        fs.create(std::format!("/f{index}").as_bytes(), 2).unwrap();
    }
    let before = fs.metadata.encode();
    assert_eq!(fs.create(b"/overflow", 2), Err(Error::NoSpace));
    assert_eq!(fs.write(id, MAX_FILE, &[0]), Err(Error::TooLarge));
    assert_eq!(fs.metadata.encode(), before);
    fs.remove(b"/f2").unwrap();
    assert!(fs.create(b"/reused", 2).is_ok());
}

#[test]
fn vfs_normalization_ownership_rights_stale_fds_and_capacity() {
    let mut vfs = Vfs::new(Filesystem::mount(MemoryDisk::formatted()).unwrap());
    vfs.mkdir(b"//dir/./").unwrap();
    let fd = vfs
        .open(1, b"/dir/child/../file", READ | WRITE | CREATE)
        .unwrap();
    vfs.write(1, fd, b"hello").unwrap();
    assert_eq!(vfs.read(2, fd, &mut [0; 5]), Err(Error::BadFd));
    assert_eq!(vfs.close(2, fd), Err(Error::BadFd));
    assert_eq!(vfs.remove(b"/dir/file"), Err(Error::Denied));
    vfs.rename(b"/dir/file", b"/dir/new").unwrap();
    vfs.close(1, fd).unwrap();
    let replacement = vfs.open(1, b"/dir/new", READ).unwrap();
    assert_ne!(replacement, fd);
    assert_eq!(vfs.close(1, fd), Err(Error::BadFd));
    assert_eq!(vfs.write(1, replacement, b"no"), Err(Error::Denied));
    let mut buffer = [0; 5];
    assert_eq!(vfs.read(1, replacement, &mut buffer), Ok(5));
    assert_eq!(&buffer, b"hello");
    assert_eq!(vfs.read(1, replacement, &mut buffer), Ok(0));
    vfs.close(1, replacement).unwrap();
    assert!(matches!(
        vfs::normalize(b"/../../escape"),
        Err(Error::Invalid)
    ));
    assert!(matches!(vfs::normalize(b"relative"), Err(Error::Invalid)));
    assert!(matches!(vfs::normalize(b"/bad\0name"), Err(Error::Invalid)));
    assert_eq!(vfs.rename(b"/dir", b"/dir/loop"), Err(Error::Invalid));
    let fd = vfs.open(1, b"/dir/new", WRITE | TRUNCATE).unwrap();
    assert_eq!(vfs.stat(b"/dir/new").unwrap().size, 0);
    vfs.close(1, fd).unwrap();
    let mut fds = [0; DESCRIPTORS_FOR_TEST];
    for fd in &mut fds {
        *fd = vfs.open(1, b"/dir/new", READ).unwrap();
    }
    assert_eq!(
        vfs.open(1, b"/never-created", WRITE | CREATE),
        Err(Error::NoSpace)
    );
    assert_eq!(vfs.stat(b"/never-created"), Err(Error::NotFound));
    for fd in fds {
        vfs.close(1, fd).unwrap();
    }
    vfs.remove(b"/dir/new").unwrap();
    vfs.remove(b"/dir").unwrap();
}
const DESCRIPTORS_FOR_TEST: usize = 64;

#[test]
fn exited_owner_cleanup_preserves_other_clients_and_invalidates_old_handles() {
    let mut vfs = Vfs::new(Filesystem::mount(MemoryDisk::formatted()).unwrap());
    let first = vfs.open(1, b"/file", READ | WRITE | CREATE).unwrap();
    vfs.write(1, first, b"data").unwrap();
    let other = vfs.open(2, b"/file", READ).unwrap();
    vfs.close_owner(1);
    assert_eq!(vfs.read(1, first, &mut [0; 4]), Err(Error::BadFd));
    let replacement = vfs.open(1, b"/file", READ).unwrap();
    assert_ne!(replacement, first);
    assert_eq!(vfs.close(1, first), Err(Error::BadFd));
    let mut bytes = [0; 4];
    assert_eq!(vfs.read(2, other, &mut bytes), Ok(4));
    assert_eq!(&bytes, b"data");
    vfs.close_owner(1);
    assert_eq!(vfs.remove(b"/file"), Err(Error::Denied));
    vfs.close(2, other).unwrap();
    vfs.remove(b"/file").unwrap();
}

#[test]
fn malformed_wire_requests_and_trailing_bytes_fail_explicitly() {
    use crate::protocol::{Reader, Request};
    let mut request = Reader::new(&[1, 2]);
    assert_eq!(request.u64(), Err(Error::Invalid));
    let mut request = Reader::new(&[1, 2]);
    request.byte().unwrap();
    assert_eq!(request.end(), Err(Error::Invalid));
    let mut request = Request::new(0);
    assert_eq!(
        request.push(&[0; zos_libos::MAX_STORAGE_MESSAGE]),
        Err(Error::TooLarge)
    );
    assert_eq!(request.bytes(), &[0]);
}
