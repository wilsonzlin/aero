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

#[test]
fn snapshot_restore_preserves_pci_intx_refcounts_across_multiple_sources() {
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

    // Choose two distinct sources that intentionally swizzle onto the same PIRQ/GSI so the INTx
    // router's assert refcount logic is exercised across snapshot restore.
    //
    // PIRQ index = (pin.index + device) mod 4; default PIRQ->GSI mapping is 10-13.
    let src1_bdf = PciBdf::new(0, 0, 0);
    let src1_pin = PciInterruptPin::IntA; // index 0
    let src2_bdf = PciBdf::new(0, 1, 0);
    let src2_pin = PciInterruptPin::IntD; // index 3; (3 + 1) mod 4 = 0 -> same as (0 + 0)

    let gsi = {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut interrupts = interrupts.borrow_mut();

        let gsi1 = pci_intx.gsi_for_intx(src1_bdf, src1_pin);
        let gsi2 = pci_intx.gsi_for_intx(src2_bdf, src2_pin);
        assert_eq!(gsi1, gsi2, "sanity: chosen sources should map to the same GSI");

        // Assert both sources.
        pci_intx.assert_intx(src1_bdf, src1_pin, &mut *interrupts);
        pci_intx.assert_intx(src2_bdf, src2_pin, &mut *interrupts);
        assert!(
            interrupts.gsi_level(gsi1),
            "sanity: asserting two sources should raise the routed GSI"
        );

        // Force the sink low while leaving router state asserted. This ensures snapshot restore must
        // call `PciIntxRouter::sync_levels_to_sink()` (and restore router state) to make the sink
        // reflect asserted INTx levels again.
        interrupts.set_gsi_level(gsi1, false);
        assert!(
            !interrupts.gsi_level(gsi1),
            "sanity: test should desync sink state before snapshot"
        );
        gsi1
    };

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let restored_pci_intx = restored
        .pci_intx_router()
        .expect("restored machine should provide a PCI INTx router");
    let restored_interrupts = restored
        .platform_interrupts()
        .expect("restored machine should have PlatformInterrupts");

    assert!(
        restored_interrupts.borrow().gsi_level(gsi),
        "restored machine should re-assert the routed GSI after snapshot restore"
    );

    // Deassert each source in turn; the GSI should remain asserted until the final deassert.
    {
        let mut pci_intx = restored_pci_intx.borrow_mut();
        let mut interrupts = restored_interrupts.borrow_mut();

        pci_intx.deassert_intx(src1_bdf, src1_pin, &mut *interrupts);
        assert!(
            interrupts.gsi_level(gsi),
            "GSI should remain asserted while another INTx source is still asserted"
        );

        pci_intx.deassert_intx(src2_bdf, src2_pin, &mut *interrupts);
        assert!(
            !interrupts.gsi_level(gsi),
            "GSI should deassert once all INTx sources deassert"
        );
    }
}
