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

mod pipes {
    use super::*;
    use crate::vfs::{Io, PIPE_BUFFER};

    fn vfs() -> Vfs<Filesystem<MemoryDisk>> {
        Vfs::new(Filesystem::mount(MemoryDisk::formatted()).unwrap())
    }

    fn drain(vfs: &mut Vfs<Filesystem<MemoryDisk>>) -> Vec<vfs::Wake> {
        std::iter::from_fn(|| vfs.next_wake()).collect()
    }

    #[test]
    fn bytes_arrive_in_order_and_an_empty_read_waits_for_the_writer() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        assert_eq!(vfs.write_io(1, write, b"hello", 9), Ok(Io::Done(5)));
        let mut bytes = [0; 8];
        assert_eq!(vfs.read_io(1, read, &mut bytes, 9), Ok(Io::Done(5)));
        assert_eq!(&bytes[..5], b"hello");
        // Nothing buffered and a writer still open: the reader's token is parked.
        assert_eq!(vfs.read_io(1, read, &mut bytes, 77), Ok(Io::Parked));
        assert!(drain(&mut vfs).is_empty());
        assert_eq!(vfs.write_io(1, write, b"xy", 9), Ok(Io::Done(2)));
        let wakes = drain(&mut vfs);
        assert_eq!(wakes.len(), 1);
        assert_eq!((wakes[0].token, wakes[0].reading), (77, true));
        assert_eq!(wakes[0].result, Ok(2));
        assert_eq!(&wakes[0].data[..2], b"xy");
        // A plain (non-parking) read of an empty pipe is refused, never silently parked.
        assert_eq!(vfs.read(1, read, &mut bytes), Err(Error::Invalid));
    }

    #[test]
    fn end_of_file_only_after_every_writer_is_closed() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        let copy = vfs.dup(1, write).unwrap();
        vfs.write(1, write, b"ab").unwrap();
        vfs.close(1, write).unwrap();
        let mut bytes = [0; 4];
        // Data first, even though one end is closed; the duplicate keeps it open.
        assert_eq!(vfs.read_io(1, read, &mut bytes, 5), Ok(Io::Done(2)));
        assert_eq!(vfs.read_io(1, read, &mut bytes, 5), Ok(Io::Parked));
        vfs.close(1, copy).unwrap();
        let wakes = drain(&mut vfs);
        assert_eq!(wakes.len(), 1);
        assert_eq!((wakes[0].token, wakes[0].result), (5, Ok(0)));
        assert_eq!(vfs.read_io(1, read, &mut bytes, 5), Ok(Io::Done(0)));
        vfs.close(1, read).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
    }

    #[test]
    fn writing_with_no_readers_is_a_broken_pipe_and_wakes_a_blocked_writer() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        // Fill the pipe, then park a writer on it.
        let full = [7; PIPE_BUFFER];
        assert_eq!(vfs.write_io(1, write, &full[..900], 1), Ok(Io::Done(900)));
        assert_eq!(vfs.write_io(1, write, &full[..300], 42), Ok(Io::Parked));
        vfs.close(1, read).unwrap();
        let wakes = drain(&mut vfs);
        assert_eq!(wakes.len(), 1);
        assert_eq!(
            (wakes[0].token, wakes[0].result),
            (42, Err(Error::BrokenPipe))
        );
        assert_eq!(vfs.write(1, write, b"x"), Err(Error::BrokenPipe));
        vfs.close(1, write).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
    }

    #[test]
    fn a_stream_many_times_the_buffer_arrives_complete_and_in_order() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        let stream: Vec<u8> = (0..10_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let (mut sent, mut received) = (0, Vec::new());
        let mut writer_parked = false;
        let mut bytes = [0u8; 300];
        while received.len() < stream.len() {
            // The writer produces a chunk whenever it is not blocked.
            if !writer_parked && sent < stream.len() {
                let end = (sent + 700).min(stream.len());
                match vfs.write_io(1, write, &stream[sent..end], 100).unwrap() {
                    Io::Done(count) => sent += count,
                    Io::Parked => {
                        sent = end;
                        writer_parked = true;
                    }
                }
            }
            // The (slow) reader takes a small piece.
            match vfs.read_io(1, read, &mut bytes, 200).unwrap() {
                Io::Done(count) => received.extend_from_slice(&bytes[..count]),
                Io::Parked => unreachable!("the writer always runs ahead of this reader"),
            }
            for wake in drain(&mut vfs) {
                if wake.token == 100 {
                    assert_eq!(wake.result, Ok(wake.result.unwrap()));
                    writer_parked = false;
                }
            }
        }
        assert_eq!(received, stream);
        vfs.close(1, write).unwrap();
        let mut last = [0; 4];
        assert_eq!(vfs.read_io(1, read, &mut last, 200), Ok(Io::Done(0)));
        vfs.close(1, read).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
    }

    #[test]
    fn a_parked_writer_is_finished_in_pieces_as_the_reader_makes_room() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        assert_eq!(vfs.write_io(1, write, &data, 1), Ok(Io::Done(1000)));
        assert_eq!(vfs.write_io(1, write, &data, 2), Ok(Io::Parked));
        let mut got = Vec::new();
        let mut bytes = [0u8; 400];
        let mut finished = None;
        for _ in 0..10 {
            if let Io::Done(count) = vfs.read_io(1, read, &mut bytes, 3).unwrap() {
                got.extend_from_slice(&bytes[..count]);
            }
            if let Some(wake) = drain(&mut vfs).into_iter().find(|wake| wake.token == 2) {
                finished = Some(wake.result);
            }
            if got.len() >= 2000 {
                break;
            }
        }
        assert_eq!(finished, Some(Ok(1000)));
        assert_eq!(&got[..1000], &data[..]);
        assert_eq!(&got[1000..2000], &data[..]);
    }

    #[test]
    fn descriptors_can_be_granted_to_a_child_and_only_the_owner_may_use_them() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        vfs.grant(1, write, 2).unwrap();
        assert_eq!(vfs.write(1, write, b"no"), Err(Error::BadFd));
        assert_eq!(vfs.grant(1, write, 3), Err(Error::BadFd));
        assert_eq!(vfs.write(3, write, b"no"), Err(Error::BadFd));
        vfs.write(2, write, b"child").unwrap();
        let mut bytes = [0; 8];
        assert_eq!(vfs.read(1, read, &mut bytes), Ok(5));
        // The child exits: its end is closed for it, so the parent sees end of file.
        vfs.close_owner(2);
        assert_eq!(vfs.read_io(1, read, &mut bytes, 4), Ok(Io::Done(0)));
        vfs.close(1, read).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
    }

    #[test]
    fn a_dead_process_loses_its_parked_request_without_a_reply() {
        let mut vfs = vfs();
        let (read, write) = vfs.pipe(1).unwrap();
        vfs.grant(1, read, 2).unwrap();
        let mut bytes = [0; 4];
        assert_eq!(vfs.read_io(2, read, &mut bytes, 55), Ok(Io::Parked));
        vfs.close_owner(2);
        assert_eq!(vfs.write(1, write, b"data"), Err(Error::BrokenPipe));
        assert!(
            drain(&mut vfs).is_empty(),
            "no reply may go to a dead process"
        );
        vfs.close(1, write).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
    }

    #[test]
    fn duplicated_file_descriptors_share_their_offset() {
        let mut vfs = vfs();
        let fd = vfs.open(1, b"/f", READ | WRITE | CREATE).unwrap();
        vfs.write(1, fd, b"abcdef").unwrap();
        vfs.close(1, fd).unwrap();
        let first = vfs.open(1, b"/f", READ).unwrap();
        let second = vfs.dup(1, first).unwrap();
        let mut bytes = [0; 3];
        assert_eq!(vfs.read(1, first, &mut bytes), Ok(3));
        assert_eq!(&bytes, b"abc");
        assert_eq!(vfs.read(1, second, &mut bytes), Ok(3));
        assert_eq!(&bytes, b"def");
        assert_eq!(vfs.counts().1, 1, "one open file behind both descriptors");
        vfs.close(1, first).unwrap();
        assert_eq!(vfs.read(1, second, &mut bytes), Ok(0));
        vfs.close(1, second).unwrap();
        assert_eq!(vfs.counts(), (0, 0, 0));
        // A file that is held open (even through a dup) cannot be removed.
        let again = vfs.open(1, b"/f", READ).unwrap();
        let copy = vfs.dup(1, again).unwrap();
        vfs.close(1, again).unwrap();
        assert_eq!(vfs.remove(b"/f"), Err(Error::Denied));
        vfs.close(1, copy).unwrap();
        vfs.remove(b"/f").unwrap();
    }

    #[test]
    fn exhausting_pipes_descriptors_and_wrong_directions_fail_cleanly() {
        let mut vfs = vfs();
        let mut ends = Vec::new();
        let mut created = 0;
        while let Ok(pair) = vfs.pipe(1) {
            ends.push(pair);
            created += 1;
        }
        assert_eq!(created, 8, "the pipe table is bounded");
        let (read, write) = ends[0];
        assert_eq!(vfs.write(1, read, b"x"), Err(Error::Denied));
        assert_eq!(vfs.read(1, write, &mut [0; 1]), Err(Error::Denied));
        assert_eq!(vfs.write_io(1, write, &[0; 2000], 1), Err(Error::TooLarge));
        for (read, write) in ends {
            vfs.close(1, read).unwrap();
            vfs.close(1, write).unwrap();
        }
        assert_eq!(vfs.counts(), (0, 0, 0));
        // And the table is usable again afterwards.
        assert!(vfs.pipe(1).is_ok());
    }
}
