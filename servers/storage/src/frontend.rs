#![no_std]
#![no_main]

use zos_libos as os;
use zos_storage::{
    protocol::{FsClient, Reader},
    vfs::{Io, Vfs},
    *,
};

/// What a request did: answered now with this many bytes, or kept the caller waiting
/// (a pipe read with nothing to read, a pipe write with no room); `Vfs::next_wake`
/// delivers the answer later.
enum Done {
    Reply(usize),
    Parked,
}

fn handle(
    vfs: &mut Vfs<FsClient>,
    owner: usize,
    token: usize,
    request: &[u8],
    reply: &mut [u8],
) -> Result<Done> {
    let mut reader = Reader::new(request);
    match reader.byte()? {
        0 => {
            let flags = reader.byte()?;
            let handle = vfs.open(owner, reader.rest(), flags)?;
            reply[..8].copy_from_slice(&handle.to_le_bytes());
            Ok(Done::Reply(8))
        }
        1 => {
            let fd = reader.u64()?;
            let count = reader.u16()? as usize;
            reader.end()?;
            if count > PAYLOAD {
                return Err(Error::TooLarge);
            }
            match vfs.read_io(owner, fd, &mut reply[..count], token)? {
                Io::Done(count) => Ok(Done::Reply(count)),
                Io::Parked => Ok(Done::Parked),
            }
        }
        2 => {
            let fd = reader.u64()?;
            let bytes = reader.rest();
            if bytes.len() > PAYLOAD {
                return Err(Error::TooLarge);
            }
            match vfs.write_io(owner, fd, bytes, token)? {
                Io::Done(count) => {
                    reply[..4].copy_from_slice(&(count as u32).to_le_bytes());
                    Ok(Done::Reply(4))
                }
                Io::Parked => Ok(Done::Parked),
            }
        }
        // pipe: a new pipe; replies with the read end then the write end.
        11 => {
            reader.end()?;
            let (read, write) = vfs.pipe(owner)?;
            reply[..8].copy_from_slice(&read.to_le_bytes());
            reply[8..16].copy_from_slice(&write.to_le_bytes());
            Ok(Done::Reply(16))
        }
        // dup fd: a second descriptor for the same open file or pipe end.
        12 => {
            let fd = reader.u64()?;
            reader.end()?;
            let copy = vfs.dup(owner, fd)?;
            reply[..8].copy_from_slice(&copy.to_le_bytes());
            Ok(Done::Reply(8))
        }
        // grant fd to process: hand a descriptor to a child that is about to start.
        13 => {
            let fd = reader.u64()?;
            let target = reader.byte()? as usize;
            reader.end()?;
            if !(1..=os::PROCESS_SLOTS).contains(&target) || target == owner {
                return Err(Error::Invalid);
            }
            vfs.grant(owner, fd, target)?;
            Ok(Done::Reply(0))
        }
        3 => {
            let fd = reader.u64()?;
            reader.end()?;
            vfs.close(owner, fd)?;
            Ok(Done::Reply(0))
        }
        4 => {
            let entry = vfs.stat(reader.rest())?;
            reply[..32].copy_from_slice(&entry.encode());
            Ok(Done::Reply(32))
        }
        5 => {
            let ordinal = reader.u16()? as usize;
            match vfs.readdir(reader.rest(), ordinal)? {
                Some(entry) => {
                    reply[..32].copy_from_slice(&entry.encode());
                    Ok(Done::Reply(32))
                }
                None => Ok(Done::Reply(0)),
            }
        }
        6 => {
            vfs.mkdir(reader.rest())?;
            Ok(Done::Reply(0))
        }
        7 => {
            vfs.remove(reader.rest())?;
            Ok(Done::Reply(0))
        }
        8 => {
            let length = reader.u16()? as usize;
            let from = reader.take(length)?;
            vfs.rename(from, reader.rest())?;
            Ok(Done::Reply(0))
        }
        9 => {
            reader.end()?;
            vfs.sync()?;
            Ok(Done::Reply(0))
        }
        10 => {
            if owner != 8 {
                return Err(Error::Denied);
            }
            let target = reader.byte()? as usize;
            reader.end()?;
            if !(1..=os::PROCESS_SLOTS).contains(&target) {
                return Err(Error::Invalid);
            }
            vfs.close_owner(target);
            Ok(Done::Reply(0))
        }
        // counts: (descriptors, open files, pipes) in use; for the shell's leak checks.
        14 => {
            reader.end()?;
            if owner != 3 {
                return Err(Error::Denied);
            }
            let (descriptors, opens, pipes) = vfs.counts();
            for (index, value) in [descriptors, opens, pipes].into_iter().enumerate() {
                reply[index * 2..index * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            Ok(Done::Reply(6))
        }
        _ => Err(Error::Invalid),
    }
}

/// Answers every parked request that has finished since the last call. The caller may
/// have died meanwhile; a refused reply to it is not an error.
fn deliver(vfs: &mut Vfs<FsClient>, reply: &mut [u8; PAYLOAD + 1]) {
    while let Some(wake) = vfs.next_wake() {
        let length = match wake.result {
            Ok(count) if wake.reading => {
                reply[0] = 0;
                reply[1..count + 1].copy_from_slice(&wake.data[..count]);
                count + 1
            }
            Ok(count) => {
                reply[0] = 0;
                reply[1..5].copy_from_slice(&(count as u32).to_le_bytes());
                5
            }
            Err(error) => {
                reply[0] = error as u8;
                1
            }
        };
        let _ = os::reply_live(wake.token, &reply[..length]);
    }
}

static mut TABLE: Option<Vfs<FsClient>> = None;
#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(VFS));
    // SAFETY: this server is single-threaded and this is the only reference; the
    // table is large (pipe buffers and parked writes) so it lives in static storage.
    let vfs = unsafe { (*core::ptr::addr_of_mut!(TABLE)).insert(Vfs::new(FsClient)) };
    assert_eq!(vfs.stat(b"/").unwrap().kind, 1);
    os::debug_write("VFS ready: root mount, owned FDs, bounded copied IPC").unwrap();
    let mut request = [0; os::MAX_STORAGE_MESSAGE];
    let mut reply = [0; PAYLOAD + 1];
    loop {
        let (token, length) = os::receive(VFS, &mut request).unwrap();
        let result = handle(
            vfs,
            token & 0xffff,
            token,
            &request[..length],
            &mut reply[1..],
        );
        match result {
            Ok(Done::Reply(count)) => {
                reply[0] = 0;
                os::reply_live(token, &reply[..count + 1]).unwrap();
            }
            Ok(Done::Parked) => (),
            Err(error) => {
                reply[0] = error as u8;
                os::reply_live(token, &reply[..1]).unwrap();
            }
        }
        deliver(vfs, &mut reply);
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
