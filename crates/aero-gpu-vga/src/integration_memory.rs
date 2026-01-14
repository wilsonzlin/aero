use crate::VgaDevice;
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

/// [`memory::MmioHandler`] adapter for the legacy VGA memory window (`0xA0000..0xC0000`).
///
/// This is intended to be mapped at a guest physical base such as `0xA0000`, with `offset`
/// interpreted as `paddr - base_paddr`.
pub struct VgaLegacyMmioHandler {
    pub base_paddr: u32,
    pub dev: Rc<RefCell<VgaDevice>>,
}

impl MmioHandler for VgaLegacyMmioHandler {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if !(1..=8).contains(&size) {
            return u64::MAX;
        }

        let base = u64::from(self.base_paddr).wrapping_add(offset);
        let mut dev = self.dev.borrow_mut();

        let mut out = 0u64;
        for i in 0..size {
            let paddr = base.wrapping_add(i as u64);
            let b = dev.mem_read_u8(u32::try_from(paddr).unwrap_or(0)) as u64;
            out |= b << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || !(1..=8).contains(&size) {
            return;
        }

        let base = u64::from(self.base_paddr).wrapping_add(offset);
        let mut dev = self.dev.borrow_mut();
        for i in 0..size {
            let paddr = base.wrapping_add(i as u64);
            let b = ((value >> (i * 8)) & 0xFF) as u8;
            dev.mem_write_u8(u32::try_from(paddr).unwrap_or(0), b);
        }
    }
}

/// [`memory::MmioHandler`] adapter for the VGA/SVGA VRAM aperture (linear framebuffer).
///
/// This is intended to be mapped at the physical base of the VBE linear framebuffer (LFB), such as
/// [`crate::SVGA_LFB_BASE`], with `offset` interpreted as a byte offset into the VBE framebuffer
/// region (starting at [`crate::VgaConfig::lfb_offset`] within [`VgaDevice::vram`]).
pub struct VgaLfbMmioHandler {
    pub dev: Rc<RefCell<VgaDevice>>,
}

impl MmioHandler for VgaLfbMmioHandler {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        let dev = self.dev.borrow();
        let lfb_off = usize::try_from(dev.config().lfb_offset).unwrap_or(0);
        let vram = dev.vram();
        let fb_len = vram.len().saturating_sub(lfb_off);
        let base = offset as usize;
        if base >= fb_len {
            return 0;
        }

        let end_in_fb = base.saturating_add(size).min(fb_len);
        let len = end_in_fb - base;
        let start = lfb_off + base;
        let end = lfb_off + end_in_fb;

        let mut buf = [0u8; 8];
        buf[..len].copy_from_slice(&vram[start..end]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = match size {
            1 | 2 | 4 | 8 => size,
            _ => size.clamp(1, 8),
        };

        let base = offset as usize;
        let mut dev = self.dev.borrow_mut();
        let lfb_off = usize::try_from(dev.config().lfb_offset).unwrap_or(0);
        let vram = dev.vram_mut();
        let fb_len = vram.len().saturating_sub(lfb_off);
        if base >= fb_len {
            return;
        }

        let end_in_fb = base.saturating_add(size).min(fb_len);
        let len = end_in_fb - base;
        let start = lfb_off + base;
        let end = lfb_off + end_in_fb;
        let bytes = value.to_le_bytes();
        vram[start..end].copy_from_slice(&bytes[..len]);
    }
}
