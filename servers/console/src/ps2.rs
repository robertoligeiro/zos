#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Absent,
    Timeout,
    Controller,
    Response(u8),
    Flood,
}

pub trait Bus {
    fn read(&mut self, port: u16) -> u8;
    fn write(&mut self, port: u16, value: u8);
    fn ticks(&self) -> u64;
}

pub struct Devices {
    pub keyboard: Result<(), Error>,
    pub mouse: Result<(), Error>,
}

fn write(bus: &mut impl Bus, port: u16, value: u8) -> Result<(), Error> {
    let deadline = bus.ticks() + 10;
    for _ in 0..1_000_000 {
        let status = bus.read(0x64);
        if status == 0xff {
            return Err(Error::Absent);
        }
        if status & 2 == 0 {
            bus.write(port, value);
            return Ok(());
        }
        if bus.ticks() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
    Err(Error::Timeout)
}

fn response(bus: &mut impl Bus, mouse: bool) -> Result<u8, Error> {
    let deadline = bus.ticks() + 10;
    for _ in 0..1_000_000 {
        let status = bus.read(0x64);
        if status == 0xff {
            return Err(Error::Absent);
        }
        if status & 1 != 0 {
            let byte = bus.read(0x60);
            if status & 0xc0 != 0 || (status & 0x20 != 0) != mouse {
                return Err(Error::Controller);
            }
            return Ok(byte);
        }
        if bus.ticks() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
    Err(Error::Timeout)
}

fn command(bus: &mut impl Bus, mouse: bool, value: u8) -> Result<(), Error> {
    for _ in 0..3 {
        if mouse {
            write(bus, 0x64, 0xd4)?;
        }
        write(bus, 0x60, value)?;
        match response(bus, mouse)? {
            0xfa => return Ok(()),
            0xfe => (),
            byte => return Err(Error::Response(byte)),
        }
    }
    Err(Error::Response(0xfe))
}

pub fn initialize(bus: &mut impl Bus) -> Result<Devices, Error> {
    if bus.read(0x64) == 0xff {
        return Err(Error::Absent);
    }
    write(bus, 0x64, 0xad)?;
    write(bus, 0x64, 0xa7)?;
    drain(bus, |_, _| ())?;
    write(bus, 0x64, 0x20)?;
    let configuration = response(bus, false)?;
    if configuration == 0xff {
        return Err(Error::Absent);
    }
    write(bus, 0x64, 0x60)?;
    write(bus, 0x60, (configuration & !0x73) | 0x30)?;
    write(bus, 0x64, 0xae)?;
    let keyboard = (|| {
        command(bus, false, 0xf5)?;
        command(bus, false, 0xf0)?;
        command(bus, false, 1)
    })();
    if keyboard.is_err() {
        write(bus, 0x64, 0xad)?;
    }
    write(bus, 0x64, 0xa8)?;
    let mouse = (|| {
        command(bus, true, 0xf5)?;
        command(bus, true, 0xf6)?;
        command(bus, true, 0xf2)?;
        match response(bus, true)? {
            0 => Ok(()),
            id => Err(Error::Response(id)),
        }
    })();
    if mouse.is_err() {
        write(bus, 0x64, 0xa7)?;
    }
    write(bus, 0x64, 0x60)?;
    write(
        bus,
        0x60,
        (configuration & !0x73)
            | if keyboard.is_err() { 0x10 } else { 0 }
            | if mouse.is_err() { 0x20 } else { 0 },
    )?;
    let keyboard = keyboard.and_then(|()| command(bus, false, 0xf4));
    let mouse = mouse.and_then(|()| command(bus, true, 0xf4));
    Ok(Devices { keyboard, mouse })
}

pub fn drain(bus: &mut impl Bus, mut byte: impl FnMut(bool, u8)) -> Result<(), Error> {
    for _ in 0..256 {
        let status = bus.read(0x64);
        if status == 0xff {
            return Err(Error::Absent);
        }
        if status & 1 == 0 {
            return Ok(());
        }
        let value = bus.read(0x60);
        if status & 0xc0 != 0 {
            return Err(Error::Controller);
        }
        byte(status & 0x20 != 0, value);
    }
    Err(Error::Flood)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque};

    struct Mock {
        keyboard: bool,
        mouse: bool,
        absent: bool,
        configuration: bool,
        auxiliary: bool,
        queue: VecDeque<(bool, u8)>,
        clock: Cell<u64>,
    }
    impl Bus for Mock {
        fn read(&mut self, port: u16) -> u8 {
            if self.absent {
                return 0xff;
            }
            if port == 0x64 {
                self.queue
                    .front()
                    .map_or(0, |&(mouse, _)| 1 | if mouse { 0x20 } else { 0 })
            } else {
                self.queue.pop_front().unwrap().1
            }
        }
        fn write(&mut self, port: u16, value: u8) {
            if port == 0x64 {
                match value {
                    0x20 => self.queue.push_back((false, 0x30)),
                    0x60 => self.configuration = true,
                    0xd4 => self.auxiliary = true,
                    _ => (),
                }
            } else if self.configuration {
                assert_eq!(value & 0x43, 0, "translation and IRQs must remain disabled");
                self.configuration = false;
            } else {
                let auxiliary = core::mem::take(&mut self.auxiliary);
                if if auxiliary { self.mouse } else { self.keyboard } {
                    self.queue.push_back((auxiliary, 0xfa));
                    if auxiliary && value == 0xf2 {
                        self.queue.push_back((true, 0));
                    }
                }
            }
        }
        fn ticks(&self) -> u64 {
            let tick = self.clock.get();
            self.clock.set(tick + 1);
            tick
        }
    }
    #[test]
    fn integrated_keyboard_does_not_require_a_ps2_mouse() {
        for (keyboard, mouse) in [(true, false), (false, true), (true, true), (false, false)] {
            let mut bus = Mock {
                keyboard,
                mouse,
                absent: false,
                configuration: false,
                auxiliary: false,
                queue: VecDeque::new(),
                clock: Cell::new(0),
            };
            let devices = initialize(&mut bus).unwrap();
            assert_eq!(devices.keyboard.is_ok(), keyboard);
            assert_eq!(devices.mouse.is_ok(), mouse);
            bus.queue.extend([(false, 0x1e), (true, 8)]);
            let mut bytes = std::vec::Vec::new();
            drain(&mut bus, |mouse, byte| bytes.push((mouse, byte))).unwrap();
            assert_eq!(bytes, [(false, 0x1e), (true, 8)]);
            bus.absent = true;
            assert!(matches!(initialize(&mut bus), Err(Error::Absent)));
        }
    }
}
