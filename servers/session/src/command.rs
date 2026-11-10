#![no_std]
#![no_main]

use zos_libos as os;
use zos_session::{client, *};
use zos_storage::{protocol as api, vfs::*, Error, Result};

fn output(fd: Option<u64>, bytes: &[u8]) -> Result<()> {
    if let Some(fd) = fd {
        write_all(fd, bytes)
    } else {
        console(bytes);
        Ok(())
    }
}

fn cat(cwd: &[u8], source: &[u8], out: Option<u64>) -> Result<()> {
    let source = path(cwd, source)?;
    let fd = api::open(source.bytes(), READ)?;
    let result = (|| {
        let mut bytes = [0; zos_storage::PAYLOAD];
        loop {
            let count = api::read(fd, &mut bytes)?;
            if count == 0 {
                return Ok(());
            }
            output(out, &bytes[..count])?;
        }
    })();
    let closed = api::close(fd);
    result.and(closed)
}

fn copy(cwd: &[u8], source: &[u8], destination: &[u8]) -> Result<()> {
    let source = path(cwd, source)?;
    let destination = path(cwd, destination)?;
    let source_entry = api::stat(source.bytes())?;
    match api::stat(destination.bytes()) {
        Ok(entry) if entry.inode == source_entry.inode => return Err(Error::Invalid),
        Ok(_) | Err(Error::NotFound) => (),
        Err(error) => return Err(error),
    }
    let read = api::open(source.bytes(), READ)?;
    let result = (|| {
        let write = api::open(destination.bytes(), WRITE | CREATE | TRUNCATE)?;
        let result = (|| {
            let mut bytes = [0; zos_storage::PAYLOAD];
            loop {
                let count = api::read(read, &mut bytes)?;
                if count == 0 {
                    return Ok(());
                }
                write_all(write, &bytes[..count])?;
            }
        })();
        let closed = api::close(write);
        result.and(closed)
    })();
    let closed = api::close(read);
    result.and(closed)
}

fn execute(inv: &client::Invocation<'_>, out: Option<u64>) -> Result<i64> {
    let name = inv.argv[0].rsplit(|&b| b == b'/').next().unwrap();
    match name {
        b"echo" => {
            for (i, arg) in inv.argv[1..inv.argc].iter().enumerate() {
                if i != 0 {
                    output(out, b" ")?;
                }
                output(out, arg)?;
            }
            output(out, b"\n")?;
        }
        b"cat" if inv.argc >= 2 => {
            for arg in &inv.argv[1..inv.argc] {
                cat(inv.cwd, arg, out)?;
            }
        }
        b"cp" if inv.argc == 3 => copy(inv.cwd, inv.argv[1], inv.argv[2])?,
        b"true" if inv.argc == 1 => (),
        b"false" if inv.argc == 1 => return Ok(1),
        b"uname" if inv.argc == 1 => output(out, b"ZOS - Zombie OS (x86_64)\n")?,
        b"spin" if inv.argc == 1 => loop {
            core::hint::spin_loop();
        },
        b"block" | b"leak" if inv.argc == 2 => {
            api::open(path(inv.cwd, inv.argv[1])?.bytes(), WRITE | CREATE)?;
            os::map_memory(0x1000_0000, true, false).unwrap();
            if name == b"block" {
                let channel = os::channel_create().unwrap();
                api::rpc(PROCMGR, &[8], &mut [0]).unwrap();
                let mut bytes = [0; 64];
                os::receive(channel, &mut bytes).unwrap();
                return Err(Error::Invalid);
            }
        }
        b"fault" if inv.argc == 1 => {
            // SAFETY: dedicated ring-3 fault probe verifies kernel containment
            // and process-manager fault status; no memory is accessed.
            unsafe { core::arch::asm!("ud2", options(noreturn)) };
        }
        _ => return Err(Error::Invalid),
    }
    Ok(0)
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    let startup = client::startup().unwrap();
    let inv = startup.decode().unwrap();
    assert_eq!(os::process_kill(3), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x170, 8), Err(os::Error::Denied));
    let result = (|| {
        if !inv.output.is_empty() && inv.argv[0].rsplit(|&b| b == b'/').next() == Some(b"cat") {
            match api::stat(inv.output) {
                Ok(destination) => {
                    for arg in &inv.argv[1..inv.argc] {
                        if api::stat(path(inv.cwd, arg)?.bytes())?.inode == destination.inode {
                            return Err(Error::Invalid);
                        }
                    }
                }
                Err(Error::NotFound) => (),
                Err(error) => return Err(error),
            }
        }
        let out = if inv.output.is_empty() {
            None
        } else {
            Some(api::open(
                inv.output,
                WRITE | CREATE | if inv.append { APPEND } else { TRUNCATE },
            )?)
        };
        let result = execute(&inv, out);
        let closed = out.map_or(Ok(()), api::close);
        result.and_then(|status| closed.map(|_| status))
    })();
    match result {
        Ok(status) => os::exit(status),
        Err(error) => {
            zos_console::log(format_args!("COMMAND ERROR: {error:?}"));
            console(b"command failed; see serial COMMAND ERROR\n");
            os::exit(1)
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
