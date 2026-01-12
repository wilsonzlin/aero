use std::io::Cursor;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::hpet::HPET_MMIO_BASE;
use aero_devices::ioapic::IoApic;
use aero_devices::pci::{
    GsiLevelSink, PciBdf, PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader};
use aero_pc_platform::PcPlatform;
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result as SnapshotResult, SaveOptions, SnapshotError, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};
use memory::MemoryBus as _;

const RAM_SIZE: usize = 2 * 1024 * 1024;
const CMOS_INDEX_PORT: u16 = 0x70;
const CMOS_DATA_PORT: u16 = 0x71;
const ONE_MIB: u64 = 0x10_0000;

struct PcPlatformSnapshotHarness {
    platform: PcPlatform,
    meta: SnapshotMeta,
}

impl PcPlatformSnapshotHarness {
    fn new(ram_size: usize) -> Self {
        Self {
            platform: PcPlatform::new(ram_size),
            meta: SnapshotMeta {
                snapshot_id: 1,
                parent_snapshot_id: None,
                created_unix_ms: 0,
                label: None,
            },
        }
    }
}

impl SnapshotSource for PcPlatformSnapshotHarness {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Keep meta deterministic so snapshot byte output is stable across runs.
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![
            // Platform interrupt controller complex (PIC + IOAPIC + LAPIC).
            device_state_from_io_snapshot(
                DeviceId::PLATFORM_INTERRUPTS,
                &*self.platform.interrupts.borrow(),
            ),
            // Timers.
            device_state_from_io_snapshot(DeviceId::PIT, &*self.platform.pit().borrow()),
            device_state_from_io_snapshot(DeviceId::RTC, &*self.platform.rtc().borrow()),
            device_state_from_io_snapshot(DeviceId::HPET, &*self.platform.hpet().borrow()),
            // PCI.
            device_state_from_io_snapshot(DeviceId::PCI_CFG, &*self.platform.pci_cfg.borrow()),
            device_state_from_io_snapshot(DeviceId::PCI_INTX_ROUTER, &self.platform.pci_intx),
            // Optional (if available): ACPI PM I/O state.
            device_state_from_io_snapshot(DeviceId::ACPI_PM, &*self.platform.acpi_pm.borrow()),
            // Memory/chipset glue (A20 gate latch only).
            DeviceState {
                id: DeviceId::MEMORY,
                version: 1,
                flags: 0,
                data: vec![self.platform.chipset.a20().enabled() as u8],
            },
        ]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.platform.memory.ram().size()).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> SnapshotResult<()> {
        // Snapshot RAM reads must bypass A20 gating: `MemoryBus::read_physical` masks the physical
        // address when A20 is disabled, which would corrupt the saved RAM image.
        self.platform
            .memory
            .ram()
            .read_into(offset, buf)
            .map_err(|_| SnapshotError::Corrupt("ram read out of range"))?;
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for PcPlatformSnapshotHarness {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        let mut interrupts = None;
        let mut pit = None;
        let mut rtc = None;
        let mut hpet = None;
        let mut pci_cfg = None;
        let mut pci_intx = None;
        let mut acpi_pm = None;
        let mut memory = None;

        for state in states {
            match state.id {
                DeviceId::PLATFORM_INTERRUPTS | DeviceId::APIC => interrupts = Some(state),
                DeviceId::PIT => pit = Some(state),
                DeviceId::RTC => rtc = Some(state),
                DeviceId::HPET => hpet = Some(state),
                DeviceId::PCI_CFG | DeviceId::PCI => pci_cfg = Some(state),
                DeviceId::PCI_INTX_ROUTER => pci_intx = Some(state),
                DeviceId::ACPI_PM => acpi_pm = Some(state),
                DeviceId::MEMORY => memory = Some(state),
                _ => {}
            }
        }

        // 1) Restore platform interrupt controller first so it is a valid sink for
        //    timer/PCI wiring during later restores.
        if let Some(state) = interrupts {
            apply_io_snapshot_to_device(&state, &mut *self.platform.interrupts.borrow_mut())
                .unwrap();
        }

        // 2) Restore non-router devices.
        if let Some(state) = memory {
            if state.version == 1 {
                let enabled = state.data.first().copied().unwrap_or(0) != 0;
                self.platform.chipset.a20().set_enabled(enabled);
            }
        }
        if let Some(state) = pit {
            apply_io_snapshot_to_device(&state, &mut *self.platform.pit().borrow_mut()).unwrap();
        }
        if let Some(state) = rtc {
            apply_io_snapshot_to_device(&state, &mut *self.platform.rtc().borrow_mut()).unwrap();
        }
        if let Some(state) = hpet {
            let hpet = self.platform.hpet();
            let mut hpet = hpet.borrow_mut();
            apply_io_snapshot_to_device(&state, &mut *hpet).unwrap();

            // HPET snapshots restore `general_int_status` but cannot touch the interrupt sink. If a
            // level-triggered timer interrupt was pending at snapshot time, re-drive the line
            // immediately after restore.
            let mut interrupts = self.platform.interrupts.borrow_mut();
            hpet.sync_levels_to_sink(&mut *interrupts);
        }
        if let Some(state) = pci_cfg {
            apply_io_snapshot_to_device(&state, &mut *self.platform.pci_cfg.borrow_mut()).unwrap();
        }
        if let Some(state) = acpi_pm {
            apply_io_snapshot_to_device(&state, &mut *self.platform.acpi_pm.borrow_mut()).unwrap();
        }

        // 3) Restore the INTx router and re-drive asserted levels into the interrupt controller.
        if let Some(state) = pci_intx {
            apply_io_snapshot_to_device(&state, &mut self.platform.pci_intx).unwrap();
        }
        self.platform
            .pci_intx
            .sync_levels_to_sink(&mut *self.platform.interrupts.borrow_mut());
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        usize::try_from(self.platform.memory.ram().size()).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> SnapshotResult<()> {
        // Snapshot RAM writes must bypass A20 gating for the same reason as reads.
        self.platform
            .memory
            .ram_mut()
            .write_from(offset, data)
            .map_err(|_| SnapshotError::Corrupt("ram write out of range"))?;
        Ok(())
    }
}

fn save_snapshot_bytes(source: &mut PcPlatformSnapshotHarness) -> Vec<u8> {
    let mut options = SaveOptions::default();
    options.ram.compression = Compression::None;
    options.ram.chunk_size = 4096;

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, options).unwrap();
    cursor.into_inner()
}

