use std::cell::RefCell;
use std::rc::Rc;

use crate::memory_bus::MmioHandler;

use super::{VgaDevice, VBE_LFB_SIZE};

pub const VGA_BANK_WINDOW_PADDR: u64 = 0x000A_0000;
pub const VGA_BANK_WINDOW_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaMmioRegion {
    Lfb,
    BankedWindowA,
}

/// MMIO handler that exposes the VGA/VBE framebuffer apertures.
///
/// This is designed to plug into [`crate::memory_bus::MemoryBus`] via
/// `add_mmio_region`.
pub struct VgaMmio {
    vga: Rc<RefCell<VgaDevice>>,
    region: VgaMmioRegion,
}

impl VgaMmio {
    pub fn new(vga: Rc<RefCell<VgaDevice>>, region: VgaMmioRegion) -> Self {
        Self { vga, region }
    }

    pub fn region_len(region: VgaMmioRegion) -> u64 {
        match region {
            VgaMmioRegion::Lfb => VBE_LFB_SIZE as u64,
            VgaMmioRegion::BankedWindowA => VGA_BANK_WINDOW_SIZE,
        }
    }
}

impl MmioHandler for VgaMmio {
    fn read_u8(&mut self, offset: u64) -> u8 {
        let mut buf = [0u8; 1];
        match self.region {
            VgaMmioRegion::Lfb => self.vga.borrow().lfb_read(offset as usize, &mut buf),
            VgaMmioRegion::BankedWindowA => {
                self.vga.borrow().banked_read(offset as usize, &mut buf)
            }
        }
        buf[0]
    }

    fn write_u8(&mut self, offset: u64, value: u8) {
        match self.region {
            VgaMmioRegion::Lfb => self.vga.borrow_mut().lfb_write(offset as usize, &[value]),
            VgaMmioRegion::BankedWindowA => {
                self.vga
                    .borrow_mut()
                    .banked_write(offset as usize, &[value]);
            }
        }
    }
}
