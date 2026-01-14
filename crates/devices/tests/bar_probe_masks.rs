//! Helper functions for computing expected PCI BAR probe masks in tests.
//!
//! These mirror the logic in `aero_devices::pci::PciConfigSpace::read_bar_register`:
//! - MMIO32 probes return `!(size - 1)` with the low 4 bits reserved for flags, plus the
//!   prefetchable bit (bit 3) when applicable.
//! - IO probes return `!(size - 1)` masked to clear bits 1:0, plus the IO indicator bit (bit 0).
#![allow(dead_code)]
pub fn mmio32_probe_mask(size: u32, prefetchable: bool) -> u32 {
    let mut mask = !(size.saturating_sub(1)) & 0xFFFF_FFF0;
    if prefetchable {
        mask |= 1 << 3;
    }
    mask
}

pub fn io_probe_mask(size: u32) -> u32 {
    let mask = !(size.saturating_sub(1)) & 0xFFFF_FFFC;
    mask | 0x1
}
