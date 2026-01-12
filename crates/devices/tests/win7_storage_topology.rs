//! Guards the canonical Windows 7 storage PCI topology against accidental drift.
//!
//! If you update any of these values, also update:
//! - `docs/05-storage-topology-win7.md`

use aero_devices::pci::profile::{IDE_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9};
use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};

#[test]
fn win7_storage_controllers_have_stable_bdfs_and_intx_gsis() {
    // PCI BDFs (bus:device.function) for storage controllers.
    assert_eq!(IDE_PIIX3.bdf, PciBdf::new(0, 1, 1), "PIIX3 IDE BDF changed");
    assert_eq!(
        SATA_AHCI_ICH9.bdf,
        PciBdf::new(0, 2, 0),
        "ICH9 AHCI BDF changed"
    );

    // NVMe is optional/off by default for Win7, but its BDF is still reserved to keep
    // enumeration deterministic when enabled.
    assert_eq!(
        NVME_CONTROLLER.bdf,
        PciBdf::new(0, 3, 0),
        "NVMe controller BDF changed"
    );

    // All canonical profiles use INTA# and rely on the platform INTx router.
    assert_eq!(IDE_PIIX3.interrupt_pin, Some(PciInterruptPin::IntA));
    assert_eq!(SATA_AHCI_ICH9.interrupt_pin, Some(PciInterruptPin::IntA));
    assert_eq!(NVME_CONTROLLER.interrupt_pin, Some(PciInterruptPin::IntA));

    // Under the default INTx router config:
    // - PIRQ[A-D] -> GSI[10,11,12,13]
    // - Root-bus swizzle: PIRQ = (INTx + device_number) mod 4
    let router = PciIntxRouter::new(PciIntxRouterConfig::default());

    assert_eq!(
        router.gsi_for_intx(IDE_PIIX3.bdf, PciInterruptPin::IntA),
        11,
        "PIIX3 IDE INTA# should route to GSI 11"
    );
    assert_eq!(
        router.gsi_for_intx(SATA_AHCI_ICH9.bdf, PciInterruptPin::IntA),
        12,
        "ICH9 AHCI INTA# should route to GSI 12"
    );
    assert_eq!(
        router.gsi_for_intx(NVME_CONTROLLER.bdf, PciInterruptPin::IntA),
        13,
        "NVMe INTA# should route to GSI 13"
    );
}

