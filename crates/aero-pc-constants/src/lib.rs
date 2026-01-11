#![forbid(unsafe_code)]

/// Shared physical address / topology constants for the x86 PC platform.
///
/// This crate exists so the BIOS/firmware (`firmware`) and the device platform wiring
/// (`aero-pc-platform`) agree on addresses that must match exactly at runtime.

/// Base physical address of the PCIe ECAM ("MMCONFIG") window.
///
/// This follows the QEMU Q35 convention (256MiB window at 0xB000_0000 covering buses 0..=255).
pub const PCIE_ECAM_BASE: u64 = 0xB000_0000;

pub const PCIE_ECAM_SEGMENT: u16 = 0;
pub const PCIE_ECAM_START_BUS: u8 = 0;
pub const PCIE_ECAM_END_BUS: u8 = 0xFF;

/// Number of bytes covered by one bus worth of PCIe ECAM configuration space.
///
/// The ECAM layout is:
/// - 32 devices per bus
/// - 8 functions per device
/// - 4KiB config space per function
///
/// Which yields 32 * 8 * 4096 = 1MiB per bus.
pub const PCIE_ECAM_BUS_STRIDE: u64 = 1 << 20;

/// Size of the ECAM window in bytes.
pub const PCIE_ECAM_SIZE: u64 =
    (PCIE_ECAM_END_BUS as u64 - PCIE_ECAM_START_BUS as u64 + 1) * PCIE_ECAM_BUS_STRIDE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecam_defaults_follow_q35_convention() {
        assert_eq!(PCIE_ECAM_BASE, 0xB000_0000);
        assert_eq!(PCIE_ECAM_SEGMENT, 0);
        assert_eq!(PCIE_ECAM_START_BUS, 0);
        assert_eq!(PCIE_ECAM_END_BUS, 0xFF);
        assert_eq!(PCIE_ECAM_BUS_STRIDE, 1 << 20);
        assert_eq!(PCIE_ECAM_SIZE, 0x1000_0000);
    }
}
