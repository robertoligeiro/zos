//! Synopsys DesignWare I2C master, as used by Intel serial-IO (LPSS) controllers.
//! Polled: no interrupts, no DMA. Standard register map from the DW_apb_i2c databook.

use super::{Bus, Error, Regs, Result};

const IC_CON: usize = 0x00;
const IC_TAR: usize = 0x04;
const IC_DATA_CMD: usize = 0x10;
const IC_SS_SCL_HCNT: usize = 0x14;
const IC_SS_SCL_LCNT: usize = 0x18;
const IC_FS_SCL_HCNT: usize = 0x1c;
const IC_FS_SCL_LCNT: usize = 0x20;
const IC_INTR_MASK: usize = 0x30;
const IC_RAW_INTR_STAT: usize = 0x34;
const IC_RX_TL: usize = 0x38;
const IC_TX_TL: usize = 0x3c;
const IC_CLR_INTR: usize = 0x40;
const IC_CLR_TX_ABRT: usize = 0x54;
const IC_ENABLE: usize = 0x6c;
const IC_STATUS: usize = 0x70;
const IC_TX_ABRT_SOURCE: usize = 0x80;
const IC_ENABLE_STATUS: usize = 0x9c;
const IC_COMP_PARAM_1: usize = 0xf4;
const IC_COMP_TYPE: usize = 0xfc;
/// Intel LPSS private register: bits 0 and 1 release the controller from reset.
const LPSS_RESETS: usize = 0x204;

const COMP_TYPE: u32 = 0x4457_0140; // "DW" 0x0140
const CMD_READ: u32 = 0x100;
const CMD_STOP: u32 = 0x200;
const CMD_RESTART: u32 = 0x400;
const RAW_TX_ABRT: u32 = 1 << 6;
const STATUS_ACTIVITY: u32 = 1 << 0;
const STATUS_TX_NOT_FULL: u32 = 1 << 1;
const STATUS_TX_EMPTY: u32 = 1 << 2;
const STATUS_RX_NOT_EMPTY: u32 = 1 << 3;

/// Read commands in flight at once; below the smallest RX FIFO we accept.
const MAX_OUTSTANDING: usize = 8;
/// A transfer gets this many 10 ms ticks (plus one for the tick boundary).
const TIMEOUT_TICKS: u64 = 5;

pub struct DesignWare<R: Regs> {
    regs: R,
    tx_depth: usize,
}

impl<R: Regs> DesignWare<R> {
    /// Releases the controller from reset, checks its signature and configures it as
    /// a 7-bit master at the speed the firmware already set up. Returns the controller
    /// and a description (version, timing registers) for the boot log.
    pub fn new(mut regs: R) -> Result<(Self, u32, u32)> {
        // LPSS: the controller sits in reset until both bits are set.
        regs.write(LPSS_RESETS, 3);
        let signature = regs.read(IC_COMP_TYPE);
        if signature != COMP_TYPE {
            return Err(Error::NotDesignWare);
        }
        let parameters = regs.read(IC_COMP_PARAM_1);
        let tx_depth = ((parameters >> 16 & 0xff) + 1) as usize;
        let mut controller = Self { regs, tx_depth };
        controller.disable()?;
        let fast =
            controller.regs.read(IC_FS_SCL_HCNT) != 0 && controller.regs.read(IC_FS_SCL_LCNT) != 0;
        let standard =
            controller.regs.read(IC_SS_SCL_HCNT) != 0 && controller.regs.read(IC_SS_SCL_LCNT) != 0;
        if !fast && !standard {
            // Firmware left no timing: about 100 kHz for a 133 MHz input clock.
            controller.regs.write(IC_SS_SCL_HCNT, 0x2a0);
            controller.regs.write(IC_SS_SCL_LCNT, 0x310);
        }
        // Master, restart enabled, slave disabled; speed 2 (fast) if configured.
        let speed = if fast { 2 } else { 1 };
        controller.regs.write(IC_CON, 0x61 | speed << 1);
        controller.regs.write(IC_INTR_MASK, 0);
        controller.regs.write(IC_RX_TL, 0);
        controller.regs.write(IC_TX_TL, 0);
        let timing = (controller.regs.read(IC_FS_SCL_HCNT) << 16)
            | (controller.regs.read(IC_SS_SCL_HCNT) & 0xffff);
        Ok((controller, signature, timing))
    }

