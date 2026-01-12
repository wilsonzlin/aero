use std::io::Cursor;

use aero_devices::pci::{GsiLevelSink, PciBdf, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig};
use aero_pc_platform::{PcPlatform, PcPlatformSnapshotHarness};
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{restore_snapshot, save_snapshot, SaveOptions};

fn snapshot_bytes(pc: &mut PcPlatform) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    let mut harness = PcPlatformSnapshotHarness::new(pc);
    save_snapshot(&mut out, &mut harness, SaveOptions::default()).unwrap();
    out.into_inner()
}

fn restore_bytes(pc: &mut PcPlatform, bytes: &[u8]) {
    let mut harness = PcPlatformSnapshotHarness::new(pc);
    restore_snapshot(&mut Cursor::new(bytes), &mut harness).unwrap();
}

#[test]
fn snapshot_roundtrip_bypasses_a20_gating_for_raw_ram() {
    // 2 MiB: enough for both 0x00000 and 0x1_00000.
    let ram_size = 2 * 1024 * 1024;
    let mut pc = PcPlatform::new(ram_size);

    // Write directly into the underlying RAM backing, bypassing A20 gating.
    pc.memory.ram_mut().write_u8_le(0x0, 0xAA).unwrap();
    pc.memory.ram_mut().write_u8_le(0x1_00000, 0xBB).unwrap();

    // Ensure the underlying RAM contains distinct bytes.
    assert_eq!(pc.memory.ram().read_u8_le(0x0).unwrap(), 0xAA);
    assert_eq!(pc.memory.ram().read_u8_le(0x1_00000).unwrap(), 0xBB);

    // Disable A20: physical accesses alias, but the snapshot should still see full RAM.
    pc.memory.a20().set_enabled(false);

    let snap = snapshot_bytes(&mut pc);

    let mut restored = PcPlatform::new(ram_size);
    restore_bytes(&mut restored, &snap);

    // Validate the restored *raw RAM* still has distinct bytes at both offsets.
    assert_eq!(restored.memory.ram().read_u8_le(0x0).unwrap(), 0xAA);
    assert_eq!(restored.memory.ram().read_u8_le(0x1_00000).unwrap(), 0xBB);
}

#[derive(Default)]
struct NullSink;

impl GsiLevelSink for NullSink {
    fn set_gsi_level(&mut self, _gsi: u32, _level: bool) {}
}

#[test]
fn snapshot_restore_redrives_pci_intx_levels_to_interrupt_sink() {
    let ram_size = 2 * 1024 * 1024;
    let mut pc = PcPlatform::new(ram_size);

    // Configure the legacy PIC so a raised IRQ10 is observable via get_pending_vector().
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false); // cascade
        interrupts.pic_mut().set_masked(10, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Create a router state where 00:00.0 INTA# is asserted, but apply it via `load_state`
    // (which cannot drive the sink). This simulates the restore ordering hazard.
    let bdf = PciBdf::new(0, 0, 0);
    let mut asserted_router = PciIntxRouter::new(PciIntxRouterConfig::default());
    asserted_router.assert_intx(bdf, PciInterruptPin::IntA, &mut NullSink::default());

    let router_state =
        device_state_from_io_snapshot(aero_snapshot::DeviceId::PCI_INTX_ROUTER, &asserted_router);
    apply_io_snapshot_to_device(&router_state, &mut pc.pci_intx).unwrap();

    // The sink hasn't been re-driven yet, so no interrupt should be pending.
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    let snap = snapshot_bytes(&mut pc);

    let mut restored = PcPlatform::new(ram_size);
    restore_bytes(&mut restored, &snap);

    // The snapshot contained an asserted INTx source; after restore, the platform must
    // call `PciIntxRouter::sync_levels_to_sink()` so the asserted GSI is observable.
    let pending = restored
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("expected PCI INTx assertion to raise a PIC IRQ after restore");
    let irq = restored
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 10);
}
