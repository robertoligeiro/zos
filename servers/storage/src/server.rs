#![no_std]
#![no_main]

use zos_libos as os;
use zos_storage::{fs::Filesystem, protocol::Reader, vfs::Entry, *};

fn entry(fs: &Filesystem<BlockClient>, id: usize, reply: &mut [u8]) -> Result<usize> {
    reply[..32].copy_from_slice(&Entry::from_node(id, fs.stat(id)?).encode());
    Ok(32)
}
fn handle(fs: &mut Filesystem<BlockClient>, request: &[u8], reply: &mut [u8]) -> Result<usize> {
    let mut reader = Reader::new(request);
    match reader.byte()? {
        0 => {
            let id = fs.lookup(reader.rest())?;
            entry(fs, id, reply)
        }
        1 => {
            let kind = reader.byte()?;
            let id = fs.create(reader.rest(), kind)?;
            entry(fs, id, reply)
        }
        2 => {
            let id = reader.u16()? as usize;
            let offset = reader.u32()? as usize;
            let count = reader.u16()? as usize;
            reader.end()?;
            if count > PAYLOAD {
                return Err(Error::TooLarge);
            }
            fs.read(id, offset, &mut reply[..count])
        }
        3 => {
            let id = reader.u16()? as usize;
            let offset = reader.u32()? as usize;
            let data = reader.rest();
            if data.len() > PAYLOAD {
                return Err(Error::TooLarge);
            }
            let count = fs.write(id, offset, data)?;
            reply[..4].copy_from_slice(&(count as u32).to_le_bytes());
            Ok(4)
        }
        4 => {
            fs.remove(reader.rest())?;
            Ok(0)
        }
        5 => {
            let length = reader.u16()? as usize;
            let from = reader.take(length)?;
            fs.rename(from, reader.rest())?;
            Ok(0)
        }
        6 => {
            let id = reader.u16()? as usize;
            let ordinal = reader.u16()? as usize;
            reader.end()?;
            match fs.readdir(id, ordinal)? {
                Some((id, node)) => {
                    reply[..32].copy_from_slice(&Entry::from_node(id, node).encode());
                    Ok(32)
                }
                None => Ok(0),
            }
        }
        7 => {
            let id = reader.u16()? as usize;
            reader.end()?;
            fs.truncate(id)?;
            Ok(0)
        }
        8 => {
            reader.end()?;
            fs.sync()?;
            Ok(0)
        }
        9 => {
            let id = reader.u16()? as usize;
            reader.end()?;
            entry(fs, id, reply)
        }
        _ => Err(Error::Invalid),
    }
}
#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(FILESYSTEM));
    let mut fs = Filesystem::mount(BlockClient)
        .expect("Invalid or unreadable filesystem; refusing to format");
    zos_console::log(format_args!(
        "FS ready: checkpoint={}, COW metadata/data",
        fs.metadata.generation
    ));
    let mut request = [0; os::MAX_STORAGE_MESSAGE];
    let mut reply = [0; PAYLOAD + 1];
    loop {
        let (token, length) = os::receive(FILESYSTEM, &mut request).unwrap();
        let result = if token & 0xffff != VFS {
            Err(Error::Denied)
        } else {
            handle(&mut fs, &request[..length], &mut reply[1..])
        };
        let count = match result {
            Ok(count) => {
                reply[0] = 0;
                count + 1
            }
            Err(error) => {
                reply[0] = error as u8;
                if error == Error::Io {
                    os::debug_write("FILESYSTEM IO ERROR").unwrap();
                }
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
