//! Guards the canonical Windows 7 storage PCI topology against accidental drift.
//!
//! If you update any of these values, also update:
//! - `docs/05-storage-topology-win7.md`
//! - `crates/devices/tests/win7_storage_topology.rs`
//! - `crates/aero-pc-platform/tests/pc_platform_win7_storage.rs`

use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, SATA_AHCI_ICH9};
use aero_devices::pci::{PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};
use aero_machine::{Machine, MachineConfig};

#[test]
fn machine_win7_storage_topology_has_stable_bdfs_and_interrupt_lines() {
    let m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ahci: true,
        enable_ide: true,
        // Keep this test focused on PCI topology and avoid unrelated devices.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    // --- Canonical BDFs present ---

    // ISA PIIX3 @ 00:01.0 (multi-function header).
    {
        let cfg = bus
            .device_config_mut(ISA_PIIX3.bdf)
            .expect("ISA_PIIX3 config function missing from PCI bus");

        let header_type = cfg.read(0x0e, 1) as u8;
        assert_eq!(
            header_type & 0x80,
            0x80,
            "ISA_PIIX3 header type multi-function bit drifted"
        );
    }

    // IDE PIIX3 @ 00:01.1.
    {
        let _cfg = bus
            .device_config_mut(IDE_PIIX3.bdf)
            .expect("IDE_PIIX3 config function missing from PCI bus");
    }

    // SATA AHCI ICH9 @ 00:02.0.
    {
        let _cfg = bus
            .device_config_mut(SATA_AHCI_ICH9.bdf)
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
            .device_config_mut(IDE_PIIX3.bdf)
            .expect("IDE_PIIX3 config function missing from PCI bus");

        let expected_gsi = router.gsi_for_intx(IDE_PIIX3.bdf, PciInterruptPin::IntA);
        assert_eq!(expected_gsi, 11, "IDE expected GSI drifted");
        assert_eq!(
            cfg.interrupt_line(),
            expected_gsi as u8,
            "IDE PCI Interrupt Line does not match router swizzle"
        );

        // Optional guard: ensure the Bus Master IDE BAR is defined.
        assert!(
            cfg.bar_definition(4).is_some(),
            "IDE_PIIX3 BAR4 definition missing"
        );
    }

    // AHCI 00:02.0 INTA -> GSI 12.
    {
        let cfg = bus
            .device_config_mut(SATA_AHCI_ICH9.bdf)
            .expect("SATA_AHCI_ICH9 config function missing from PCI bus");

        let expected_gsi = router.gsi_for_intx(SATA_AHCI_ICH9.bdf, PciInterruptPin::IntA);
        assert_eq!(expected_gsi, 12, "AHCI expected GSI drifted");
        assert_eq!(
            cfg.interrupt_line(),
            expected_gsi as u8,
            "AHCI PCI Interrupt Line does not match router swizzle"
        );

        // Optional guard: ensure the AHCI ABAR (BAR5) is defined.
        assert!(
            cfg.bar_definition(5).is_some(),
            "SATA_AHCI_ICH9 BAR5 definition missing"
        );
    }
}
