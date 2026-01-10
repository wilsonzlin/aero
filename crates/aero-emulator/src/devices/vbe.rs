//! VESA BIOS Extensions (VBE) 2.0+ implementation.
//!
//! Supported VBE mode numbers (direct color, 32bpp X8R8G8B8):
//!
//! | Mode | Resolution | BPP |
//! |------|------------|-----|
//! | 0x112| 640x480    | 32  |
//! | 0x115| 800x600    | 32  |
//! | 0x118| 1024x768   | 32  |
//!
//! The mode IDs match common “Bochs/QEMU stdvga” expectations where `0x118` is
//! `1024x768x32`.
//!
//! Linear framebuffer (LFB) is exposed as a fixed physical MMIO region at
//! [`VBE_LFB_BASE`]. The VBE banked window is mapped at 0xA0000 (64KiB).

use crate::memory::mmio::MmioDevice;
use std::cell::RefCell;
use std::rc::Rc;

/// Fixed physical address for the linear framebuffer.
pub const VBE_LFB_BASE: u32 = 0xE000_0000;

/// Reserved size for the LFB MMIO aperture (16MiB).
pub const VBE_LFB_SIZE: u32 = 16 * 1024 * 1024;

/// Physical address of the banked window (VGA window A).
pub const VBE_BANK_WINDOW_BASE: u32 = 0x000A_0000;
pub const VBE_BANK_WINDOW_SIZE: u32 = 64 * 1024;
pub const VBE_BANK_GRANULARITY: u16 = 64; // 64KiB units.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VbeError {
    UnsupportedMode,
    FramebufferTooLarge,
    UnsupportedWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbeModeId(pub u16);

