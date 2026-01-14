use aero_acpi::AcpiConfig;
use aero_devices::pci::PciIntxRouterConfig;
use firmware::bios::BiosConfig;

#[test]
fn default_pirq_to_gsi_mapping_is_consistent_across_firmware_acpi_and_pci_routing() {
    let bios = BiosConfig::default().pirq_to_gsi;
    let acpi = AcpiConfig::default().pirq_to_gsi;
    let pci_router = PciIntxRouterConfig::default().pirq_to_gsi;

    assert_eq!(
        bios, acpi,
        "BiosConfig::default().pirq_to_gsi must match AcpiConfig::default().pirq_to_gsi"
    );
    assert_eq!(
        bios, pci_router,
        "BiosConfig::default().pirq_to_gsi must match PciIntxRouterConfig::default().pirq_to_gsi"
    );
}
