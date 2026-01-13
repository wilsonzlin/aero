use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::pci::{profile, PciConfigSpace, PciDevice};
use memory::MmioHandler;

/// Size of the legacy VGA window (`0xA0000..0xC0000`).
///
/// This is the amount of VRAM reserved at the start of BAR1 for legacy VGA aliasing.
pub const LEGACY_VGA_VRAM_BYTES: u64 = 0x20_000;

/// Offset within BAR1/VRAM where the VBE linear framebuffer (LFB) region begins.
///
/// By default, the LFB is placed immediately after the legacy VGA alias region.
pub const VBE_LFB_OFFSET: u64 = 0x20_000;

/// Start physical address of the legacy VGA window.
pub const LEGACY_VGA_PADDR_BASE: u64 = 0xA_0000;

/// End physical address (exclusive) of the legacy VGA window.
pub const LEGACY_VGA_PADDR_END: u64 = 0xC_0000;

/// PCI BAR index for the AeroGPU MMIO registers window (BAR0).
pub const AEROGPU_PCI_BAR0_INDEX: u8 = 0;

/// PCI BAR index for the AeroGPU VRAM aperture (BAR1).
pub const AEROGPU_PCI_BAR1_INDEX: u8 = 1;

/// A minimal AeroGPU PCI device wrapper that owns VRAM backing storage.
pub struct AeroGpuPciDevice {
    config: PciConfigSpace,
    vram: Rc<RefCell<Vec<u8>>>,
}

impl AeroGpuPciDevice {
    /// Construct a new AeroGPU PCI device with VRAM allocated to the size of BAR1.
    pub fn new() -> Self {
        let config = profile::AEROGPU.build_config_space();

        let bar1_size = config
            .bar_range(AEROGPU_PCI_BAR1_INDEX)
            .map(|r| r.size)
            .unwrap_or(0);
        let vram = Rc::new(RefCell::new(vec![0u8; bar1_size as usize]));

        Self { config, vram }
    }

    /// Returns a handle to the VRAM vector (shared with BAR1 MMIO handlers).
    pub fn vram_shared(&self) -> Rc<RefCell<Vec<u8>>> {
        Rc::clone(&self.vram)
    }

    /// Returns an [`MmioHandler`] implementing the BAR1 VRAM aperture.
    pub fn bar1_mmio_handler(&self) -> AeroGpuBar1VramMmio {
        AeroGpuBar1VramMmio {
            vram: Rc::clone(&self.vram),
        }
    }

    /// Translate a physical address in the legacy VGA window (`0xA0000..0xC0000`) into a VRAM
    /// offset starting at 0.
    pub fn legacy_vga_paddr_to_vram_offset(paddr: u64) -> Option<u64> {
        if !(LEGACY_VGA_PADDR_BASE..LEGACY_VGA_PADDR_END).contains(&paddr) {
            return None;
        }
        Some(paddr - LEGACY_VGA_PADDR_BASE)
    }

    /// Translate a physical address in the VBE linear framebuffer region into a VRAM offset.
    ///
    /// The VBE LFB is expected to live at `bar1_base + VBE_LFB_OFFSET`.
    pub fn vbe_lfb_paddr_to_vram_offset(bar1_base: u64, paddr: u64) -> Option<u64> {
        let lfb_base = bar1_base.checked_add(VBE_LFB_OFFSET)?;
        if paddr < lfb_base {
            return None;
        }
        let off = paddr.checked_sub(bar1_base)?;
        let end = bar1_base.checked_add(Self::bar1_size_bytes()?)?;
        if paddr >= end {
            return None;
        }
        Some(off)
    }

    fn bar1_size_bytes() -> Option<u64> {
        // Keep this as a helper so callers don't need to plumb the config space just to validate
        // offsets. `profile::AEROGPU` defines BAR1; `new()` allocates VRAM accordingly.
        profile::AEROGPU
            .bars
            .iter()
            .find(|bar| bar.index == AEROGPU_PCI_BAR1_INDEX)
            .map(|bar| bar.size)
    }
}

impl Default for AeroGpuPciDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl PciDevice for AeroGpuPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

/// MMIO handler for the AeroGPU VRAM aperture (PCI BAR1).
///
/// Reads and writes access a byte-addressed VRAM vector. The handler is deliberately "dumb": it
/// does not attempt to emulate VGA planar behavior; it simply exposes the raw bytes.
pub struct AeroGpuBar1VramMmio {
    vram: Rc<RefCell<Vec<u8>>>,
}

impl MmioHandler for AeroGpuBar1VramMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        let vram = self.vram.borrow();
        let mut out = 0u64;
        for i in 0..size {
            let addr = offset.wrapping_add(i as u64);
            let b = usize::try_from(addr)
                .ok()
                .and_then(|idx| vram.get(idx).copied())
                .unwrap_or(0xFF);
            out |= (b as u64) << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let mut vram = self.vram.borrow_mut();
        for i in 0..size {
            let addr = offset.wrapping_add(i as u64);
            let Some(idx) = usize::try_from(addr).ok() else {
                continue;
            };
            if idx >= vram.len() {
                continue;
            }
            vram[idx] = ((value >> (i * 8)) & 0xFF) as u8;
        }
    }
}

