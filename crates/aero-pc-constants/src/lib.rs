#![forbid(unsafe_code)]

//! Shared physical address / topology constants for the x86 PC platform.
//!
//! This crate exists so the BIOS/firmware (`firmware`) and the device platform wiring
//! (`aero-pc-platform`) agree on addresses that must match exactly at runtime.

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

/// Base physical address of the PCI MMIO window reported by ACPI (`PCI0._CRS`) for PCI BAR
/// allocations.
///
/// The PC platform reserves:
/// - `PCIE_ECAM_BASE..PCIE_ECAM_BASE + PCIE_ECAM_SIZE` for PCIe ECAM (MCFG/MMCONFIG), and
/// - `PCI_MMIO_BASE..PCI_MMIO_END_EXCLUSIVE` for PCI MMIO BARs.
///
/// The runtime PCI MMIO router is expected to be mapped across this entire window so a guest OS
/// can legally relocate a PCI BAR anywhere inside the ACPI-reported range.
pub const PCI_MMIO_BASE: u64 = 0xC000_0000;

/// End of the PCI MMIO BAR window (exclusive).
///
/// This is kept right below the IOAPIC MMIO base (`0xFEC0_0000`) so the PCI MMIO window does not
/// overlap fixed chipset MMIO ranges.
pub const PCI_MMIO_END_EXCLUSIVE: u64 = 0xFEC0_0000;

/// Size in bytes of the PCI MMIO BAR window (`PCI_MMIO_END_EXCLUSIVE - PCI_MMIO_BASE`).
pub const PCI_MMIO_SIZE: u64 = PCI_MMIO_END_EXCLUSIVE - PCI_MMIO_BASE;

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

    #[test]
    fn pci_mmio_window_matches_acpi_defaults_and_does_not_overlap_ecam() {
        assert_eq!(PCI_MMIO_BASE, 0xC000_0000);
        assert_eq!(PCI_MMIO_END_EXCLUSIVE, 0xFEC0_0000);
        assert_eq!(PCI_MMIO_SIZE, 0x3EC0_0000);

        // The canonical Q35-style layout places the ECAM window immediately below the PCI MMIO
        // window.
        assert_eq!(PCIE_ECAM_BASE + PCIE_ECAM_SIZE, PCI_MMIO_BASE);
    }
}
