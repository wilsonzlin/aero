use std::cell::{Cell, RefCell};
use std::io::Cursor;
use std::rc::Rc;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo};
use aero_devices::clock::ManualClock;
use aero_devices::irq::IrqLine;
use aero_platform::io::PortIoDevice;
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotMeta, SnapshotSource, SnapshotTarget,
};

#[derive(Clone, Default)]
struct RecordingIrqLine {
    level: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<bool>>>,
}

impl RecordingIrqLine {
    fn level(&self) -> bool {
        self.level.get()
    }

    fn events(&self) -> Vec<bool> {
        self.events.borrow().clone()
    }
}

impl IrqLine for RecordingIrqLine {
    fn set_level(&self, level: bool) {
        self.level.set(level);
        self.events.borrow_mut().push(level);
    }
}

struct TestSource {
    meta: SnapshotMeta,
    pm: AcpiPmIo<ManualClock>,
    ram: Vec<u8>,
}

impl SnapshotSource for TestSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Keep meta deterministic so `save_snapshot` output is stable for this test.
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![device_state_from_io_snapshot(DeviceId::ACPI_PM, &self.pm)]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + buf.len() > self.ram.len() {
            return Err(aero_snapshot::SnapshotError::Corrupt(
                "ram read out of bounds",
            ));
        }
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TestTarget {
    pm: AcpiPmIo<ManualClock>,
    ram: Vec<u8>,
}

impl SnapshotTarget for TestTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::ACPI_PM {
                apply_io_snapshot_to_device(&state, &mut self.pm).unwrap();
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram.len() {
            return Err(aero_snapshot::SnapshotError::Corrupt(
                "ram write out of bounds",
            ));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

fn save_bytes(source: &mut TestSource) -> Vec<u8> {
    let mut options = SaveOptions::default();
    options.ram.compression = Compression::None;
    options.ram.chunk_size = 4096;

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, options).unwrap();
    cursor.into_inner()
}

#[test]
fn acpi_pm_io_snapshot_roundtrips_through_aero_snapshot_file() {
    let cfg = AcpiPmConfig::default();
    let half = (cfg.gpe0_blk_len as usize) / 2;

    let clock0 = ManualClock::new();
    let irq0 = RecordingIrqLine::default();
    let mut pm0 = AcpiPmIo::new_with_callbacks_and_clock(
        cfg,
        AcpiPmCallbacks {
            sci_irq: Box::new(irq0.clone()),
            request_power_off: None,
        },
        clock0.clone(),
    );

    // Ensure the PM timer has a non-zero value that must be preserved via re-anchoring on restore.
    clock0.set_ns(1_000_000);

    // Program some non-zero PM1 and GPE0 state. Keep `PM1_STS & PM1_EN == 0` so SCI is asserted
    // solely due to GPE0 (ensures the test will fail if GPE0 state is not restored correctly).
    pm0.write(cfg.pm1a_evt_blk + 2, 2, 0x0100);
    pm0.trigger_pm1_event(0x0200);
    pm0.write(cfg.pm1a_cnt_blk, 2, 0x1235);

    for i in 0..half {
        let v = 1u8 << (i.min(7) as u32);
        pm0.write(
            cfg.gpe0_blk + half as u16 + i as u16,
            1,
            u32::from(v),
        );
        pm0.trigger_gpe0(i, v);
    }

    assert!(
        pm0.sci_level(),
        "SCI should be asserted before snapshot (gated by SCI_EN and pending GPE)"
    );
    assert!(
        irq0.level(),
        "SCI line should be driven high before snapshot"
    );

    let expected_pm1_sts = pm0.pm1_status();
    let expected_pm1_en = pm0.read(cfg.pm1a_evt_blk + 2, 2) as u16;
    let expected_pm1_cnt = pm0.pm1_cnt();

    let expected_gpe0_sts: Vec<u8> = (0..half)
        .map(|i| pm0.read(cfg.gpe0_blk + i as u16, 1) as u8)
        .collect();
    let expected_gpe0_en: Vec<u8> = (0..half)
        .map(|i| pm0.read(cfg.gpe0_blk + half as u16 + i as u16, 1) as u8)
        .collect();

    let expected_pm_tmr = pm0.read(cfg.pm_tmr_blk, 4);

    let mut source = TestSource {
        meta: SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        },
        pm: pm0,
        ram: vec![0u8; 4096],
    };

    let snap1 = save_bytes(&mut source);
    let snap2 = save_bytes(&mut source);
    assert_eq!(snap1, snap2, "snapshot bytes must be deterministic");

    // Restore into a fresh device with a different clock origin to validate PM_TMR re-anchoring.
    let clock1 = ManualClock::new();
    clock1.set_ns(9_000_000);

    let irq1 = RecordingIrqLine::default();
    let mut target = TestTarget {
        pm: AcpiPmIo::new_with_callbacks_and_clock(
            cfg,
            AcpiPmCallbacks {
                sci_irq: Box::new(irq1.clone()),
                request_power_off: None,
            },
            clock1,
        ),
        ram: vec![0u8; 4096],
    };

    restore_snapshot(&mut Cursor::new(&snap1), &mut target).unwrap();

    assert_eq!(target.pm.pm1_status(), expected_pm1_sts);
    assert_eq!(
        target.pm.read(cfg.pm1a_evt_blk + 2, 2) as u16,
        expected_pm1_en
    );
    assert_eq!(target.pm.pm1_cnt(), expected_pm1_cnt);

    for (i, expected) in expected_gpe0_sts.iter().copied().enumerate() {
        assert_eq!(
            target.pm.read(cfg.gpe0_blk + i as u16, 1) as u8,
            expected,
            "GPE0_STS byte {i}"
        );
    }
    for (i, expected) in expected_gpe0_en.iter().copied().enumerate() {
        assert_eq!(
            target.pm.read(cfg.gpe0_blk + half as u16 + i as u16, 1) as u8,
            expected,
            "GPE0_EN byte {i}"
        );
    }

    assert_eq!(
        target.pm.read(cfg.pm_tmr_blk, 4),
        expected_pm_tmr,
        "PM_TMR must match after restore at a different clock origin"
    );

    assert!(target.pm.sci_level());
    assert!(
        irq1.level(),
        "restored ACPI PM should re-drive SCI based on restored state"
    );
    assert_eq!(
        irq1.events(),
        vec![true],
        "SCI restore should not glitch low/high; it should assert once based on restored state"
    );
}

