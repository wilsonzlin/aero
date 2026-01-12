use std::io::Cursor;

use aero_devices::ioapic::IoApic;
use aero_devices::pci::{
    GsiLevelSink, PciBdf, PciCoreSnapshot, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PCI_CFG_ADDR_PORT,
};
use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::SnapshotReader;
use aero_pc_platform::{PcPlatform, PcPlatformSnapshotHarness};
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{
    restore_snapshot, save_snapshot, CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState,
    SaveOptions, SnapshotMeta, SnapshotSource,
};

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

#[test]
fn snapshot_restore_redrives_hpet_level_to_interrupt_sink() {
    // This is a regression test for HPET restore ordering. HPET snapshots do not include the
    // `irq_asserted` handshake state, so restore must call `Hpet::sync_levels_to_sink()` after the
    // interrupt controller sink is restored.
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // HPET MMIO offsets.
    const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
    const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
    const HPET_REG_TIMER0_BASE: u64 = 0x100;
    const HPET_REG_TIMER_CONFIG: u64 = 0x00;
    const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

    const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
    const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
    const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

    let mut pc = PcPlatform::new(RAM_SIZE);

    // Configure the legacy PIC so IRQ0 delivery is observable.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(0, false);
    }
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    // Program HPET timer0 to fire a level-triggered interrupt, but deliver it into a dummy sink
    // rather than the platform interrupt controller. This leaves the platform GSI line low while
    // HPET has a pending `general_int_status` bit set.
    {
        let clock = pc.clock();
        let hpet = pc.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut dummy_sink = IoApic::default();

        hpet.mmio_write(
            HPET_REG_GENERAL_CONFIG,
            8,
            HPET_GEN_CONF_ENABLE,
            &mut dummy_sink,
        );

        let timer0_cfg = hpet.mmio_read(
            HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
            8,
            &mut dummy_sink,
        );
        hpet.mmio_write(
            HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
            8,
            timer0_cfg | HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL,
            &mut dummy_sink,
        );
        hpet.mmio_write(
            HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
            8,
            1,
            &mut dummy_sink,
        );

        clock.advance_ns(100);
        hpet.poll(&mut dummy_sink);

        assert_ne!(
            hpet.mmio_read(HPET_REG_GENERAL_INT_STATUS, 8, &mut dummy_sink) & 1,
            0,
            "HPET timer0 status bit should be pending before snapshot"
        );
    }

    // Sanity: platform interrupt controller still sees the timer line deasserted at save time.
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);

    let snap = snapshot_bytes(&mut pc);

    let mut restored = PcPlatform::new(RAM_SIZE);
    restore_bytes(&mut restored, &snap);

    // After restore, the adapter must call `Hpet::sync_levels_to_sink()` so the interrupt
    // controller sees the asserted routed GSI (GSI2 -> IRQ0 in legacy mode).
    let pending = restored
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("expected HPET timer0 interrupt to be pending after restore");
    let irq = restored
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 0);
}

#[test]
fn snapshot_restore_keeps_hpet_level_asserted_when_pci_intx_sync_runs() {
    // Regression test: HPET timer2 defaults to routing to GSI10, which is also commonly used for
    // PCI INTA#. During restore we re-drive PCI INTx levels via
    // `PciIntxRouter::sync_levels_to_sink()`, which *deasserts* GSIs that have no INTx sources.
    //
    // The snapshot adapter must ensure PCI sync runs *before* reasserting HPET pending level lines
    // (via `Hpet::sync_levels_to_sink()`), otherwise the PCI sync can incorrectly clear an HPET
    // assertion.
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    // HPET register offsets.
    const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
    const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
    const HPET_REG_TIMER0_BASE: u64 = 0x100;
    const HPET_TIMER_STRIDE: u64 = 0x20;
    const HPET_REG_TIMER_CONFIG: u64 = 0x00;
    const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

    const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
    const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
    const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

    const TIMER2_INDEX: u64 = 2;
    const TIMER2_GSI: u32 = 10;
    const TIMER2_STATUS_BIT: u64 = 1 << TIMER2_INDEX;

    let mut pc = PcPlatform::new(RAM_SIZE);

    // Sanity: the platform interrupt controller should not see the HPET line asserted at save
    // time; we program HPET using a dummy sink.
    assert!(!pc.interrupts.borrow().gsi_level(TIMER2_GSI));

    // Program HPET timer2 to fire a level-triggered interrupt, but deliver it into a dummy sink
    // rather than the platform interrupt controller. This leaves the platform GSI line low while
    // HPET has a pending `general_int_status` bit set.
    {
        let clock = pc.clock();
        let hpet = pc.hpet();
        let mut hpet = hpet.borrow_mut();
        let mut dummy_sink = IoApic::default();

        hpet.mmio_write(
            HPET_REG_GENERAL_CONFIG,
            8,
            HPET_GEN_CONF_ENABLE,
            &mut dummy_sink,
        );

        let timer2_base = HPET_REG_TIMER0_BASE + TIMER2_INDEX * HPET_TIMER_STRIDE;
        let timer2_cfg = hpet.mmio_read(timer2_base + HPET_REG_TIMER_CONFIG, 8, &mut dummy_sink);
        hpet.mmio_write(
            timer2_base + HPET_REG_TIMER_CONFIG,
            8,
            timer2_cfg | HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL,
            &mut dummy_sink,
        );
        hpet.mmio_write(
            timer2_base + HPET_REG_TIMER_COMPARATOR,
            8,
            1,
            &mut dummy_sink,
        );

        clock.advance_ns(100);
        hpet.poll(&mut dummy_sink);

        assert_ne!(
            hpet.mmio_read(HPET_REG_GENERAL_INT_STATUS, 8, &mut dummy_sink) & TIMER2_STATUS_BIT,
            0,
            "HPET timer2 status bit should be pending before snapshot"
        );
    }

    // Sanity: platform interrupt controller still sees the timer line deasserted at save time.
    assert!(!pc.interrupts.borrow().gsi_level(TIMER2_GSI));

    let snap = snapshot_bytes(&mut pc);

    let mut restored = PcPlatform::new(RAM_SIZE);
    restore_bytes(&mut restored, &snap);

    // After restore, HPET should have reasserted the pending level GSI, and PCI INTx sync should
    // not clear it.
    assert!(
        restored.interrupts.borrow().gsi_level(TIMER2_GSI),
        "expected HPET timer2 pending interrupt to assert GSI{TIMER2_GSI} after restore"
    );
}