#[test]
fn aero_snapshot_roundtrip_happy_path() {
    let mut src = PcPlatformSnapshotHarness::new(RAM_SIZE);

    // ---- RAM / A20 gating correctness ----
    //
    // Set physical RAM at 0 and 1MiB to different values by temporarily enabling A20.
    // The snapshot must preserve both values even though A20 is disabled at snapshot time.
    src.platform.memory.write_u8(0, 0xAA);
    src.platform.io.write_u8(A20_GATE_PORT, 0x02); // enable A20
    src.platform.memory.write_u8(ONE_MIB, 0xBB);
    src.platform.io.write_u8(A20_GATE_PORT, 0x00); // disable A20 again (snapshot should store 0)

    assert!(!src.platform.chipset.a20().enabled());
    assert_eq!(src.platform.memory.read_u8(ONE_MIB), 0xAA); // A20-masked aliasing

    // ---- Mutate device state ----
    // PIT: program channel 0 reload to 0x1234 (mode 2, lobyte/hibyte).
    src.platform.io.write_u8(PIT_CMD, 0x34);
    src.platform.io.write_u8(PIT_CH0, 0x34);
    src.platform.io.write_u8(PIT_CH0, 0x12);
    src.platform.tick(1_000_000);

    // RTC: write an arbitrary NVRAM byte.
    src.platform.io.write_u8(CMOS_INDEX_PORT, 0x10);
    src.platform.io.write_u8(CMOS_DATA_PORT, 0xAB);

    // HPET: requires A20 enabled to avoid aliasing with IOAPIC base.
    src.platform.io.write_u8(A20_GATE_PORT, 0x02);
    src.platform.memory.write_u64(HPET_MMIO_BASE + 0x10, 0x1); // GEN_CONF_ENABLE
    src.platform.memory.write_u64(HPET_MMIO_BASE + 0xF0, 0x1234);
    src.platform.tick(123_456);
    src.platform.io.write_u8(A20_GATE_PORT, 0x00);

    // PCI config ports: leave the address latch on an arbitrary register and poke the command reg.
    src.platform.io.write(PCI_CFG_ADDR_PORT, 4, 0x8000_0004);
    src.platform.io.write(PCI_CFG_DATA_PORT, 2, 0x0003);

    // PCI INTx: assert a source so the router snapshot is non-empty.
    let bdf = PciBdf::new(0, 0, 0);
    {
        let mut interrupts = src.platform.interrupts.borrow_mut();
        src.platform
            .pci_intx
            .assert_intx(bdf, PciInterruptPin::IntA, &mut *interrupts);
    }

    assert!(!src.platform.chipset.a20().enabled());

    // Capture expected per-device IoSnapshot bytes.
    let expected_intr = src.platform.interrupts.borrow().save_state();
    let expected_pit = src.platform.pit().borrow().save_state();
    let expected_rtc = src.platform.rtc().borrow().save_state();
    let expected_hpet = src.platform.hpet().borrow().save_state();
    let expected_pci_cfg = src.platform.pci_cfg.borrow().save_state();
    let expected_pci_intx = src.platform.pci_intx.save_state();
    let expected_acpi_pm = src.platform.acpi_pm.borrow().save_state();
    let expected_a20 = src.platform.chipset.a20().enabled();

    // Capture raw physical RAM bytes (bypassing A20 gating) for correctness checking.
    let mut ram0 = [0u8; 1];
    let mut ram1 = [0u8; 1];
    src.platform.memory.ram().read_into(0, &mut ram0).unwrap();
    src.platform
        .memory
        .ram()
        .read_into(ONE_MIB, &mut ram1)
        .unwrap();
    assert_eq!(ram0[0], 0xAA);
    assert_eq!(ram1[0], 0xBB);

    // Save snapshot (twice) and assert deterministic byte output.
    let snap1 = save_snapshot_bytes(&mut src);
    let snap2 = save_snapshot_bytes(&mut src);
    assert_eq!(snap1, snap2, "snapshot bytes must be deterministic");

    // Restore into a fresh harness.
    let mut restored = PcPlatformSnapshotHarness::new(RAM_SIZE);
    restore_snapshot(&mut Cursor::new(&snap1), &mut restored).unwrap();

    // Assert device snapshots roundtrip as byte-identical IoSnapshot payloads.
    assert_eq!(
        restored.platform.interrupts.borrow().save_state(),
        expected_intr
    );
    assert_eq!(restored.platform.pit().borrow().save_state(), expected_pit);
    assert_eq!(restored.platform.rtc().borrow().save_state(), expected_rtc);
    assert_eq!(
        restored.platform.hpet().borrow().save_state(),
        expected_hpet
    );
    assert_eq!(
        restored.platform.pci_cfg.borrow().save_state(),
        expected_pci_cfg
    );
    assert_eq!(restored.platform.pci_intx.save_state(), expected_pci_intx);
    assert_eq!(
        restored.platform.acpi_pm.borrow().save_state(),
        expected_acpi_pm
    );
    assert_eq!(restored.platform.chipset.a20().enabled(), expected_a20);

    // Assert raw physical RAM preserved even with A20 disabled.
    let mut restored_ram0 = [0u8; 1];
    let mut restored_ram1 = [0u8; 1];
    restored
        .platform
        .memory
        .ram()
        .read_into(0, &mut restored_ram0)
        .unwrap();
    restored
        .platform
        .memory
        .ram()
        .read_into(ONE_MIB, &mut restored_ram1)
        .unwrap();
    assert_eq!(restored_ram0, ram0);
    assert_eq!(restored_ram1, ram1);

    // Enabling A20 after restore should reveal the preserved 1MiB byte.
    assert!(!restored.platform.chipset.a20().enabled());
    assert_eq!(restored.platform.memory.read_u8(ONE_MIB), 0xAA);
    restored.platform.io.write_u8(A20_GATE_PORT, 0x02);
    assert_eq!(restored.platform.memory.read_u8(ONE_MIB), 0xBB);
}

