#![no_std]
#![no_main]

use zos_libos as os;
use zos_session::{client::Startup, *};
use zos_storage::{
    protocol::{self as api, Reader},
    vfs::*,
    Error, Result,
};

static mut IMAGE: [u8; 65536] = [0; 65536];
#[derive(Clone, Copy)]
struct Job {
    pid: u64,
    startup: Startup,
    status: Option<i64>,
    waiter: Option<usize>,
    started_waiter: Option<usize>,
    startup_sent: bool,
    blocked_ready: bool,
    /// The shell has finished preparing the process (descriptors granted); until
    /// then its request for the startup packet is parked in `startup_waiter`.
    released: bool,
    startup_waiter: Option<usize>,
}

fn read_image(path: &[u8], image: &mut [u8]) -> Result<usize> {
    let size = api::stat(path)?.size as usize;
    if size == 0 || size > image.len() {
        return Err(Error::TooLarge);
    }
    let fd = api::open(path, READ)?;
    let result = (|| {
        let mut length = 0;
        while length < size {
            let end = (length + zos_storage::PAYLOAD).min(size);
            let count = api::read(fd, &mut image[length..end])?;
            if count == 0 {
                return Err(Error::Corrupt);
            }
            length += count;
        }
        Ok(length)
    })();
    let closed = api::close(fd);
    result.and_then(|length| closed.map(|_| length))
}

fn install(image: &mut [u8]) -> Result<()> {
    match api::stat(b"/bin") {
        Ok(entry) if entry.kind == 1 => (),
        Ok(_) => return Err(Error::NotDir),
        Err(Error::NotFound) => api::mkdir(b"/bin")?,
        Err(error) => return Err(error),
    }
    let length = os::boot_program(image).map_err(kernel)?;
    for name in PROGRAMS {
        let executable = path(b"/bin", name)?;
        match api::stat(executable.bytes()) {
            Ok(_) => continue,
            Err(Error::NotFound) => (),
            Err(error) => return Err(error),
        }
        let fd = api::open(executable.bytes(), WRITE | CREATE)?;
        let result = write_all(fd, &image[..length]);
        let closed = api::close(fd);
        result.and(closed)?;
    }
    api::sync()
}

fn status_reply(token: usize, status: i64) {
    let mut bytes = [0; 9];
    bytes[1..].copy_from_slice(&status.to_le_bytes());
    os::reply_live(token, &bytes).unwrap();
}

fn poll(jobs: &mut [Option<Job>; 24]) -> Result<()> {
    for job in jobs {
        let Some(current) = job.as_mut() else {
            continue;
        };
        if current.status.is_none() {
            let info = os::process_info(((current.pid & 255) - 1) as usize).map_err(kernel)?;
            if info.pid != current.pid {
                return Err(Error::Corrupt);
            }
            if current.blocked_ready && info.state == 1 {
                if let Some(token) = current.started_waiter.take() {
                    os::reply_live(token, &[0]).unwrap();
                }
            }
            if info.state == 2 || info.state == 3 {
                api::rpc(zos_storage::VFS, &[10, (current.pid & 255) as u8], &mut [0])?;
                os::process_reap(current.pid).map_err(kernel)?;
                current.status = Some(info.status);
                if let Some(token) = current.started_waiter.take() {
                    os::reply_live(token, &[Error::Invalid as u8]).unwrap();
                }
            }
        }
        if let (Some(status), Some(token)) = (current.status, current.waiter) {
            status_reply(token, status);
            *job = None;
        }
    }
    Ok(())
}

