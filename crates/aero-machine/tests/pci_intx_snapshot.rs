use aero_devices::pci::{GsiLevelSink, PciBdf, PciInterruptPin};
use aero_machine::{Machine, MachineConfig};

#[test]
fn snapshot_restore_preserves_pci_intx_asserted_gsi() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();
    let pci_intx = src
        .pci_intx_router()
        .expect("pc platform should provide a PCI INTx router");
    let interrupts = src
        .platform_interrupts()
        .expect("pc platform should provide PlatformInterrupts");

    // Deterministic INTx source: bus 0, device 0, function 0, INTA#.
    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;

    // Assert INTx via the router, then intentionally desynchronize the platform interrupt
    // controller's view of the GSI level. Snapshot restore is expected to call
    // `PciIntxRouter::sync_levels_to_sink()` to re-drive asserted lines, so the restored machine
    // should see the GSI asserted again.
    let gsi = {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut interrupts = interrupts.borrow_mut();

        let gsi = pci_intx.gsi_for_intx(bdf, pin);
        pci_intx.assert_intx(bdf, pin, &mut *interrupts);
        assert!(
            interrupts.gsi_level(gsi),
            "sanity: INTx assert should raise the routed GSI"
        );

        // Force the platform controller low without updating the router; this simulates the state
        // that exists immediately after restoring `PciIntxRouter` (router knows the line is
        // asserted, but the platform sink has not been updated yet).
        interrupts.set_gsi_level(gsi, false);
        assert!(
            !interrupts.gsi_level(gsi),
            "sanity: test should desync sink state before snapshot"
        );
        gsi
    };

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let restored_interrupts = restored
        .platform_interrupts()
        .expect("restored machine should have PlatformInterrupts");
    assert!(
        restored_interrupts.borrow().gsi_level(gsi),
        "restored machine should re-assert the routed GSI after snapshot restore"
    );
}
