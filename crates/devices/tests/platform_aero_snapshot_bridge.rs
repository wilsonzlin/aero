use std::io::Cursor;

use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN};
use aero_devices::clock::ManualClock;
use aero_devices::hpet::Hpet;
use aero_devices::ioapic::{GsiEvent, IoApic};
use aero_devices::irq::IrqLine;
use aero_platform::io::PortIoDevice;
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotMeta, SnapshotSource, SnapshotTarget,
};

const HPET_REG_GENERAL_CONFIG: u64 = 0x010;
const HPET_REG_GENERAL_INT_STATUS: u64 = 0x020;
const HPET_REG_TIMER0_BASE: u64 = 0x100;
const HPET_REG_TIMER_CONFIG: u64 = 0x00;
const HPET_REG_TIMER_COMPARATOR: u64 = 0x08;

const HPET_GEN_CONF_ENABLE: u64 = 1 << 0;
const HPET_TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const HPET_TIMER_CFG_INT_ENABLE: u64 = 1 << 2;

#[derive(Clone, Default)]
struct TestIrq(std::rc::Rc<std::cell::Cell<bool>>);

impl TestIrq {
    fn level(&self) -> bool {
        self.0.get()
    }
}

impl IrqLine for TestIrq {
    fn set_level(&self, level: bool) {
        self.0.set(level);
    }
}

struct BridgeSource {
    meta: SnapshotMeta,
    devices: Vec<DeviceState>,
    ram: Vec<u8>,
}

impl SnapshotSource for BridgeSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Keep meta deterministic so snapshot bytes are deterministic for this test.
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        self.devices.clone()
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
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct BridgeTarget {
    devices: Vec<DeviceState>,
    ram: Vec<u8>,
}

impl SnapshotTarget for BridgeTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        self.devices = states;
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

fn save_bytes(source: &mut BridgeSource) -> Vec<u8> {
    let mut options = SaveOptions::default();
    options.ram.compression = Compression::None;
    options.ram.chunk_size = 4096;

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, options).unwrap();
    cursor.into_inner()
}

#[test]
fn acpi_pm_io_snapshot_roundtrips_through_aero_snapshot_file() {
    let clock = ManualClock::new();
    clock.advance_ns(1_000_000);

    let irq0 = TestIrq::default();

    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        AcpiPmConfig::default(),
        AcpiPmCallbacks {
            sci_irq: Box::new(irq0.clone()),
            request_power_off: None,
        },
        clock.clone(),
    );

    // Enable ACPI (SCI_EN) via SMI_CMD handshake.
    pm.write(AcpiPmConfig::default().smi_cmd_port, 1, 0xA0);
    // Enable the power-button status bit and trigger it so SCI is asserted.
    pm.write(
        AcpiPmConfig::default().pm1a_evt_blk + 2,
        2,
        PM1_STS_PWRBTN as u32,
    );
    pm.trigger_pm1_event(PM1_STS_PWRBTN);
    assert!(irq0.level(), "SCI should be asserted before snapshot");

    let expected_pm1_cnt = pm.pm1_cnt();
    let expected_pm1_sts = pm.pm1_status();
    let expected_pm1_en = pm.read(AcpiPmConfig::default().pm1a_evt_blk + 2, 2) as u16;

    let device_state = device_state_from_io_snapshot(DeviceId::ACPI_PM, &pm);

    let mut source = BridgeSource {
        meta: SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        },
        devices: vec![device_state],
        ram: vec![0u8; 4096],
    };

    let snap1 = save_bytes(&mut source);
    let snap2 = save_bytes(&mut source);
    assert_eq!(snap1, snap2, "snapshot bytes must be deterministic");

    let mut target = BridgeTarget {
        devices: Vec::new(),
        ram: vec![0u8; 4096],
    };
    restore_snapshot(&mut Cursor::new(&snap1), &mut target).unwrap();

    let irq1 = TestIrq::default();
    let mut restored_pm = AcpiPmIo::new_with_callbacks_and_clock(
        AcpiPmConfig::default(),
        AcpiPmCallbacks {
            sci_irq: Box::new(irq1.clone()),
            request_power_off: None,
        },
        clock.clone(),
    );

    for state in &target.devices {
        if state.id == DeviceId::ACPI_PM {
            apply_io_snapshot_to_device(state, &mut restored_pm).unwrap();
        }
    }

    assert_eq!(restored_pm.pm1_cnt(), expected_pm1_cnt);
    assert_eq!(restored_pm.pm1_status(), expected_pm1_sts);
    assert_eq!(
        restored_pm.read(AcpiPmConfig::default().pm1a_evt_blk + 2, 2) as u16,
        expected_pm1_en
    );
    assert!(
        irq1.level(),
        "restored ACPI PM should re-drive SCI based on restored PM1 state"
    );
}

#[test]
fn hpet_io_snapshot_roundtrips_through_aero_snapshot_file() {
    let clock = ManualClock::new();
    let mut ioapic0 = IoApic::default();
    let mut hpet = Hpet::new_default(clock.clone());

    hpet.mmio_write(
        HPET_REG_GENERAL_CONFIG,
        8,
        HPET_GEN_CONF_ENABLE,
        &mut ioapic0,
    );
    let timer0_cfg = hpet.mmio_read(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        &mut ioapic0,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_CONFIG,
        8,
        timer0_cfg | HPET_TIMER_CFG_INT_ENABLE | HPET_TIMER_CFG_INT_LEVEL,
        &mut ioapic0,
    );
    hpet.mmio_write(
        HPET_REG_TIMER0_BASE + HPET_REG_TIMER_COMPARATOR,
        8,
        1,
        &mut ioapic0,
    );

    clock.advance_ns(100);
    hpet.poll(&mut ioapic0);
    assert!(ioapic0.is_asserted(2));
    ioapic0.take_events();

    let device_state = device_state_from_io_snapshot(DeviceId::HPET, &hpet);

    let mut source = BridgeSource {
        meta: SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        },
        devices: vec![device_state],
        ram: vec![0u8; 4096],
    };

    let snap1 = save_bytes(&mut source);
    let snap2 = save_bytes(&mut source);
    assert_eq!(snap1, snap2, "snapshot bytes must be deterministic");

    let mut target = BridgeTarget {
        devices: Vec::new(),
        ram: vec![0u8; 4096],
    };
    restore_snapshot(&mut Cursor::new(&snap1), &mut target).unwrap();

    let mut ioapic1 = IoApic::default();
    let mut restored_hpet = Hpet::new_default(clock.clone());

    for state in &target.devices {
        if state.id == DeviceId::HPET {
            apply_io_snapshot_to_device(state, &mut restored_hpet).unwrap();
        }
    }

    restored_hpet.sync_levels_to_sink(&mut ioapic1);
    assert!(
        ioapic1.is_asserted(2),
        "level-triggered interrupt should be reasserted based on general_int_status"
    );
    assert_eq!(ioapic1.take_events(), vec![GsiEvent::Raise(2)]);

    restored_hpet.mmio_write(HPET_REG_GENERAL_INT_STATUS, 8, 1, &mut ioapic1);
    assert!(!ioapic1.is_asserted(2));
    assert_eq!(ioapic1.take_events(), vec![GsiEvent::Lower(2)]);
}
