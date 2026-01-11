use std::cell::RefCell;
use std::rc::Rc;

use memory::MmioHandler;

use super::{VgaDevice, VBE_LFB_SIZE};

pub const VGA_BANK_WINDOW_PADDR: u64 = 0x000A_0000;
pub const VGA_BANK_WINDOW_SIZE: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgaMmioRegion {
    Lfb,
    BankedWindowA,
}

/// MMIO handler that exposes the VGA/VBE framebuffer apertures.
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
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.min(8);
        let mut buf = [0u8; 8];
        match self.region {
            VgaMmioRegion::Lfb => self
                .vga
                .borrow()
                .lfb_read(offset as usize, &mut buf[..size]),
            VgaMmioRegion::BankedWindowA => self
                .vga
                .borrow()
                .banked_read(offset as usize, &mut buf[..size]),
        }
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.min(8);
        let bytes = value.to_le_bytes();
        match self.region {
            VgaMmioRegion::Lfb => self
                .vga
                .borrow_mut()
                .lfb_write(offset as usize, &bytes[..size]),
            VgaMmioRegion::BankedWindowA => {
                self.vga
                    .borrow_mut()
                    .banked_write(offset as usize, &bytes[..size]);
            }
        }
    }
}