impl VbeModeId {
    pub fn from_raw(raw: u16) -> Self {
        VbeModeId(raw & 0x3FFF)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VbeMode {
    pub id: VbeModeId,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
}

const VBE_MODES: &[VbeMode] = &[
    VbeMode {
        id: VbeModeId(0x112),
        width: 640,
        height: 480,
        bpp: 32,
    },
    VbeMode {
        id: VbeModeId(0x115),
        width: 800,
        height: 600,
        bpp: 32,
    },
    VbeMode {
        id: VbeModeId(0x118),
        width: 1024,
        height: 768,
        bpp: 32,
    },
];

#[derive(Debug, Default)]
struct VbeState {
    current_mode: Option<VbeModeId>,
    lfb_enabled: bool,

    width: u16,
    height: u16,
    stride_bytes: u16,

    /// Raw pixel memory in VBE format (B, G, R, X).
    lfb: Vec<u8>,

    /// 4KiB dirty page tracking for the raw LFB buffer.
    dirty_pages: Vec<bool>,

    /// Cached converted RGBA framebuffer.
    rgba: Vec<u8>,

    bank_a: u16,
}

#[derive(Debug, Clone)]
pub struct VbeDevice {
    inner: Rc<RefCell<VbeState>>,
}

impl Default for VbeDevice {
    fn default() -> Self {
        Self {
            inner: Rc::new(RefCell::new(VbeState::default())),
        }
    }
}

impl VbeDevice {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mmio_lfb(&self) -> VbeLfbMmio {
        VbeLfbMmio {
            inner: self.inner.clone(),
        }
    }

    pub fn mmio_bank_window(&self) -> VbeBankWindowMmio {
        VbeBankWindowMmio {
            inner: self.inner.clone(),
        }
    }

    pub fn current_mode(&self) -> Option<(VbeModeId, bool)> {
        let inner = self.inner.borrow();
        inner.current_mode.map(|m| (m, inner.lfb_enabled))
    }

    pub fn set_mode(&self, mode: VbeModeId, lfb: bool) -> Result<(), VbeError> {
        let mode_info = VBE_MODES
            .iter()
            .find(|m| m.id == mode)
            .ok_or(VbeError::UnsupportedMode)?;
        if mode_info.bpp != 32 {
            return Err(VbeError::UnsupportedMode);
        }

        let stride_bytes = mode_info.width as usize * 4;
        let size = stride_bytes
            .checked_mul(mode_info.height as usize)
            .ok_or(VbeError::FramebufferTooLarge)?;

        if size > VBE_LFB_SIZE as usize {
            return Err(VbeError::FramebufferTooLarge);
        }

        let mut inner = self.inner.borrow_mut();
        inner.current_mode = Some(mode);
        inner.lfb_enabled = lfb;
        inner.width = mode_info.width;
        inner.height = mode_info.height;
        inner.stride_bytes = stride_bytes as u16;
        inner.lfb = vec![0u8; size];
        inner.rgba = vec![0u8; size];
        inner.dirty_pages = vec![true; size.div_ceil(4096)];
        inner.bank_a = 0;
        Ok(())
    }

    pub fn mode_list(&self) -> impl Iterator<Item = VbeModeId> + 'static {
        VBE_MODES.iter().map(|m| m.id)
    }

    pub fn build_controller_info(&self, es: u16, di: u16, out: &mut [u8; 512]) {
        // Caller-provided buffer contains the data; we point strings/mode list
        // back into the same segment for simplicity.
        out.fill(0);

        // Signature "VESA"
        out[0..4].copy_from_slice(b"VESA");

        // VBE version 2.0
        out[4..6].copy_from_slice(&0x0200u16.to_le_bytes());

        // Place strings/mode list inside the OEM data area (offset 256+).
        let mode_list_offset = 0x0100u16; // 256
        let oem_string_offset = 0x0110u16; // 272
        let vendor_string_offset = 0x0120u16; // 288
        let product_string_offset = 0x0130u16; // 304
        let product_rev_offset = 0x0140u16; // 320

        let oem_ptr = far_pointer(es, di.wrapping_add(oem_string_offset));
        out[6..10].copy_from_slice(&oem_ptr.to_le_bytes());

        // Capabilities: none.
        out[10..14].copy_from_slice(&[0, 0, 0, 0]);

        let mode_ptr = far_pointer(es, di.wrapping_add(mode_list_offset));
        out[14..18].copy_from_slice(&mode_ptr.to_le_bytes());

        // TotalMemory in 64KiB blocks. We reserve 16MiB => 256 blocks.
        out[18..20].copy_from_slice(&((VBE_LFB_SIZE / 65536) as u16).to_le_bytes());

        // OEMSoftwareRev
        out[20..22].copy_from_slice(&0x0001u16.to_le_bytes());

        let vendor_ptr = far_pointer(es, di.wrapping_add(vendor_string_offset));
        out[22..26].copy_from_slice(&vendor_ptr.to_le_bytes());
        let product_ptr = far_pointer(es, di.wrapping_add(product_string_offset));
        out[26..30].copy_from_slice(&product_ptr.to_le_bytes());
        let rev_ptr = far_pointer(es, di.wrapping_add(product_rev_offset));
        out[30..34].copy_from_slice(&rev_ptr.to_le_bytes());

        // Mode list (u16 terminated with 0xFFFF)
        let mut cursor = mode_list_offset as usize;
        for mode in self.mode_list() {
            out[cursor..cursor + 2].copy_from_slice(&mode.0.to_le_bytes());
            cursor += 2;
        }
        out[cursor..cursor + 2].copy_from_slice(&0xFFFFu16.to_le_bytes());

        // Strings are NUL-terminated.
        write_c_string(out, oem_string_offset, b"Aero VBE 2.0");
        write_c_string(out, vendor_string_offset, b"Aero");
        write_c_string(out, product_string_offset, b"Aero Emulator");
        write_c_string(out, product_rev_offset, b"0.1");
    }

    pub fn mode_info(&self, mode: VbeModeId) -> Option<[u8; 256]> {
        let mode_info = VBE_MODES.iter().find(|m| m.id == mode)?;

        if mode_info.bpp != 32 {
            return None;
        }

        let stride_bytes = mode_info.width * 4;

        let mut out = [0u8; 256];

        // ModeAttributes: supported | color | graphics | LFB available.
        out[0..2].copy_from_slice(&0x009Bu16.to_le_bytes());

        // Window A attributes: read/write, window B not supported.
        out[2] = 0x07;
        out[3] = 0x00;

        // Granularity and window size in KiB.
        out[4..6].copy_from_slice(&VBE_BANK_GRANULARITY.to_le_bytes());
        out[6..8].copy_from_slice(&VBE_BANK_GRANULARITY.to_le_bytes());
        out[8..10].copy_from_slice(&((VBE_BANK_WINDOW_BASE >> 4) as u16).to_le_bytes()); // 0xA000 segment
        out[10..12].copy_from_slice(&0u16.to_le_bytes());

        // WinFuncPtr (deprecated in VBE 2.0+)
        out[12..16].copy_from_slice(&0u32.to_le_bytes());

        out[16..18].copy_from_slice(&stride_bytes.to_le_bytes());
        out[18..20].copy_from_slice(&mode_info.width.to_le_bytes());
        out[20..22].copy_from_slice(&mode_info.height.to_le_bytes());

        // Character cell sizes unused for graphics.
        out[22] = 0;
        out[23] = 0;

        out[24] = 1; // planes
        out[25] = 32; // bpp
        let stride_bytes_u32 = stride_bytes as u32;
        let bytes_total = stride_bytes_u32 * mode_info.height as u32;
        let banks = bytes_total.div_ceil(VBE_BANK_WINDOW_SIZE) as u8;
        out[26] = banks; // banks
        out[27] = 6; // memory model: Direct Color
        out[28] = VBE_BANK_GRANULARITY as u8; // bank size in KiB
        out[29] = 0; // image pages
        out[30] = 0;

        // Color masks for X8R8G8B8.
        out[31] = 8; // red mask size
        out[32] = 16; // red position
        out[33] = 8; // green mask size
        out[34] = 8; // green position
        out[35] = 8; // blue mask size
        out[36] = 0; // blue position
        out[37] = 8; // reserved mask size
        out[38] = 24; // reserved position
        out[39] = 0; // direct color mode info

        out[40..44].copy_from_slice(&VBE_LFB_BASE.to_le_bytes()); // PhysBasePtr
                                                                  // remaining fields left zero

        Some(out)
    }

    pub fn set_bank(&self, window: u8, bank: u16) -> Result<(), VbeError> {
        if window != 0 {
            return Err(VbeError::UnsupportedWindow);
        }
        let mut inner = self.inner.borrow_mut();
        inner.bank_a = bank;
        Ok(())
    }

    pub fn get_bank(&self, window: u8) -> Option<u16> {
        if window != 0 {
            return None;
        }
        Some(self.inner.borrow().bank_a)
    }

    pub fn with_framebuffer_rgba<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(u16, u16, &[u8]) -> R,
    {
        let mut inner = self.inner.borrow_mut();
        inner.update_rgba();
        inner.current_mode?;
        Some(f(inner.width, inner.height, &inner.rgba))
    }
}

fn far_pointer(seg: u16, off: u16) -> u32 {
    ((seg as u32) << 16) | off as u32
}

fn write_c_string(buf: &mut [u8], offset: u16, s: &[u8]) {
    let start = offset as usize;
    let end = start + s.len();
    buf[start..end].copy_from_slice(s);
    buf[end] = 0;
}

impl VbeState {
    fn update_rgba(&mut self) {
        if self.dirty_pages.is_empty() {
            return;
        }

        for (page_idx, dirty) in self.dirty_pages.iter_mut().enumerate() {
            if !*dirty {
                continue;
            }
            *dirty = false;

            let page_start = page_idx * 4096;
            let page_end = (page_start + 4096).min(self.lfb.len());

            // Convert BGRX -> RGBA.
            for src_idx in (page_start..page_end).step_by(4) {
                if src_idx + 3 >= self.lfb.len() {
                    break;
                }
                let b = self.lfb[src_idx];
                let g = self.lfb[src_idx + 1];
                let r = self.lfb[src_idx + 2];

                self.rgba[src_idx] = r;
                self.rgba[src_idx + 1] = g;
                self.rgba[src_idx + 2] = b;
                self.rgba[src_idx + 3] = 0xFF;
            }
        }
    }

