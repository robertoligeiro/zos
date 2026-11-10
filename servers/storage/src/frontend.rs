#![no_std]
#![no_main]

use zos_libos as os;
use zos_storage::{
    protocol::{FsClient, Reader},
    vfs::Vfs,
    *,
};

fn handle(
    vfs: &mut Vfs<FsClient>,
    owner: usize,
    request: &[u8],
    reply: &mut [u8],
) -> Result<usize> {
    let mut reader = Reader::new(request);
    match reader.byte()? {
        0 => {
            let flags = reader.byte()?;
            let handle = vfs.open(owner, reader.rest(), flags)?;
            reply[..8].copy_from_slice(&handle.to_le_bytes());
            Ok(8)
        }
        1 => {
            let fd = reader.u64()?;
            let count = reader.u16()? as usize;
            reader.end()?;
            if count > PAYLOAD {
                return Err(Error::TooLarge);
            }
            vfs.read(owner, fd, &mut reply[..count])
        }
        2 => {
            let fd = reader.u64()?;
            let bytes = reader.rest();
            if bytes.len() > PAYLOAD {
                return Err(Error::TooLarge);
            }
            let count = vfs.write(owner, fd, bytes)?;
            reply[..4].copy_from_slice(&(count as u32).to_le_bytes());
            Ok(4)
        }
        3 => {
            let fd = reader.u64()?;
            reader.end()?;
            vfs.close(owner, fd)?;
            Ok(0)
        }
        4 => {
            let entry = vfs.stat(reader.rest())?;
            reply[..32].copy_from_slice(&entry.encode());
            Ok(32)
        }
        5 => {
            let ordinal = reader.u16()? as usize;
            match vfs.readdir(reader.rest(), ordinal)? {
                Some(entry) => {
                    reply[..32].copy_from_slice(&entry.encode());
                    Ok(32)
                }
                None => Ok(0),
            }
        }
        6 => {
            vfs.mkdir(reader.rest())?;
            Ok(0)
        }
        7 => {
            vfs.remove(reader.rest())?;
            Ok(0)
        }
        8 => {
            let length = reader.u16()? as usize;
            let from = reader.take(length)?;
            vfs.rename(from, reader.rest())?;
            Ok(0)
        }
        9 => {
            reader.end()?;
            vfs.sync()?;
            Ok(0)
        }
        _ => Err(Error::Invalid),
    }
}
#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(VFS));
    let mut vfs = Vfs::new(FsClient);
    assert_eq!(vfs.stat(b"/").unwrap().kind, 1);
    os::debug_write("VFS ready: root mount, owned FDs, bounded copied IPC").unwrap();
    let mut request = [0; os::MAX_STORAGE_MESSAGE];
    let mut reply = [0; PAYLOAD + 1];
    loop {
        let (token, length) = os::receive(VFS, &mut request).unwrap();
        let count = match handle(
            &mut vfs,
            token & 0xffff,
            &request[..length],
            &mut reply[1..],
        ) {
            Ok(count) => {
                reply[0] = 0;
                count + 1
            }
            Err(error) => {
                reply[0] = error as u8;
                1
            }
        };
        os::reply(token, &reply[..count]).unwrap();
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