#[test]
fn snapshot_restore_accepts_legacy_apic_and_pci_core_wrapper() {
    // Regression test for snapshot backward compatibility:
    // - `DeviceId::APIC` (legacy) for the platform interrupts snapshot.
    // - `DeviceId::PCI` (legacy) containing a `PciCoreSnapshot` wrapper (`PCIC`) that nests both
    //   config ports (`PCPT`) and the INTx router (`INTX`).
    //
    // Restore must apply the nested PCI core state and re-drive asserted INTx levels into the
    // restored interrupt controller via `sync_levels_to_sink()`.
    const RAM_SIZE: usize = 2 * 1024 * 1024;

    struct LegacySource {
        pc: std::cell::RefCell<PcPlatform>,
    }

    impl SnapshotSource for LegacySource {
        fn snapshot_meta(&mut self) -> SnapshotMeta {
            SnapshotMeta::default()
        }

        fn cpu_state(&self) -> CpuState {
            CpuState::default()
        }

        fn mmu_state(&self) -> MmuState {
            MmuState::default()
        }

        fn device_states(&self) -> Vec<DeviceState> {
            let mut pc = self.pc.borrow_mut();

            let interrupts =
                device_state_from_io_snapshot(DeviceId::APIC, &*pc.interrupts.borrow());

            // Build a legacy combined PCI core snapshot (`PCIC`) under the historical `DeviceId::PCI`.
            //
            // `pc.pci_cfg` is behind a `RefCell`, while `pc.pci_intx` is a plain field. To satisfy the
            // borrow checker without `unsafe`, temporarily move the router out of the platform while
            // we borrow `pci_cfg` mutably.
            let mut intx_router = std::mem::replace(
                &mut pc.pci_intx,
                PciIntxRouter::new(PciIntxRouterConfig::default()),
            );
            let pci_core = {
                let mut cfg_ports = pc.pci_cfg.borrow_mut();
                let core = PciCoreSnapshot::new(&mut cfg_ports, &mut intx_router);
                device_state_from_io_snapshot(DeviceId::PCI, &core)
            };
            pc.pci_intx = intx_router;

            vec![interrupts, pci_core]
        }

        fn disk_overlays(&self) -> DiskOverlayRefs {
            DiskOverlayRefs::default()
        }

        fn ram_len(&self) -> usize {
            usize::try_from(self.pc.borrow().memory.ram().size()).unwrap_or(0)
        }

        fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
            self.pc
                .borrow()
                .memory
                .ram()
                .read_into(offset, buf)
                .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram read out of range"))?;
            Ok(())
        }

        fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
            None
        }
    }

    let mut pc = PcPlatform::new(RAM_SIZE);

    // Set PCI config address latch to a non-default value so we can assert it round-trips.
    pc.io.write(PCI_CFG_ADDR_PORT, 4, 0x8000_0004);
    let expected_latch = pc.io.read(PCI_CFG_ADDR_PORT, 4);

    // Assert an INTx source via a dummy sink, creating an intentionally inconsistent snapshot
    // where the router says the line is asserted but the interrupt controller does not.
    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;
    let expected_gsi = pc.pci_intx.gsi_for_intx(bdf, pin);
    pc.pci_intx.assert_intx(bdf, pin, &mut NullSink::default());

    // Sanity: GSI level is low at snapshot time.
    let intr_state = device_state_from_io_snapshot(DeviceId::APIC, &*pc.interrupts.borrow());
    let reader = SnapshotReader::parse(&intr_state.data, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert_eq!(levels[expected_gsi as usize], 0);

    let mut cursor = Cursor::new(Vec::new());
    let mut source = LegacySource {
        pc: std::cell::RefCell::new(pc),
    };
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let snap = cursor.into_inner();

    let mut restored = PcPlatform::new(RAM_SIZE);
    restore_bytes(&mut restored, &snap);

    // PCI config address latch should be restored from the nested `PCPT` snapshot.
    assert_eq!(restored.io.read(PCI_CFG_ADDR_PORT, 4), expected_latch);

    // INTx router levels must be re-driven into the interrupt controller after restore.
    let intr_state = device_state_from_io_snapshot(DeviceId::APIC, &*restored.interrupts.borrow());
    let reader = SnapshotReader::parse(&intr_state.data, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert_eq!(levels[expected_gsi as usize], 1);
}
