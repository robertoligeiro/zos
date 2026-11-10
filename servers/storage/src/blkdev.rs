#![no_std]
#![no_main]

use core::arch::asm;
use zos_libos as os;
use zos_storage::{protocol::Reader, *};

const DATA: u16 = 0x170;
const STATUS: u16 = 0x177;
const CONTROL: u16 = 0x376;

// These helpers are private to the driver that has the exact port grants.
fn input(port: u16) -> u8 {
    let value;
    // SAFETY: private driver helper; startup grants all reachable input ports.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}
fn output(port: u16, value: u8) {
    // SAFETY: private driver helper; startup grants all reachable output ports.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}
fn delay() {
    for _ in 0..4 {
        let _ = input(CONTROL);
    }
}
fn wait(data: bool) -> Result<()> {
    for _ in 0..1_000_000 {
        let status = input(CONTROL);
        if matches!(status, 0 | 255) {
            return Err(Error::Io);
        }
        if status & 0x80 != 0 {
            core::hint::spin_loop();
            continue;
        }
        if status & 0x21 != 0 {
            return Err(Error::Io);
        }
        if (status & 8 != 0) == data {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Error::Io)
}
fn read_sector(bytes: &mut [u8; SECTOR]) {
    for pair in bytes.chunks_exact_mut(2) {
        let word: u16;
        // SAFETY: the granted ATA data port transfers exactly one 16-bit word.
        unsafe {
            asm!("in ax, dx", in("dx") DATA, out("ax") word, options(nomem, nostack));
        }
        pair.copy_from_slice(&word.to_le_bytes());
    }
}
fn write_sector(bytes: &[u8; SECTOR]) {
    for pair in bytes.chunks_exact(2) {
        let word = u16::from_le_bytes(pair.try_into().unwrap());
        // SAFETY: the granted ATA data port accepts exactly one 16-bit word.
        unsafe {
            asm!("out dx, ax", in("dx") DATA, in("ax") word, options(nomem, nostack));
        }
    }
}
struct Ata;
impl Ata {
    fn identify() -> Result<Self> {
        output(CONTROL, 2);
        output(0x176, 0xa0);
        delay();
        wait(false)?;
        for port in 0x172..=0x175 {
            output(port, 0);
        }
        output(STATUS, 0xec);
        delay();
        wait(true)?;
        if input(0x174) != 0 || input(0x175) != 0 {
            return Err(Error::Invalid);
        }
        let mut bytes = [0; SECTOR];
        read_sector(&mut bytes);
        wait(false)?;
        let word = |i: usize| u16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]);
        if word(0) & 0x8000 != 0
            || word(49) & 0x200 == 0
            || word(83) & 0x1000 == 0
            || ((word(61) as u32) << 16 | word(60) as u32) < SECTORS
        {
            return Err(Error::Invalid);
        }
        Ok(Self)
    }
    fn command(&mut self, sector: u32, command: u8) -> Result<()> {
        if sector >= SECTORS {
            return Err(Error::Invalid);
        }
        wait(false)?;
        output(0x176, 0xe0 | ((sector >> 24) as u8 & 15));
        delay();
        output(0x172, 1);
        output(0x173, sector as u8);
        output(0x174, (sector >> 8) as u8);
        output(0x175, (sector >> 16) as u8);
        output(STATUS, command);
        delay();
        wait(true)
    }
}
impl Disk for Ata {
    fn read(&mut self, sector: u32, bytes: &mut [u8; SECTOR]) -> Result<()> {
        self.command(sector, 0x20)?;
        read_sector(bytes);
        delay();
        wait(false)
    }
    fn write(&mut self, sector: u32, bytes: &[u8; SECTOR]) -> Result<()> {
        self.command(sector, 0x30)?;
        write_sector(bytes);
        delay();
        wait(false)
    }
    fn flush(&mut self) -> Result<()> {
        wait(false)?;
        output(STATUS, 0xe7);
        delay();
        wait(false)
    }
}

fn handle(ata: &mut Ata, request: &[u8], reply: &mut [u8]) -> Result<usize> {
    let mut reader = Reader::new(request);
    match reader.byte()? {
        0 => {
            let sector = reader.u32()?;
            reader.end()?;
            ata.read(sector, (&mut reply[..SECTOR]).try_into().unwrap())?;
            Ok(SECTOR)
        }
        1 => {
            let sector = reader.u32()?;
            let bytes = reader.take(SECTOR)?;
            reader.end()?;
            ata.write(sector, bytes.try_into().unwrap())?;
            Ok(0)
        }
        2 => {
            reader.end()?;
            ata.flush()?;
            Ok(0)
        }
        _ => Err(Error::Invalid),
    }
}
#[no_mangle]
pub extern "C" fn _start(_: usize) -> ! {
    assert_eq!(os::channel_create(), Ok(BLKDEV));
    os::io_port_grant(DATA, 8).unwrap();
    os::io_port_grant(CONTROL, 1).unwrap();
    assert_eq!(os::io_port_grant(0x1f0, 8), Err(os::Error::Denied));
    let mut ata = Ata::identify().expect("Secondary ATA data disk unavailable");
    os::debug_write("BLOCK ready: secondary ATA, LBA28, flush, 32768 sectors").unwrap();
    let mut request = [0; os::MAX_STORAGE_MESSAGE];
    let mut reply = [0; SECTOR + 1];
    loop {
        let (token, length) = os::receive(BLKDEV, &mut request).unwrap();
        let result = if token & 0xffff != FILESYSTEM {
            Err(Error::Denied)
        } else {
            handle(&mut ata, &request[..length], &mut reply[1..])
        };
        let length = match result {
            Ok(count) => {
                reply[0] = 0;
                count + 1
            }
            Err(error) => {
                reply[0] = error as u8;
                if error == Error::Io {
                    os::debug_write("BLOCK IO ERROR").unwrap();
                }
                1
            }
        };
        os::reply(token, &reply[..length]).unwrap();
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    fail(info)
}