    pub fn regs(&mut self) -> &mut R {
        &mut self.regs
    }

    fn wait(&mut self, mut done: impl FnMut(&mut R) -> bool) -> Result<()> {
        let deadline = self.regs.ticks() + TIMEOUT_TICKS;
        for _ in 0..2_000_000 {
            if done(&mut self.regs) {
                return Ok(());
            }
            if self.regs.ticks() > deadline {
                break;
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout)
    }

    fn disable(&mut self) -> Result<()> {
        self.regs.write(IC_ENABLE, 0);
        self.wait(|regs| regs.read(IC_ENABLE_STATUS) & 1 == 0)
    }

    /// After an abort the FIFOs are flushed by disabling; report what went wrong.
    fn failed(&mut self) -> Error {
        let source = self.regs.read(IC_TX_ABRT_SOURCE);
        self.regs.read(IC_CLR_TX_ABRT);
        let _ = self.disable();
        // Bits 0-3: address or data not acknowledged.
        if source & 0xf != 0 {
            Error::Nack
        } else {
            Error::Abort(source)
        }
    }

    fn run(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> Result<()> {
        self.disable()?;
        self.regs.write(IC_TAR, address as u32 & 0x7f);
        self.regs.write(IC_ENABLE, 1);
        self.regs.read(IC_CLR_INTR);
        let total = write.len() + read.len();
        let (mut pushed, mut received) = (0, 0);
        let deadline = self.regs.ticks() + TIMEOUT_TICKS;
        let mut spins = 0u32;
        while received < read.len() || pushed < total {
            if self.regs.read(IC_RAW_INTR_STAT) & RAW_TX_ABRT != 0 {
                return Err(self.failed());
            }
            let status = self.regs.read(IC_STATUS);
            let outstanding = pushed.saturating_sub(write.len()) - received;
            if pushed < total && status & STATUS_TX_NOT_FULL != 0 && outstanding < MAX_OUTSTANDING {
                let last = pushed + 1 == total;
                let command = if pushed < write.len() {
                    write[pushed] as u32 | if last { CMD_STOP } else { 0 }
                } else {
                    CMD_READ
                        | if pushed == write.len() && !write.is_empty() {
                            CMD_RESTART
                        } else {
                            0
                        }
                        | if last { CMD_STOP } else { 0 }
                };
                self.regs.write(IC_DATA_CMD, command);
                pushed += 1;
                continue;
            }
            if status & STATUS_RX_NOT_EMPTY != 0 && received < read.len() {
                read[received] = self.regs.read(IC_DATA_CMD) as u8;
                received += 1;
                continue;
            }
            spins += 1;
            if self.regs.ticks() > deadline || spins > 2_000_000 {
                let _ = self.disable();
                return Err(Error::Timeout);
            }
            core::hint::spin_loop();
        }
        // Wait for the STOP: transmit FIFO empty and the bus idle.
        let mut result = self.wait(|regs| {
            let status = regs.read(IC_STATUS);
            status & STATUS_TX_EMPTY != 0 && status & STATUS_ACTIVITY == 0
                || regs.read(IC_RAW_INTR_STAT) & RAW_TX_ABRT != 0
        });
        if self.regs.read(IC_RAW_INTR_STAT) & RAW_TX_ABRT != 0 {
            result = Err(self.failed());
        }
        let _ = self.disable();
        result
    }

    /// Full FIFO depth the controller reported (for the boot log).
    pub fn tx_depth(&self) -> usize {
        self.tx_depth
    }
}

impl<R: Regs> Bus for DesignWare<R> {
    fn transfer(&mut self, address: u8, write: &[u8], read: &mut [u8]) -> Result<()> {
        if write.is_empty() && read.is_empty() {
            return Err(Error::TooLarge);
        }
        self.run(address, write, read)
    }
    fn ticks(&self) -> u64 {
        self.regs.ticks()
    }
}

#[cfg(test)]
mod tests {
    use super::super::simulation::{HidTouchpad, Plain, Simulation};
    use super::*;
    use std::boxed::Box;
    use std::vec;

    fn controller(
        devices: std::vec::Vec<Box<dyn super::super::simulation::Device>>,
    ) -> DesignWare<Simulation> {
        DesignWare::new(Simulation::new(devices)).unwrap().0
    }

    #[test]
    fn a_window_that_is_not_a_designware_controller_is_refused() {
        let mut simulation = Simulation::new(vec![]);
        simulation.set(0xfc, 0xffff_ffff); // what an unpowered device reads as
        assert_eq!(
            DesignWare::new(simulation).err(),
            Some(Error::NotDesignWare)
        );
    }

    #[test]
    fn initialization_releases_reset_and_follows_the_firmware_speed() {
        let mut simulation = Simulation::new(vec![]);
        simulation.set(0x1c, 0x100);
        simulation.set(0x20, 0x120);
        let (mut controller, _, _) = DesignWare::new(simulation).unwrap();
        assert_eq!(controller.regs().writes_to_reset_register, 1);
        // Master, restart enable, slave disabled, fast (2) speed.
        assert_eq!(controller.regs().read(0x00), 0x65);
        let mut standard = Simulation::new(vec![]);
        standard.set(0x14, 0x190);
        standard.set(0x18, 0x1d6);
        assert_eq!(DesignWare::new(standard).unwrap().0.regs().read(0x00), 0x63);
        // With no timing programmed at all, standard-speed timing is supplied.
        let mut blank = DesignWare::new(Simulation::new(vec![])).unwrap().0;
        assert_ne!(blank.regs().read(0x14), 0);
        assert_ne!(blank.regs().read(0x18), 0);
    }

    #[test]
    fn write_then_read_returns_the_register_contents_and_repeats() {
        let pad = HidTouchpad::new(0x2c, vec![0x05, 0x01]);
        let mut bus = controller(vec![Box::new(pad)]);
        let mut descriptor = [0u8; 30];
        bus.transfer(0x2c, &[0x01, 0x00], &mut descriptor).unwrap();
        assert_eq!(&descriptor[..4], &[30, 0, 0, 1]);
        // The controller is reusable afterwards, repeatedly.
        for _ in 0..3 {
            let mut again = [0u8; 30];
            bus.transfer(0x2c, &[0x01, 0x00], &mut again).unwrap();
            assert_eq!(again, descriptor);
        }
        assert!(!bus.regs().overflowed);
    }

    #[test]
    fn an_absent_device_is_a_nack_and_the_bus_recovers() {
        let mut bus = controller(vec![Box::new(Plain(0x50))]);
        let mut byte = [0];
        assert_eq!(bus.transfer(0x33, &[], &mut byte), Err(Error::Nack));
        assert_eq!(bus.transfer(0x33, &[1, 2], &mut []), Err(Error::Nack));
        bus.transfer(0x50, &[], &mut byte).unwrap();
        assert_eq!(byte, [0xa5]);
    }

    #[test]
    fn a_long_read_never_overruns_the_receive_fifo() {
        let mut bus = controller(vec![Box::new(Plain(0x50))]);
        let mut big = [0u8; 300];
        bus.transfer(0x50, &[], &mut big).unwrap();
        assert!(big.iter().all(|&b| b == 0xa5));
        assert!(
            !bus.regs().overflowed,
            "more reads in flight than the FIFO holds"
        );
    }

    #[test]
    fn an_empty_transfer_is_rejected() {
        let mut bus = controller(vec![]);
        assert_eq!(bus.transfer(0x10, &[], &mut []), Err(Error::TooLarge));
    }
}
