//! Guards the canonical Windows 7 storage PCI topology against accidental drift.
//!
//! If you update any of these values, also update:
//! - `docs/05-storage-topology-win7.md`

use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9};
use aero_devices::pci::{PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};

#[test]
fn win7_storage_controllers_have_stable_bdfs_and_intx_gsis() {
    // PCI BDFs (bus:device.function) for storage controllers.
    assert_eq!(IDE_PIIX3.bdf, PciBdf::new(0, 1, 1), "PIIX3 IDE BDF changed");
    assert_eq!(
        ISA_PIIX3.bdf,
        PciBdf::new(0, 1, 0),
        "PIIX3 ISA bridge BDF changed"
    );
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

    // PIIX3 is a multi-function PCI device. The ISA bridge function at 00:01.0 must set the
    // multi-function bit in `header_type` so OS enumeration discovers the IDE function at 00:01.1
    // (and UHCI at 00:01.2).
    assert_eq!(
        ISA_PIIX3.header_type, 0x80,
        "PIIX3 ISA bridge must set multifunction bit (header_type=0x80)"
    );
    let mut isa_cfg = ISA_PIIX3.build_config_space();
    assert_eq!(
        isa_cfg.read(0x0e, 1),
        u32::from(ISA_PIIX3.header_type),
        "PIIX3 ISA header_type byte drifted in config space"
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

    // The PCI config-space Interrupt Line register should be programmed consistently with the
    // router expectations, so guests can discover the routing during enumeration.
    let mut ide_cfg = IDE_PIIX3.build_config_space();
    let mut ahci_cfg = SATA_AHCI_ICH9.build_config_space();
    let mut nvme_cfg = NVME_CONTROLLER.build_config_space();

    assert_eq!(ide_cfg.interrupt_line(), 11);
    assert_eq!(ahci_cfg.interrupt_line(), 12);
    assert_eq!(nvme_cfg.interrupt_line(), 13);
}
