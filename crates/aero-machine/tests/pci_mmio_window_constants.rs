use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_pc_constants::{
    PCIE_ECAM_BASE, PCIE_ECAM_SIZE, PCI_MMIO_BASE, PCI_MMIO_END_EXCLUSIVE, PCI_MMIO_SIZE,
};

#[test]
fn pci_mmio_window_matches_firmware_acpi_policy() {
    // Firmware ACPI builder advertises the canonical PCI MMIO window via PCI0._CRS; ensure the
    // runtime-facing constants used by the machine match that policy exactly.
    assert_eq!(PCI_MMIO_BASE, firmware::acpi::DEFAULT_PCI_MMIO_START);
    assert_eq!(
        PCI_MMIO_END_EXCLUSIVE,
        u64::from(firmware::acpi::IO_APIC_BASE)
    );
    assert_eq!(PCI_MMIO_END_EXCLUSIVE, IOAPIC_MMIO_BASE);
    assert_eq!(PCI_MMIO_SIZE, PCI_MMIO_END_EXCLUSIVE - PCI_MMIO_BASE);

    // ECAM (MCFG/MMCONFIG) is placed immediately below the PCI MMIO window on the canonical PC
    // platform.
    assert_eq!(PCIE_ECAM_BASE + PCIE_ECAM_SIZE, PCI_MMIO_BASE);
}