#[test]
fn aero_snapshot_restore_syncs_pci_intx_levels_into_interrupt_controller() {
    #[derive(Default)]
    struct DummySink;

    impl GsiLevelSink for DummySink {
        fn set_gsi_level(&mut self, _gsi: u32, _level: bool) {}
    }

    let mut src = PcPlatformSnapshotHarness::new(RAM_SIZE);

    // Assert an INTx source, but do not propagate it into PlatformInterrupts (inconsistent snapshot).
    let bdf = PciBdf::new(0, 0, 0);
    let pin = PciInterruptPin::IntA;
    let expected_gsi = src.platform.pci_intx.gsi_for_intx(bdf, pin);
    let mut dummy_sink = DummySink;
    src.platform.pci_intx.assert_intx(bdf, pin, &mut dummy_sink);

    // Sanity: the interrupt controller snapshot still sees the GSI deasserted at save time.
    let intr_bytes = src.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert!(
        (expected_gsi as usize) < levels.len(),
        "expected GSI out of range: {expected_gsi} (len={})",
        levels.len()
    );
    assert_eq!(levels[expected_gsi as usize], 0);

    let snap = save_snapshot_bytes(&mut src);

    let mut restored = PcPlatformSnapshotHarness::new(RAM_SIZE);
    restore_snapshot(&mut Cursor::new(&snap), &mut restored).unwrap();

    // After restore, the adapter must call `PciIntxRouter::sync_levels_to_sink()` so the
    // interrupt controller sees the asserted routed GSI.
    let intr_bytes = restored.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();

    assert_eq!(levels[expected_gsi as usize], 1);
}

