use crate::devices::pci::{PciBar, PciConfigSpace, PciFunction};
use std::any::Any;

pub const VGA_PCI_VENDOR_ID: u16 = 0x1234;
pub const VGA_PCI_DEVICE_ID: u16 = 0x1111;

pub const VGA_PCI_CLASS_CODE: u8 = 0x03;
pub const VGA_PCI_SUBCLASS: u8 = 0x00;
pub const VGA_PCI_PROG_IF: u8 = 0x00;

// Full legacy VGA decode range, including the mono+color CRTC aliasing ranges.
pub const VGA_LEGACY_IO_START: u16 = 0x3B0;
pub const VGA_LEGACY_IO_END: u16 = 0x3DF;

pub const VGA_LEGACY_MEM_START: u32 = 0xA0000;
pub const VGA_LEGACY_MEM_END: u32 = 0xBFFFF;

pub const DEFAULT_LFB_BASE: u32 = 0xE000_0000;
pub const DEFAULT_LFB_SIZE: u32 = 16 * 1024 * 1024;

pub struct VgaPciFunction {
    config: PciConfigSpace,
    vbe_phys_base_ptr: u32,
    lfb: Vec<u8>,
}

impl Default for VgaPciFunction {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaPciFunction {
    pub fn new() -> Self {
        Self::new_with_lfb(DEFAULT_LFB_BASE, DEFAULT_LFB_SIZE)
    }

    pub fn new_with_lfb(lfb_base: u32, lfb_size: u32) -> Self {
        let mut config = PciConfigSpace::new(
            VGA_PCI_VENDOR_ID,
            VGA_PCI_DEVICE_ID,
            VGA_PCI_CLASS_CODE,
            VGA_PCI_SUBCLASS,
            VGA_PCI_PROG_IF,
        );
        config.set_header_type(0x00);
        config.bars[0] = PciBar::memory32(lfb_base, lfb_size, true);

        let lfb_base = config.bars[0].base();

        Self {
            vbe_phys_base_ptr: lfb_base,
            lfb: vec![0; lfb_size as usize],
            config,
        }
    }

    pub fn vbe_phys_base_ptr(&self) -> u32 {
        self.vbe_phys_base_ptr
    }

    pub fn lfb_base(&self) -> u32 {
        self.config.bars[0].base()
    }

    pub fn lfb_size(&self) -> u32 {
        self.config.bars[0].size()
    }

    pub fn framebuffer(&self) -> &[u8] {
        &self.lfb
    }
}

impl PciFunction for VgaPciFunction {
    fn config_read(&mut self, offset: u16, size: u8) -> u32 {
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: u8, value: u32) {
        let old_base = self.config.bars[0].base();
        self.config.write(offset, size, value);
        let new_base = self.config.bars[0].base();
        if new_base != old_base {
            self.vbe_phys_base_ptr = new_base;
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