fn handle(
    owner: usize,
    token: usize,
    request: &[u8],
    response: &mut [u8],
    jobs: &mut [Option<Job>; 24],
    image: &mut [u8],
) -> Result<Option<usize>> {
    let mut reader = Reader::new(request);
    match reader.byte()? {
        0 => {
            if owner != 3 {
                return Err(Error::Denied);
            }
            let length = reader.u16()? as usize;
            let executable = normalize(reader.take(length)?)?;
            let data = reader.rest();
            if data.len() > 768 {
                return Err(Error::TooLarge);
            }
            let mut startup = Startup {
                length: data.len(),
                ..Startup::default()
            };
            startup.bytes[..data.len()].copy_from_slice(data);
            startup.decode()?;
            let slot = jobs
                .iter()
                .position(Option::is_none)
                .ok_or(Error::NoSpace)?;
            let length = read_image(executable.bytes(), image)?;
            let name = executable
                .bytes()
                .rsplit(|&b| b == b'/')
                .next()
                .ok_or(Error::Invalid)?;
            let pid = os::process_spawn(&image[..length], name).map_err(kernel)?;
            jobs[slot] = Some(Job {
                pid,
                startup,
                status: None,
                waiter: None,
                started_waiter: None,
                startup_sent: false,
                blocked_ready: false,
                released: false,
                startup_waiter: None,
            });
            response[..8].copy_from_slice(&pid.to_le_bytes());
            Ok(Some(8))
        }
        1 | 7 => {
            if owner != 3 {
                return Err(Error::Denied);
            }
            let started = request[0] == 7;
            let pid = reader.u64()?;
            reader.end()?;
            let slot = jobs
                .iter()
                .position(|j| j.is_some_and(|j| j.pid == pid))
                .ok_or(Error::NotFound)?;
            let job = jobs[slot].as_mut().unwrap();
            if started {
                if job.status.is_some() {
                    return Err(Error::Invalid);
                }
                if job.started_waiter.is_some() {
                    return Err(Error::Denied);
                }
                job.started_waiter = Some(token);
                Ok(None)
            } else if let Some(status) = job.status {
                response[..8].copy_from_slice(&status.to_le_bytes());
                jobs[slot] = None;
                Ok(Some(8))
            } else {
                if job.waiter.is_some() {
                    return Err(Error::Denied);
                }
                job.waiter = Some(token);
                Ok(None)
            }
        }
        2 => {
            if owner != 3 {
                return Err(Error::Denied);
            }
            let pid = reader.u64()?;
            reader.end()?;
            os::process_kill(pid).map_err(kernel)?;
            Ok(Some(0))
        }
        3 => {
            let slot = reader.byte()? as usize;
            reader.end()?;
            match os::process_info(slot) {
                Ok(info) if info.state != 4 => {
                    response[..8].copy_from_slice(&info.pid.to_le_bytes());
                    response[8] = info.state;
                    response[9] = info.system as u8;
                    response[10..18].copy_from_slice(&info.memory.to_le_bytes());
                    response[18..26].copy_from_slice(&info.status.to_le_bytes());
                    response[26..50].copy_from_slice(&info.name);
                    Ok(Some(50))
                }
                Ok(_) | Err(os::Error::Invalid) => Ok(Some(0)),
                Err(error) => Err(kernel(error)),
            }
        }
        4 => {
            reader.end()?;
            for (i, value) in os::memory_stats().map_err(kernel)?.into_iter().enumerate() {
                response[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            Ok(Some(32))
        }
        5 => {
            reader.end()?;
            let job = jobs
                .iter_mut()
                .flatten()
                .find(|j| j.pid & 255 == owner as u64 && j.status.is_none())
                .ok_or(Error::Denied)?;
            if !job.released {
                // Not yet: the parent is still handing this process its descriptors.
                if job.startup_waiter.is_some() {
                    return Err(Error::Denied);
                }
                job.startup_waiter = Some(token);
                return Ok(None);
            }
            response[..job.startup.length]
                .copy_from_slice(&job.startup.bytes[..job.startup.length]);
            job.startup_sent = true;
            Ok(Some(job.startup.length))
        }
        10 => {
            if owner != 3 {
                return Err(Error::Denied);
            }
            let pid = reader.u64()?;
            reader.end()?;
            let job = jobs
                .iter_mut()
                .flatten()
                .find(|j| j.pid == pid)
                .ok_or(Error::NotFound)?;
            job.released = true;
            if let Some(waiter) = job.startup_waiter.take() {
                let mut packet = [0; 769];
                packet[1..=job.startup.length]
                    .copy_from_slice(&job.startup.bytes[..job.startup.length]);
                job.startup_sent = true;
                // The child may have been killed while waiting; that is not an error.
                let _ = os::reply_live(waiter, &packet[..job.startup.length + 1]);
            }
            Ok(Some(0))
        }
        6 => {
            if owner != 3 {
                return Err(Error::Denied);
            }
            let action = reader.byte()?;
            reader.end()?;
            api::sync()?;
            os::system_control(action as u64).map_err(kernel)?;
            Ok(Some(0))
        }
        9 => {
            // Kernel/service log text for the shell's dmesg: 8-byte next offset, then data.
            if owner != 3 {
                return Err(Error::Denied);
            }
            let offset = reader.u64()?;
            reader.end()?;
            let (next, count) = os::log_read(offset, &mut response[8..968]).map_err(kernel)?;
            response[..8].copy_from_slice(&next.to_le_bytes());
            Ok(Some(8 + count))
        }
        8 => {
            reader.end()?;
            let job = jobs
                .iter_mut()
                .flatten()
                .find(|j| j.pid & 255 == owner as u64 && j.status.is_none() && j.startup_sent)
                .ok_or(Error::Denied)?;
            job.blocked_ready = true;
            Ok(Some(0))
        }
        _ => Err(Error::Invalid),
    }
}

#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(PROCMGR));
    os::irq_attach(PROCMGR, 0).unwrap();
    // SAFETY: only this single manager thread accesses IMAGE; no reference is
    // retained by IPC or by the kernel's copy-based ELF loader after a call.
    let image = unsafe { &mut *core::ptr::addr_of_mut!(IMAGE) };
    install(image).expect("Bundled program installation failed");
    os::debug_write("PROCMGR ready: path spawn, kill, wait, stats, reusable PIDs").unwrap();
    os::send(zos_console::TTY, b"session-ready", &mut []).unwrap();
    let mut jobs = [None; 24];
    let mut request = [0; 1024];
    let mut response = [0; 1024];
    loop {
        let (token, length) = os::receive(PROCMGR, &mut request).unwrap();
        poll(&mut jobs).expect("Process lifecycle cleanup failed");
        if token == 0 {
            continue;
        }
        match handle(
            token & 0xffff,
            token,
            &request[..length],
            &mut response[1..],
            &mut jobs,
            image,
        ) {
            Ok(Some(length)) => {
                response[0] = 0;
                os::reply_live(token, &response[..length + 1]).unwrap();
            }
            Ok(None) => (),
            Err(error) => {
                os::reply_live(token, &[error as u8]).unwrap();
            }
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