#[test]
fn aero_snapshot_restore_syncs_hpet_level_irqs_into_interrupt_controller() {
    // Snapshot restore must re-drive pending HPET level-triggered lines into the interrupt
    // controller. This matters when a snapshot is taken from a VM that was itself restored
    // without calling `Hpet::poll()` yet (i.e. `general_int_status` is pending but the platform
    // sink line is low).
    const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
    const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
    const HPET_REG_TIMER0_BASE: u64 = 0x100;
    const HPET_REG_TIMER_CONFIG: u64 = 0x00;
    const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

    const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
    const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
    const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

    const EXPECTED_GSI: u32 = 2; // timer0 default route

    let mut src = PcPlatformSnapshotHarness::new(RAM_SIZE);

    // Program HPET timer0 to fire a level-triggered interrupt, but deliver it into a dummy sink
    // rather than the platform interrupt controller. This leaves the platform GSI line low while
    // the HPET has a pending `general_int_status` bit set.
    {
        let clock = src.platform.clock();
        let hpet = src.platform.hpet();
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

    // Sanity: platform interrupt controller still sees the line deasserted at save time.
    let intr_bytes = src.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert_eq!(levels[EXPECTED_GSI as usize], 0);

    let snap = save_snapshot_bytes(&mut src);

    let mut restored = PcPlatformSnapshotHarness::new(RAM_SIZE);
    restore_snapshot(&mut Cursor::new(&snap), &mut restored).unwrap();

    // After restore, the adapter must call `Hpet::sync_levels_to_sink()` so the interrupt
    // controller sees the asserted routed GSI.
    let intr_bytes = restored.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert_eq!(levels[EXPECTED_GSI as usize], 1);
}

#[test]
fn aero_snapshot_restore_syncs_hpet_levels_for_non_default_route_into_interrupt_controller() {
    use aero_devices::clock::ManualClock;
    use aero_devices::hpet::Hpet;

    // HPET MMIO register offsets (see `aero_devices::hpet`).
    const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
    const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
    const HPET_REG_TIMER0_BASE: u64 = 0x100;
    const HPET_REG_TIMER_CONFIG: u64 = 0x00;
    const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

    const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
    const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
    const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;
    const HPET_TIMER_CFG_INT_ROUTE_SHIFT: u64 = 9;
    const HPET_TIMER_CFG_INT_ROUTE_MASK: u64 = 0x1F << HPET_TIMER_CFG_INT_ROUTE_SHIFT;

    // Use a non-default timer0 route so this test exercises snapshot restore of the route bits.
    const EXPECTED_GSI: u32 = 17;

    let mut src = PcPlatformSnapshotHarness::new(RAM_SIZE);

    // Generate HPET state (pending level IRQ) without touching the platform interrupt controller.
    let clock = ManualClock::new();
    let mut hpet = Hpet::new_default(clock.clone());
    let mut dummy_sink = IoApic::default();

    hpet.mmio_write(
        HPET_REG_GENERAL_CONFIG,
        8,
        HPET_GEN_CONF_ENABLE,
        &mut dummy_sink,
    );
    let mut timer0_cfg = hpet.mmio_read(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        &mut dummy_sink,
    );
    timer0_cfg |= HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL;
    timer0_cfg = (timer0_cfg & !HPET_TIMER_CFG_INT_ROUTE_MASK)
        | (u64::from(EXPECTED_GSI) << HPET_TIMER_CFG_INT_ROUTE_SHIFT);
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg,
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

    let hpet_state = hpet.save_state();
    src.platform
        .hpet()
        .borrow_mut()
        .load_state(&hpet_state)
        .unwrap();

    // Sanity: platform interrupt controller still sees the line deasserted at save time.
    let intr_bytes = src.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();
    assert!(
        (EXPECTED_GSI as usize) < levels.len(),
        "expected GSI out of range: {EXPECTED_GSI} (len={})",
        levels.len()
    );
    assert_eq!(levels[EXPECTED_GSI as usize], 0);

    let snap = save_snapshot_bytes(&mut src);

    let mut restored = PcPlatformSnapshotHarness::new(RAM_SIZE);
    restore_snapshot(&mut Cursor::new(&snap), &mut restored).unwrap();

    let intr_bytes = restored.platform.interrupts.borrow().save_state();
    let reader = SnapshotReader::parse(&intr_bytes, *b"INTR").unwrap();
    let levels_buf = reader.bytes(8).expect("TAG_GSI_LEVEL missing");
    let mut d = Decoder::new(levels_buf);
    let levels = d.vec_u8().unwrap();
    d.finish().unwrap();

    assert_eq!(levels[EXPECTED_GSI as usize], 1);
}
