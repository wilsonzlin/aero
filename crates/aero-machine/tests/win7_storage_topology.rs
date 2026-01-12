//! Guards the canonical Windows 7 storage PCI topology against accidental drift.
//!
//! If you update any of these values, also update:
//! - `docs/05-storage-topology-win7.md`
//! - `crates/devices/tests/win7_storage_topology.rs`
//! - `crates/aero-machine/tests/machine_win7_storage_topology.rs`
//! - `crates/aero-pc-platform/tests/pc_platform_win7_storage.rs`
//! - `crates/aero-pc-platform/tests/windows7_storage_topology.rs`

use aero_devices::pci::profile::{
    AHCI_ABAR_BAR_INDEX, AHCI_ABAR_SIZE_U32, IDE_PIIX3, ISA_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9,
};
use aero_devices::pci::{
    PciBarDefinition, PciBarKind, PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_machine::{Machine, MachineConfig};

#[test]
fn machine_win7_storage_topology_has_stable_bdfs_and_interrupt_lines() {
    // Freeze the canonical BDFs (bus:device.function) for the Win7 storage topology.
    //
    // This is the contract documented in `docs/05-storage-topology-win7.md`; if any of these
    // change, Windows 7 installation/boot behavior (and snapshot + frontend expectations) may
    // drift.
    const ISA_BDF: PciBdf = PciBdf::new(0, 1, 0);
    const IDE_BDF: PciBdf = PciBdf::new(0, 1, 1);
    const AHCI_BDF: PciBdf = PciBdf::new(0, 2, 0);

    assert_eq!(ISA_PIIX3.bdf, ISA_BDF, "ISA_PIIX3 BDF drifted");
    assert_eq!(IDE_PIIX3.bdf, IDE_BDF, "IDE_PIIX3 BDF drifted");
    assert_eq!(SATA_AHCI_ICH9.bdf, AHCI_BDF, "SATA_AHCI_ICH9 BDF drifted");

    let mut cfg = MachineConfig::win7_storage_defaults(2 * 1024 * 1024);
    // Keep this test focused on PCI topology and avoid unrelated devices.
    cfg.enable_vga = false;
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;

    let m = Machine::new(cfg).unwrap();

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    // --- Canonical BDFs present ---

    // ISA PIIX3 @ 00:01.0 (multi-function header).
    {
        let cfg = bus
            .device_config_mut(ISA_BDF)
            .expect("ISA_PIIX3 config function missing from PCI bus");

        let header_type = cfg.read(0x0e, 1) as u8;
        assert_eq!(
            header_type, 0x80,
            "ISA_PIIX3 header type drifted (expected multi-function bit set and type 0)"
        );
    }

    // IDE PIIX3 @ 00:01.1.
    {
        let _cfg = bus
            .device_config_mut(IDE_BDF)
            .expect("IDE_PIIX3 config function missing from PCI bus");
    }

    // SATA AHCI ICH9 @ 00:02.0.
    {
        let _cfg = bus
            .device_config_mut(AHCI_BDF)
            .expect("SATA_AHCI_ICH9 config function missing from PCI bus");
    }

    // --- Interrupt Line values match the default router swizzle ---
    //
    // Under `PciIntxRouterConfig::default()`:
    // - PIRQ[A-D] -> GSI[10,11,12,13]
    // - Root-bus swizzle: PIRQ = (INTx + device_number) mod 4
    let router = PciIntxRouter::new(PciIntxRouterConfig::default());

    // IDE 00:01.1 INTA -> GSI 11.
    {
        let cfg = bus
            .device_config_mut(IDE_BDF)
            .expect("IDE_PIIX3 config function missing from PCI bus");

        let expected_gsi = router.gsi_for_intx(IDE_BDF, PciInterruptPin::IntA);
        assert_eq!(expected_gsi, 11, "IDE expected GSI drifted");
        assert_eq!(
            cfg.interrupt_line(),
            expected_gsi as u8,
            "IDE PCI Interrupt Line does not match router swizzle"
        );
        assert_eq!(
            cfg.interrupt_pin(),
            1,
            "IDE PCI Interrupt Pin drifted (expected INTA#)"
        );

        // Optional guard: ensure the Bus Master IDE BAR is defined.
        assert_eq!(
            cfg.bar_definition(4),
            Some(PciBarDefinition::Io { size: 16 }),
            "IDE_PIIX3 BAR4 definition drifted"
        );

        // Freeze legacy-compat BAR base assignments so firmware/OSes that assume PC-like port
        // layouts continue to work. These are documented in `docs/05-storage-topology-win7.md`.
        //
        // Note: These are hard-coded numeric constants (not derived from device-model constants)
        // so this test truly freezes the canonical Windows 7 ABI.
        assert_eq!(
            cfg.bar_range(0).map(|r| (r.kind, r.base, r.size)),
            Some((PciBarKind::Io, 0x1F0, 8)),
            "IDE_PIIX3 BAR0 (primary cmd block) drifted"
        );
        assert_eq!(
            cfg.bar_range(1).map(|r| (r.kind, r.base, r.size)),
            Some((PciBarKind::Io, 0x3F4, 4)),
            "IDE_PIIX3 BAR1 (primary control block base) drifted"
        );
        assert_eq!(
            cfg.bar_range(2).map(|r| (r.kind, r.base, r.size)),
            Some((PciBarKind::Io, 0x170, 8)),
            "IDE_PIIX3 BAR2 (secondary cmd block) drifted"
        );
        assert_eq!(
            cfg.bar_range(3).map(|r| (r.kind, r.base, r.size)),
            Some((PciBarKind::Io, 0x374, 4)),
            "IDE_PIIX3 BAR3 (secondary control block base) drifted"
        );
        assert_eq!(
            cfg.bar_range(4).map(|r| (r.kind, r.base, r.size)),
            Some((PciBarKind::Io, 0xC000, 16)),
            "IDE_PIIX3 BAR4 (bus master IDE) base/size drifted"
        );
    }

    // AHCI 00:02.0 INTA -> GSI 12.
    {
        let cfg = bus
            .device_config_mut(AHCI_BDF)
            .expect("SATA_AHCI_ICH9 config function missing from PCI bus");

        let expected_gsi = router.gsi_for_intx(AHCI_BDF, PciInterruptPin::IntA);
        assert_eq!(expected_gsi, 12, "AHCI expected GSI drifted");
        assert_eq!(
            cfg.interrupt_line(),
            expected_gsi as u8,
            "AHCI PCI Interrupt Line does not match router swizzle"
        );
        assert_eq!(
            cfg.interrupt_pin(),
            1,
            "AHCI PCI Interrupt Pin drifted (expected INTA#)"
        );

        // Optional guard: ensure the AHCI ABAR (BAR5) is defined.
        //
        // Note: This intentionally hard-codes BAR index/size (rather than referencing
        // `aero_devices::pci::profile::*` constants) so we catch accidental drift in the profile
        // layer as well.
        assert_eq!(
            cfg.bar_definition(AHCI_ABAR_BAR_INDEX),
            Some(PciBarDefinition::Mmio32 {
                size: AHCI_ABAR_SIZE_U32,
                prefetchable: false
            }),
            "SATA_AHCI_ICH9 BAR5 definition drifted"
        );
    }
}

#[test]
fn machine_win7_storage_topology_nvme_enabled_has_canonical_bdf_and_interrupt_line() {
    // NVMe is off by default for Win7 (no inbox NVMe driver), but the BDF is reserved and must be
    // stable when the controller is explicitly enabled.
    const NVME_BDF: PciBdf = PciBdf::new(0, 3, 0);
    assert_eq!(NVME_CONTROLLER.bdf, NVME_BDF, "NVME_CONTROLLER BDF drifted");

    let mut cfg = MachineConfig::win7_storage_defaults(2 * 1024 * 1024);
    cfg.enable_nvme = true;
    // Keep this test focused on PCI topology and avoid unrelated devices.
    cfg.enable_vga = false;
    cfg.enable_serial = false;
    cfg.enable_i8042 = false;
    cfg.enable_a20_gate = false;
    cfg.enable_reset_ctrl = false;

    let m = Machine::new(cfg).unwrap();

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    // Under `PciIntxRouterConfig::default()`:
    // - PIRQ[A-D] -> GSI[10,11,12,13]
    // - Root-bus swizzle: PIRQ = (INTx + device_number) mod 4
    let router = PciIntxRouter::new(PciIntxRouterConfig::default());

    let cfg = bus
        .device_config_mut(NVME_BDF)
        .expect("NVME_CONTROLLER config function missing from PCI bus");

    // NVMe 00:03.0 INTA -> GSI 13.
    let expected_gsi = router.gsi_for_intx(NVME_BDF, PciInterruptPin::IntA);
    assert_eq!(expected_gsi, 13, "NVMe expected GSI drifted");
    assert_eq!(
        cfg.interrupt_line(),
        expected_gsi as u8,
        "NVMe PCI Interrupt Line does not match router swizzle"
    );
    assert_eq!(
        cfg.interrupt_pin(),
        1,
        "NVMe PCI Interrupt Pin drifted (expected INTA#)"
    );
}
