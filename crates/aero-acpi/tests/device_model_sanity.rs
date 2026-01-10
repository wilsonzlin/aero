use aero_acpi::AcpiConfig;
use aero_devices::apic::{IOAPIC_MMIO_BASE, LAPIC_MMIO_BASE};
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::pci::PciIntxRouterConfig;

#[test]
fn default_acpi_addresses_match_device_model_bases() {
    let cfg = AcpiConfig::default();

    assert_eq!(cfg.local_apic_addr as u64, LAPIC_MMIO_BASE);
    assert_eq!(cfg.io_apic_addr as u64, IOAPIC_MMIO_BASE);
    assert_eq!(cfg.hpet_addr, HPET_MMIO_BASE);
}

#[test]
fn default_pci_pirq_routing_matches_device_model_router() {
    let cfg = AcpiConfig::default();
    assert_eq!(cfg.pirq_to_gsi, PciIntxRouterConfig::default().pirq_to_gsi);
}

