#![no_std]
#![no_main]

use zos_libos as os;
use zos_storage::{protocol as api, vfs::*, *};

const FILE: &[u8] = b"/persist/reboot.bin";
const LENGTH: usize = 8197;
fn pattern(index: usize) -> u8 {
    ((index * 37 + 11) % 251) as u8
}

fn verify_file() {
    assert_eq!(api::stat(FILE).unwrap().size as usize, LENGTH);
    let fd = api::open(b"//persist/./child/../reboot.bin", READ).unwrap();
    let mut buffer = [0; 997];
    let mut position = 0;
    loop {
        let count = api::read(fd, &mut buffer).unwrap();
        if count == 0 {
            break;
        }
        for (index, &byte) in buffer[..count].iter().enumerate() {
            assert_eq!(byte, pattern(position + index));
        }
        position += count;
    }
    assert_eq!(position, LENGTH);
    assert_eq!(api::write(fd, b"denied"), Err(Error::Denied));
    api::close(fd).unwrap();
    assert_eq!(api::read(fd, &mut buffer), Err(Error::BadFd));
}
extern "C" fn foreign(fd: usize) -> ! {
    assert_eq!(api::close(fd as u64), Err(Error::BadFd));
    assert_eq!(api::write(fd as u64, b"foreign"), Err(Error::BadFd));
    os::debug_write("FD OWNER PASS: another thread cannot use or close the handle").unwrap();
    assert_eq!(os::send(7, b"foreign verified", &mut []), Ok(0));
    os::exit(0)
}
fn boundaries(fd: u64) {
    assert_eq!(
        os::debug_write(core::str::from_utf8(&[b'x'; os::MAX_MESSAGE + 1]).unwrap()),
        Err(os::Error::TooLarge)
    );
    assert_eq!(os::io_port_grant(0x170, 8), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x376, 1), Err(os::Error::Denied));
    assert_eq!(os::io_port_grant(0x1f0, 8), Err(os::Error::Denied));
    assert_eq!(os::framebuffer_map().unwrap_err(), os::Error::Denied);
    assert_eq!(
        api::rpc(BLKDEV, &[0, 0, 0, 0, 0], &mut [0; 513]),
        Err(Error::Denied)
    );
    assert_eq!(
        api::rpc(FILESYSTEM, b"\0/", &mut [0; 33]),
        Err(Error::Denied)
    );
    assert_eq!(api::rpc(VFS, &[255], &mut [0]), Err(Error::Invalid));
    assert_eq!(api::rpc(VFS, &[1], &mut [0]), Err(Error::Invalid));
    assert_eq!(
        os::send(VFS, &[0; os::MAX_STORAGE_MESSAGE + 1], &mut []),
        Err(os::Error::TooLarge)
    );
    let mut oversized = api::Request::new(1);
    oversized.u64(fd).unwrap();
    oversized.u16(1001).unwrap();
    assert_eq!(
        api::rpc(VFS, oversized.bytes(), &mut [0]),
        Err(Error::TooLarge)
    );
    assert_eq!(os::channel_create(), Ok(7));
    os::thread_create(foreign, fd as usize, 6).unwrap();
    let mut message = [0; 64];
    let (token, count) = os::receive(7, &mut message).unwrap();
    assert_eq!(&message[..count], b"foreign verified");
    os::reply(token, &[]).unwrap();
    os::debug_write("STORAGE CAPABILITIES PASS: ATA, backend and bulk boundaries").unwrap();
}
fn crud() {
    api::mkdir(b"/work").unwrap();
    api::mkdir(b"/work/sub").unwrap();
    assert_eq!(api::mkdir(b"/work/sub"), Err(Error::Exists));
    assert_eq!(api::unlink(b"/work"), Err(Error::NotEmpty));
    assert_eq!(api::open(b"/persist", READ), Err(Error::IsDir));
    assert_eq!(api::stat(b"/../../escape"), Err(Error::Invalid));
    assert_eq!(
        api::rename(b"/work", b"/work/sub/loop"),
        Err(Error::Invalid)
    );
    let fd = api::open(b"/work/sub/file", WRITE | CREATE).unwrap();
    assert_eq!(api::read(fd, &mut [0; 1]), Err(Error::Denied));
    assert_eq!(api::write(fd, b"temporary").unwrap(), 9);
    assert_eq!(api::unlink(b"/work/sub/file"), Err(Error::Denied));
    api::rename(b"/work/sub/file", b"/work/sub/renamed").unwrap();
    assert_eq!(api::stat(b"/work/sub/file"), Err(Error::NotFound));
    assert_eq!(api::stat(b"/work/sub/renamed").unwrap().size, 9);
    api::close(fd).unwrap();
    let replacement = api::open(b"/work/sub/renamed", WRITE | TRUNCATE).unwrap();
    assert_ne!(fd, replacement);
    assert_eq!(api::close(fd), Err(Error::BadFd));
    assert_eq!(api::stat(b"/work/sub/renamed").unwrap().size, 0);
    api::write(replacement, b"new").unwrap();
    api::close(replacement).unwrap();
    let fd = api::open(b"/work/sub/renamed", WRITE | APPEND).unwrap();
    api::write(fd, b"!").unwrap();
    api::close(fd).unwrap();
    assert_eq!(api::stat(b"/work/sub/renamed").unwrap().size, 4);
    assert_eq!(api::rename(b"/work/sub/renamed", FILE), Err(Error::Exists));
    let entry = api::readdir(b"/work/sub", 0).unwrap().unwrap();
    assert_eq!(&entry.name[..entry.length as usize], b"renamed");
    assert_eq!(api::readdir(b"/work/sub", 1), Ok(None));
    api::unlink(b"/work/sub/renamed").unwrap();
    api::unlink(b"/work/sub").unwrap();
    api::unlink(b"/work").unwrap();
    let mut descriptors = [0; 64];
    for fd in &mut descriptors {
        *fd = api::open(FILE, READ).unwrap();
    }
    assert_eq!(api::open(FILE, READ), Err(Error::NoSpace));
    for fd in descriptors {
        api::close(fd).unwrap();
    }
    os::debug_write("VFS CRUD PASS: paths, stat, readdir, rename, unlink, limits").unwrap();
}
fn create() {
    api::mkdir(b"/persist").unwrap();
    let fd = api::open(FILE, READ | WRITE | CREATE).unwrap();
    let mut buffer = [0; PAYLOAD];
    let mut position = 0;
    while position < LENGTH {
        let count = PAYLOAD.min(LENGTH - position);
        for (index, byte) in buffer[..count].iter_mut().enumerate() {
            *byte = pattern(position + index);
        }
        assert_eq!(api::write(fd, &buffer[..count]).unwrap(), count);
        position += count;
    }
    assert_eq!(api::unlink(FILE), Err(Error::Denied));
    boundaries(fd);
    api::close(fd).unwrap();
    crud();
    verify_file();
    api::sync().unwrap();
    os::debug_write("FS CREATE PASS: 8197 patterned bytes durably committed").unwrap();
}
fn normal() {
    match api::stat(b"/welcome.txt") {
        Ok(_) => (),
        Err(Error::NotFound) => {
            let fd = api::open(b"/welcome.txt", WRITE | CREATE).unwrap();
            let text = b"Welcome to ZOS - Zombie OS.\nPersistent storage is ready.\n";
            assert_eq!(api::write(fd, text).unwrap(), text.len());
            api::close(fd).unwrap();
        }
        Err(error) => panic!("welcome.txt stat failed: {error:?}"),
    }
    let size = api::stat(b"/welcome.txt").unwrap().size as usize;
    let fd = api::open(b"/welcome.txt", READ).unwrap();
    let mut total = 0;
    loop {
        let count = api::read(fd, &mut [0; PAYLOAD]).unwrap();
        if count == 0 {
            break;
        }
        total += count;
    }
    assert_eq!(total, size);
    api::close(fd).unwrap();
    assert!(api::readdir(b"/", 0).unwrap().is_some());
    api::sync().unwrap();
    assert_eq!(os::storage_test_finish(), Err(os::Error::Denied));
    zos_console::output(b"\nPersistent filesystem mounted at /.\nwelcome.txt is stored on data.img.\nStep 6: storage ready; shell is Step 7.\n");
    os::send(zos_console::CONSOLE, &[zos_console::LINE, 0], &mut []).unwrap();
    os::debug_write("ZOS ready: persistent filesystem and VFS verified").unwrap();
}
#[no_mangle]
pub extern "C" fn _start(role: usize) -> ! {
    match role {
        0 => {
            normal();
            os::exit(0)
        }
        1 => create(),
        2 => {
            verify_file();
            assert_eq!(api::stat(b"/work"), Err(Error::NotFound));
            let entry = api::readdir(b"/persist", 0).unwrap().unwrap();
            assert_eq!(&entry.name[..entry.length as usize], b"reboot.bin");
            assert_eq!(api::readdir(b"/persist", 1), Ok(None));
            api::sync().unwrap();
            os::debug_write("FS REBOOT PASS: exact data and metadata survived QEMU restart")
                .unwrap();
        }
        _ => panic!("Invalid storage test role"),
    }
    os::storage_test_finish().unwrap();
    unreachable!()
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