    fn mark_dirty_range(&mut self, offset: usize, len: usize) {
        if self.dirty_pages.is_empty() {
            return;
        }
        let start_page = offset / 4096;
        let end_page = (offset + len).saturating_sub(1) / 4096;
        for page in start_page..=end_page {
            if let Some(slot) = self.dirty_pages.get_mut(page) {
                *slot = true;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct VbeLfbMmio {
    inner: Rc<RefCell<VbeState>>,
}

impl MmioDevice for VbeLfbMmio {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let inner = self.inner.borrow();
        let offset = offset as usize;
        if offset >= inner.lfb.len() {
            data.fill(0);
            return;
        }
        let end = (offset + data.len()).min(inner.lfb.len());
        data[..end - offset].copy_from_slice(&inner.lfb[offset..end]);
        if end - offset < data.len() {
            data[end - offset..].fill(0);
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        let mut inner = self.inner.borrow_mut();
        let offset = offset as usize;
        if offset >= inner.lfb.len() {
            return;
        }
        let end = (offset + data.len()).min(inner.lfb.len());
        inner.lfb[offset..end].copy_from_slice(&data[..end - offset]);
        inner.mark_dirty_range(offset, end - offset);
    }
}

#[derive(Debug, Clone)]
pub struct VbeBankWindowMmio {
    inner: Rc<RefCell<VbeState>>,
}

impl MmioDevice for VbeBankWindowMmio {
    fn read(&self, offset: u64, data: &mut [u8]) {
        let inner = self.inner.borrow();
        let bank_offset = inner.bank_a as usize * VBE_BANK_WINDOW_SIZE as usize;
        let offset = offset as usize;
        let abs = bank_offset + offset;

        if abs >= inner.lfb.len() {
            data.fill(0);
            return;
        }
        let end = (abs + data.len()).min(inner.lfb.len());
        data[..end - abs].copy_from_slice(&inner.lfb[abs..end]);
        if end - abs < data.len() {
            data[end - abs..].fill(0);
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        let mut inner = self.inner.borrow_mut();
        let bank_offset = inner.bank_a as usize * VBE_BANK_WINDOW_SIZE as usize;
        let offset = offset as usize;
        let abs = bank_offset + offset;
        if abs >= inner.lfb.len() {
            return;
        }
        let end = (abs + data.len()).min(inner.lfb.len());
        inner.lfb[abs..end].copy_from_slice(&data[..end - abs]);
        inner.mark_dirty_range(abs, end - abs);
    }
}
