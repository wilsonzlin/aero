use aero_devices::pci::{GsiLevelSink, PciBdf, PciInterruptPin};
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;

#[test]
fn snapshot_restore_preserves_pci_intx_asserted_gsi() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal for deterministic interrupt behavior.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
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
        // Keep the machine minimal for deterministic interrupt behavior.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
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
        assert_eq!(
            gsi1, gsi2,
            "sanity: chosen sources should map to the same GSI"
        );

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

#[test]
fn snapshot_restore_redrives_pci_intx_into_legacy_pic_after_ack() {
    // This test specifically validates that `Machine::restore_snapshot_bytes()` replays the
    // restored PCI INTx line levels into *both* the IOAPIC-level bookkeeping and the legacy PIC
    // (via `GsiLevelSink for PlatformInterrupts`) when running in `PlatformInterruptMode::LegacyPic`.
    //
    // The PIC is edge-triggered by default, so we explicitly ACK+EOI the interrupt before taking
    // the snapshot. This ensures the snapshot captures a quiescent PIC state even though the INTx
    // router still thinks the line is asserted; after restore, the re-drive should create a fresh
    // edge and re-establish a pending PIC vector.
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();
    let pci_intx = src
        .pci_intx_router()
        .expect("pc platform should provide a PCI INTx router");
    let interrupts = src
        .platform_interrupts()
        .expect("pc platform should provide PlatformInterrupts");

    // Two distinct sources that swizzle onto the same routed GSI under the default INTx router
    // config.
    let src1_bdf = PciBdf::new(0, 0, 0);
    let src1_pin = PciInterruptPin::IntA; // index 0
    let src2_bdf = PciBdf::new(0, 1, 0);
    let src2_pin = PciInterruptPin::IntD; // index 3; (3 + 1) mod 4 = 0 -> same as (0 + 0)

    let (gsi, irq, vector) = {
        let pci_intx = pci_intx.borrow();
        let gsi = pci_intx.gsi_for_intx(src1_bdf, src1_pin);
        assert_eq!(
            gsi,
            pci_intx.gsi_for_intx(src2_bdf, src2_pin),
            "sanity: chosen sources should map to the same GSI"
        );
        assert!(
            gsi < 16,
            "expected PCI INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
        );
        let irq = u8::try_from(gsi).unwrap();
        let vector = if irq < 8 {
            0x20 + irq
        } else {
            0x28 + (irq - 8)
        };
        (gsi, irq, vector)
    };

    // Configure the PIC for deterministic vectors and unmask the routed IRQ (and cascade).
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        if irq >= 8 {
            ints.pic_mut().set_masked(2, false); // cascade
        }
        ints.pic_mut().set_masked(irq, false);
    }

    // Assert both sources and verify the PIC sees a pending vector.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut ints = interrupts.borrow_mut();
        pci_intx.assert_intx(src1_bdf, src1_pin, &mut *ints);
        pci_intx.assert_intx(src2_bdf, src2_pin, &mut *ints);
    }
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));

    // Simulate the CPU taking the interrupt and completing an EOI, but keep the router asserted.
    // In an edge-triggered PIC, this clears IRR/ISR and requires a new edge to re-pend.
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(vector);
        ints.eoi(vector);
        assert_eq!(
            ints.get_pending(),
            None,
            "sanity: IRQ should no longer be pending after ACK+EOI"
        );

        // Desync the sink low without updating the router so snapshot restore must re-drive.
        ints.set_gsi_level(gsi, false);
        assert!(
            !ints.gsi_level(gsi),
            "sanity: sink should be deasserted before snapshot"
        );
        assert_eq!(
            ints.get_pending(),
            None,
            "sanity: IRQ should remain non-pending after sink deassert in edge-triggered PIC"
        );
    }

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
    assert_eq!(
        restored_interrupts.borrow().get_pending(),
        Some(vector),
        "restored machine should re-pend the IRQ via legacy PIC after sync"
    );
}
