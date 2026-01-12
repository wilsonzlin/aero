use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::{InterruptController, InterruptInput, PlatformInterruptMode};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;
use std::io::{Cursor, Read, Seek, SeekFrom};

use aero_devices::acpi_pm::{
    DEFAULT_ACPI_ENABLE, DEFAULT_PM1A_EVT_BLK, DEFAULT_PM_TMR_BLK, DEFAULT_SMI_CMD_PORT,
};
use aero_devices::pci::{
    GsiLevelSink, PciBarDefinition, PciBdf, PciConfigSpace, PciCoreSnapshot, PciDevice,
    PciInterruptPin, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};

fn pc_machine_config() -> MachineConfig {
    MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        // Keep the machine minimal for deterministic platform snapshot tests.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    }
}

fn program_ioapic_entry(
    ints: &mut aero_platform::interrupts::PlatformInterrupts,
    gsi: u32,
    low: u32,
    high: u32,
) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    ints.ioapic_mmio_write(0x00, redtbl_low);
    ints.ioapic_mmio_write(0x10, low);
    ints.ioapic_mmio_write(0x00, redtbl_high);
    ints.ioapic_mmio_write(0x10, high);
}

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_write(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_write(PCI_CFG_DATA_PORT + (offset & 3), size, value);
}

fn cfg_read(m: &mut Machine, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    m.io_read(PCI_CFG_DATA_PORT + (offset & 3), size)
}

fn reverse_devices_section(bytes: &[u8]) -> Vec<u8> {
    use aero_snapshot as snapshot;

    const FILE_HEADER_LEN: usize = 16;
    const SECTION_HEADER_LEN: usize = 16;

    let mut r = Cursor::new(bytes);
    let mut file_header = [0u8; FILE_HEADER_LEN];
    r.read_exact(&mut file_header).unwrap();

    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&file_header);

    while (r.position() as usize) < bytes.len() {
        let mut section_header = [0u8; SECTION_HEADER_LEN];
        // Valid snapshots end cleanly at EOF.
        if let Err(e) = r.read_exact(&mut section_header) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            panic!("failed to read section header: {e}");
        }

        let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
        let version = u16::from_le_bytes(section_header[4..6].try_into().unwrap());
        let flags = u16::from_le_bytes(section_header[6..8].try_into().unwrap());
        let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).unwrap();

        if id != snapshot::SectionId::DEVICES.0 {
            out.extend_from_slice(&section_header);
            out.extend_from_slice(&payload);
            continue;
        }

        let mut pr = Cursor::new(&payload);
        let mut count_bytes = [0u8; 4];
        pr.read_exact(&mut count_bytes).unwrap();
        let count = u32::from_le_bytes(count_bytes) as usize;

        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let mut dev_header = [0u8; 16];
            pr.read_exact(&mut dev_header).unwrap();
            let dev_len = u64::from_le_bytes(dev_header[8..16].try_into().unwrap());
            let mut dev_data = vec![0u8; dev_len as usize];
            pr.read_exact(&mut dev_data).unwrap();

            let mut entry = Vec::with_capacity(dev_header.len() + dev_data.len());
            entry.extend_from_slice(&dev_header);
            entry.extend_from_slice(&dev_data);
            entries.push(entry);
        }

        assert_eq!(
            pr.position() as usize,
            payload.len(),
            "devices section parse did not consume full payload"
        );

        entries.reverse();

        let mut new_payload = Vec::with_capacity(payload.len());
        let new_count: u32 = entries.len().try_into().unwrap();
        new_payload.extend_from_slice(&new_count.to_le_bytes());
        for entry in entries {
            new_payload.extend_from_slice(&entry);
        }
        let new_len: u64 = new_payload.len().try_into().unwrap();

        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&new_len.to_le_bytes());
        out.extend_from_slice(&new_payload);
    }

    out
}

#[derive(Default)]
struct RecordingSink {
    events: Vec<(u32, bool)>,
}

impl GsiLevelSink for RecordingSink {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.events.push((gsi, level));
    }
}

fn snapshot_devices(bytes: &[u8]) -> Vec<snapshot::DeviceState> {
    const FILE_HEADER_LEN: usize = 16;
    const SECTION_HEADER_LEN: usize = 16;

    let mut r = Cursor::new(bytes);
    let mut file_header = [0u8; FILE_HEADER_LEN];
    r.read_exact(&mut file_header).unwrap();

    while (r.position() as usize) < bytes.len() {
        let mut section_header = [0u8; SECTION_HEADER_LEN];
        // Valid snapshots end cleanly at EOF.
        if let Err(e) = r.read_exact(&mut section_header) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            panic!("failed to read section header: {e}");
        }

        let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
        let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).unwrap();

        if id != snapshot::SectionId::DEVICES.0 {
            continue;
        }

        let mut pr = Cursor::new(&payload);
        let mut count_bytes = [0u8; 4];
        pr.read_exact(&mut count_bytes).unwrap();
        let count = u32::from_le_bytes(count_bytes) as usize;

        let mut devices = Vec::with_capacity(count);
        for _ in 0..count {
            devices.push(snapshot::DeviceState::decode(&mut pr, 64 * 1024 * 1024).unwrap());
        }

        assert_eq!(
            pr.position() as usize,
            payload.len(),
            "devices section parse did not consume full payload"
        );

        return devices;
    }

    panic!("snapshot did not contain a DEVICES section");
}

fn rewrite_pci_cfg_device_id_to_legacy_pci(bytes: &[u8]) -> Vec<u8> {
    const FILE_HEADER_LEN: usize = 16;
    const SECTION_HEADER_LEN: usize = 16;

    let mut r = Cursor::new(bytes);
    let mut file_header = [0u8; FILE_HEADER_LEN];
    r.read_exact(&mut file_header).unwrap();

    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&file_header);

    while (r.position() as usize) < bytes.len() {
        let mut section_header = [0u8; SECTION_HEADER_LEN];
        // Valid snapshots end cleanly at EOF.
        if let Err(e) = r.read_exact(&mut section_header) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                break;
            }
            panic!("failed to read section header: {e}");
        }

        let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
        let version = u16::from_le_bytes(section_header[4..6].try_into().unwrap());
        let flags = u16::from_le_bytes(section_header[6..8].try_into().unwrap());
        let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

        let mut payload = vec![0u8; len as usize];
        r.read_exact(&mut payload).unwrap();

        if id != snapshot::SectionId::DEVICES.0 {
            out.extend_from_slice(&section_header);
            out.extend_from_slice(&payload);
            continue;
        }

        let mut pr = Cursor::new(&payload);
        let mut count_bytes = [0u8; 4];
        pr.read_exact(&mut count_bytes).unwrap();
        let count = u32::from_le_bytes(count_bytes) as usize;

        let mut entries = Vec::with_capacity(count);
        let mut rewritten = 0usize;
        for _ in 0..count {
            let mut dev_header = [0u8; 16];
            pr.read_exact(&mut dev_header).unwrap();
            let dev_len = u64::from_le_bytes(dev_header[8..16].try_into().unwrap());
            let mut dev_data = vec![0u8; dev_len as usize];
            pr.read_exact(&mut dev_data).unwrap();

            let dev_id = u32::from_le_bytes(dev_header[0..4].try_into().unwrap());
            if dev_id == snapshot::DeviceId::PCI_CFG.0 {
                dev_header[0..4].copy_from_slice(&snapshot::DeviceId::PCI.0.to_le_bytes());
                rewritten += 1;
            }

            let mut entry = Vec::with_capacity(dev_header.len() + dev_data.len());
            entry.extend_from_slice(&dev_header);
            entry.extend_from_slice(&dev_data);
            entries.push(entry);
        }

        assert_eq!(
            pr.position() as usize,
            payload.len(),
            "devices section parse did not consume full payload"
        );
        assert_eq!(
            rewritten, 1,
            "expected exactly one PCI_CFG entry to rewrite"
        );

        let mut new_payload = Vec::with_capacity(payload.len());
        let new_count: u32 = entries.len().try_into().unwrap();
        new_payload.extend_from_slice(&new_count.to_le_bytes());
        for entry in entries {
            new_payload.extend_from_slice(&entry);
        }
        let new_len: u64 = new_payload.len().try_into().unwrap();

        out.extend_from_slice(&id.to_le_bytes());
        out.extend_from_slice(&version.to_le_bytes());
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&new_len.to_le_bytes());
        out.extend_from_slice(&new_payload);
    }

    out
}

#[test]
fn snapshot_source_emits_pci_cfg_device_id_for_config_ports() {
    let m = Machine::new(pc_machine_config()).unwrap();
    let devices = snapshot::SnapshotSource::device_states(&m);

    assert!(
        devices.iter().any(|d| d.id == snapshot::DeviceId::PCI_CFG),
        "machine snapshot should include a PCI_CFG entry when pc platform is enabled"
    );
    assert!(
        devices
            .iter()
            .any(|d| d.id == snapshot::DeviceId::PCI_INTX_ROUTER),
        "machine snapshot should include a PCI_INTX_ROUTER entry when pc platform is enabled"
    );
    assert!(
        devices.iter().all(|d| d.id != snapshot::DeviceId::PCI),
        "machine snapshot should not use legacy PCI id for canonical PCI config ports state"
    );
}

#[test]
fn snapshot_source_emits_platform_interrupts_under_platform_interrupts_device_id() {
    let m = Machine::new(pc_machine_config()).unwrap();
    let devices = snapshot::SnapshotSource::device_states(&m);

    assert!(
        devices
            .iter()
            .any(|d| d.id == snapshot::DeviceId::PLATFORM_INTERRUPTS),
        "machine snapshot should include a PLATFORM_INTERRUPTS entry when pc platform is enabled"
    );

    // `DeviceId::APIC` is the historical id used by older snapshots; new snapshots should prefer
    // the dedicated `PLATFORM_INTERRUPTS` id.
    assert!(
        !devices.iter().any(|d| d.id == snapshot::DeviceId::APIC),
        "machine snapshot should not emit the legacy APIC DeviceId for platform interrupts"
    );
}

#[test]
fn snapshot_restore_preserves_acpi_pm_timer_and_it_advances_with_manual_clock() {
    let mut src = Machine::new(pc_machine_config()).unwrap();

    // PM_TMR should start at 0 at power-on with the shared ManualClock at 0.
    assert_eq!(src.io_read(DEFAULT_PM_TMR_BLK, 4), 0);

    // Advance 1s: PM timer frequency is 3.579545 MHz.
    src.tick_platform(1_000_000_000);
    assert_eq!(src.io_read(DEFAULT_PM_TMR_BLK, 4), 3_579_545);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Restoring rewinds time, so the timer should pick up exactly where it was snapshotted.
    assert_eq!(restored.io_read(DEFAULT_PM_TMR_BLK, 4), 3_579_545);

    // Advancing another second should deterministically add another 3_579_545 ticks.
    restored.tick_platform(1_000_000_000);
    assert_eq!(restored.io_read(DEFAULT_PM_TMR_BLK, 4), 7_159_090);
}

#[test]
fn snapshot_restore_preserves_rtc_periodic_irq8_pending_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state:
    // - master offset 0x20, slave offset 0x28
    // - unmask cascade + IRQ8
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(8, false);
    }

    // Enable RTC periodic interrupts (PIE=1) in Status Register B (0x0B).
    src.io_write(0x70, 1, 0x0B);
    src.io_write(0x71, 1, 0x42); // 24h + PIE

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Default reg A rate is 1024Hz, so 1ms is enough to trigger a periodic interrupt.
    src.tick_platform(1_000_000);

    // IRQ8 on the slave PIC with offset 0x28 => vector 0x28.
    assert_eq!(interrupts.borrow().get_pending(), Some(0x28));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x28));
}

#[test]
fn snapshot_restore_preserves_pit_phase_and_pulse_accounting() {
    let cfg = pc_machine_config();

    // Baseline: run without snapshot/restore.
    let mut baseline = Machine::new(cfg.clone()).unwrap();

    // Program PIT ch0: access lobyte/hibyte, mode 2 (rate generator), binary.
    baseline.io_write(PIT_CMD, 1, 0x34);
    baseline.io_write(PIT_CH0, 1, 100);
    baseline.io_write(PIT_CH0, 1, 0);

    let pit = baseline.pit().unwrap();
    pit.borrow_mut().advance_ticks(150);
    pit.borrow_mut().advance_ticks(50);
    assert_eq!(pit.borrow_mut().take_irq0_pulses(), 2);

    // Snapshot mid-period and continue after restore.
    let mut src = Machine::new(cfg.clone()).unwrap();
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, 100);
    src.io_write(PIT_CH0, 1, 0);
    let pit = src.pit().unwrap();
    pit.borrow_mut().advance_ticks(150);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();
    let pit = restored.pit().unwrap();
    pit.borrow_mut().advance_ticks(50);

    // The restored PIT includes pulses from before the snapshot (1) and after (1).
    assert_eq!(pit.borrow_mut().take_irq0_pulses(), 2);
}

#[test]
fn snapshot_restore_preserves_pit_irq0_pending_pic_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state and ensure IRQ0 is unmasked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(0, false);
    }

    // Program PIT ch0: access lobyte/hibyte, mode 2 (rate generator), binary. Divisor 100.
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, 100);
    src.io_write(PIT_CH0, 1, 0);

    assert_eq!(interrupts.borrow().get_pending(), None);

    // Advance exactly one period so we get a single edge-triggered IRQ0 pulse.
    let pit = src.pit().unwrap();
    pit.borrow_mut().advance_ticks(100);

    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x20));
}

#[test]
fn snapshot_restore_preserves_acpi_sci_pending_irq9_vector() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Put the PIC in a known state and ensure IRQ9 is unmasked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(2, false);
        ints.pic_mut().set_masked(9, false);
    }

    // Enable ACPI (sets SCI_EN).
    src.io_write(DEFAULT_SMI_CMD_PORT, 1, u32::from(DEFAULT_ACPI_ENABLE));
    // Enable the power button event bit in PM1_EN (bit 8).
    src.io_write(DEFAULT_PM1A_EVT_BLK + 2, 2, 1 << 8);

    // Trigger a power button event via the device model to set PM1_STS and assert SCI.
    src.acpi_pm().unwrap().borrow_mut().trigger_power_button();

    // IRQ9 on the slave PIC with offset 0x28 => vector 0x29.
    assert_eq!(interrupts.borrow().get_pending(), Some(0x29));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x29));
}

#[test]
fn snapshot_restore_pci_config_bar_programming_survives() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(PciBdf::new(0, 1, 0), Box::new(TestDev { cfg }));

    // Program BAR0 via the standard PCI config mechanism #1 ports.
    let bar0_addr = 0x8000_0000u32 | (1u32 << 11) | 0x10;
    src.io_write(PCI_CFG_ADDR_PORT, 4, bar0_addr);
    src.io_write(PCI_CFG_DATA_PORT, 4, 0x8000_0000);

    src.io_write(PCI_CFG_ADDR_PORT, 4, bar0_addr);
    assert_eq!(src.io_read(PCI_CFG_DATA_PORT, 4), 0x8000_0000);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = restored.pci_config_ports().unwrap();

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(PciBdf::new(0, 1, 0), Box::new(TestDev { cfg }));

    restored.restore_snapshot_bytes(&snap).unwrap();

    restored.io_write(PCI_CFG_ADDR_PORT, 4, bar0_addr);
    assert_eq!(restored.io_read(PCI_CFG_DATA_PORT, 4), 0x8000_0000);
}

#[test]
fn restore_device_states_accepts_legacy_pci_device_id_for_pci_cfg_state() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let bdf = PciBdf::new(0, 1, 0);
    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg }));

    // Program BAR0 base and enable decode + bus mastering + INTx disable.
    cfg_write(&mut src, bdf, 0x10, 4, 0x8000_0000);
    let command: u16 = 0x0007 | (1 << 10); // IO + MEM + BME + INTX_DISABLE
    cfg_write(&mut src, bdf, 0x04, 2, u32::from(command));

    let legacy_state = {
        let pci_cfg = pci_cfg.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI,
            &*pci_cfg,
        )
    };

    // Restore into a fresh machine with a different guest-programmed state.
    let mut restored = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = restored.pci_config_ports().expect("pc platform enabled");

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg }));

    cfg_write(&mut restored, bdf, 0x10, 4, 0x9000_0000);
    cfg_write(&mut restored, bdf, 0x04, 2, 0);
    assert_eq!(cfg_read(&mut restored, bdf, 0x10, 4), 0x9000_0000);
    assert_eq!(cfg_read(&mut restored, bdf, 0x04, 2) as u16, 0);

    snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![legacy_state]);

    assert_eq!(cfg_read(&mut restored, bdf, 0x10, 4), 0x8000_0000);
    assert_eq!(cfg_read(&mut restored, bdf, 0x04, 2) as u16, command);
}

#[test]
fn restore_device_states_prefers_pci_cfg_over_legacy_pci_entry() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let bdf = PciBdf::new(0, 1, 0);
    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg }));

    cfg_write(&mut src, bdf, 0x10, 4, 0x8000_0000);
    let canonical_state = {
        let pci_cfg = pci_cfg.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI_CFG,
            &*pci_cfg,
        )
    };

    // Legacy state: BAR0 = 0x9000_0000 stored under the historical `PCI` wrapper snapshot.
    cfg_write(&mut src, bdf, 0x10, 4, 0x9000_0000);
    let legacy_state = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let mut pci_intx = pci_intx.borrow_mut();
        let core = PciCoreSnapshot::new(&mut pci_cfg, &mut pci_intx);
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(snapshot::DeviceId::PCI, &core)
    };

    // Restore into a fresh machine and ensure the canonical state wins even if the legacy entry
    // appears first.
    let mut restored = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = restored.pci_config_ports().expect("pc platform enabled");

    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg }));

    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![legacy_state, canonical_state],
    );

    assert_eq!(cfg_read(&mut restored, bdf, 0x10, 4), 0x8000_0000);
}

#[test]
fn restore_device_states_falls_back_to_legacy_pci_cfg_when_pci_cfg_snapshot_is_invalid() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");

    // Put the PCI config ports into a non-default state (the 0xCF8 address latch is part of the
    // `PciConfigMechanism1` snapshot).
    let latch = cfg_addr(PciBdf::new(0, 1, 0), 0x10);
    src.io_write(PCI_CFG_ADDR_PORT, 4, latch);
    assert_eq!(src.io_read(PCI_CFG_ADDR_PORT, 4), latch);

    // Provide a valid legacy `DeviceId::PCI` (`PCPT`) payload.
    let legacy_state = {
        let pci_cfg = pci_cfg.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI,
            &*pci_cfg,
        )
    };

    // Provide an invalid canonical `PCI_CFG` payload by corrupting the outer version.
    let mut bad_canonical_state = {
        let pci_cfg = pci_cfg.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI_CFG,
            &*pci_cfg,
        )
    };
    bad_canonical_state.version = bad_canonical_state.version.wrapping_add(1);

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![legacy_state, bad_canonical_state],
    );

    // Restore should fall back to the legacy `DeviceId::PCI` payload when `PCI_CFG` is invalid.
    assert_eq!(restored.io_read(PCI_CFG_ADDR_PORT, 4), latch);
}

#[test]
fn restore_device_states_prefers_pci_intx_router_over_legacy_pci_entry() {
    let src = Machine::new(pc_machine_config()).unwrap();
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");

    let bdf = PciBdf::new(0, 1, 0);

    // Canonical state: assert only INTA#.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(bdf, PciInterruptPin::IntA, &mut sink);
    }
    let expected_canonical_events = {
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };
    let canonical_state = {
        let pci_intx = pci_intx.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI_INTX_ROUTER,
            &*pci_intx,
        )
    };

    // Legacy state: additionally assert INTB#. This should be ignored if the dedicated
    // `PCI_INTX_ROUTER` entry is present.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(bdf, PciInterruptPin::IntB, &mut sink);
    }
    let legacy_state = {
        let pci_intx = pci_intx.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI,
            &*pci_intx,
        )
    };

    // Sanity check: the two snapshots should encode distinct asserted GSI sets.
    let expected_legacy_events = {
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };
    assert_ne!(expected_canonical_events, expected_legacy_events);

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![legacy_state, canonical_state],
    );

    let restored_events = {
        let pci_intx = restored.pci_intx_router().expect("pc platform enabled");
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };
    assert_eq!(restored_events, expected_canonical_events);
}

#[test]
fn restore_device_states_falls_back_to_legacy_pci_intx_when_pci_intx_router_snapshot_is_invalid() {
    let src = Machine::new(pc_machine_config()).unwrap();
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");

    let bdf = PciBdf::new(0, 1, 0);

    // Canonical INTx state: assert only INTA#.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(bdf, PciInterruptPin::IntA, &mut sink);
    }
    let mut bad_canonical_state = {
        let pci_intx = pci_intx.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI_INTX_ROUTER,
            &*pci_intx,
        )
    };
    // Corrupt the outer version so `apply_io_snapshot_to_device` rejects it.
    bad_canonical_state.version = bad_canonical_state.version.wrapping_add(1);

    // Legacy INTx state: additionally assert INTB#.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(bdf, PciInterruptPin::IntB, &mut sink);
    }
    let expected_legacy_events = {
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };
    let legacy_state = {
        let pci_intx = pci_intx.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI,
            &*pci_intx,
        )
    };

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![legacy_state, bad_canonical_state],
    );

    let restored_events = {
        let pci_intx = restored.pci_intx_router().expect("pc platform enabled");
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };
    assert_eq!(restored_events, expected_legacy_events);
}

#[test]
fn restore_device_states_falls_back_to_legacy_apic_when_platform_interrupts_snapshot_is_invalid() {
    let src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // Configure APIC routing but keep the entry masked so a pending interrupt only appears after
    // we later unmask it.
    let vector = 0x53u32;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        // Active-low, level-triggered, masked.
        let low = vector | (1 << 13) | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut ints, 10, low, 0);
        ints.raise_irq(InterruptInput::Gsi(10));
        assert_eq!(ints.get_pending(), None);
    }

    // Provide a valid legacy APIC device blob.
    let apic_state = snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
        snapshot::DeviceId::APIC,
        &*interrupts.borrow(),
    );

    // Provide an invalid `PLATFORM_INTERRUPTS` blob by corrupting the outer version.
    let mut bad_platform_state = snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
        snapshot::DeviceId::PLATFORM_INTERRUPTS,
        &*interrupts.borrow(),
    );
    bad_platform_state.version = bad_platform_state.version.wrapping_add(1);

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![bad_platform_state, apic_state],
    );

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If restore fell back to the legacy APIC payload, the restored
    // asserted level should deliver immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = vector | (1 << 13) | (1 << 15); // active-low, level-triggered, unmasked
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
}

#[test]
fn restore_device_states_does_not_sync_pci_intx_when_intx_snapshot_is_invalid() {
    let src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let pci_intx = src.pci_intx_router().unwrap();

    // Configure APIC routing but keep the entry masked so a pending interrupt only appears after
    // we later unmask it.
    let vector = 0x53u32;
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        // Active-low, level-triggered, masked.
        let low = vector | (1 << 13) | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut ints, 10, low, 0);
        ints.raise_irq(InterruptInput::Gsi(10));
        assert_eq!(ints.get_pending(), None);
    }

    // Snapshot the interrupt controller with GSI10 asserted.
    let apic_state = snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
        snapshot::DeviceId::APIC,
        &*interrupts.borrow(),
    );

    // Produce an invalid PCI_INTX_ROUTER state by corrupting the outer version (must match the inner
    // io-snapshot header version). This forces `apply_io_snapshot_to_device` to fail.
    let mut bad_intx_state = snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
        snapshot::DeviceId::PCI_INTX_ROUTER,
        &*pci_intx.borrow(),
    );
    bad_intx_state.version = bad_intx_state.version.wrapping_add(1);

    // Restore into a fresh machine. The invalid PCI_INTX_ROUTER state must *not* cause Machine restore to
    // call `PciIntxRouter::sync_levels_to_sink()`, since the router's state did not apply.
    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(
        &mut restored,
        vec![apic_state, bad_intx_state],
    );

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If the restored asserted level survived and restore did not
    // spuriously deassert it via an INTx sync, unmasking should deliver immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = vector | (1 << 13) | (1 << 15); // active-low, level-triggered, unmasked
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(vector as u8));
}

#[test]
fn restore_device_states_accepts_legacy_pci_device_id_for_pci_intx_state() {
    let src = Machine::new(pc_machine_config()).unwrap();
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");

    // Put the INTx router into a non-default state by asserting an INTx pin.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntA, &mut sink);
    }

    let expected_events = {
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };

    // Legacy layout: store a `PciIntxRouter` io-snapshot blob (inner `INTX`) under the historical
    // outer `DeviceId::PCI` key.
    let legacy_state = {
        let pci_intx = pci_intx.borrow();
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
            snapshot::DeviceId::PCI,
            &*pci_intx,
        )
    };

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![legacy_state]);

    let restored_events = {
        let pci_intx = restored.pci_intx_router().expect("pc platform enabled");
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };

    assert_eq!(restored_events, expected_events);
}

#[test]
fn restore_device_states_accepts_legacy_pci_device_id_for_combined_pci_core_snapshot() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");
    let pci_intx = src.pci_intx_router().expect("pc platform enabled");

    // Put the PCI config ports into a non-default state (the 0xCF8 address latch is part of the
    // PCI config mechanism snapshot).
    let addr = cfg_addr(PciBdf::new(0, 1, 0), 0x10);
    src.io_write(PCI_CFG_ADDR_PORT, 4, addr);
    assert_eq!(src.io_read(PCI_CFG_ADDR_PORT, 4), addr);

    // Put the INTx router into a non-default state too.
    {
        let mut pci_intx = pci_intx.borrow_mut();
        let mut sink = RecordingSink::default();
        pci_intx.assert_intx(PciBdf::new(0, 1, 0), PciInterruptPin::IntA, &mut sink);
    }

    let expected_intx_events = {
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };

    let core_state = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let mut pci_intx = pci_intx.borrow_mut();
        let core = PciCoreSnapshot::new(&mut pci_cfg, &mut pci_intx);
        snapshot::io_snapshot_bridge::device_state_from_io_snapshot(snapshot::DeviceId::PCI, &core)
    };

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![core_state]);

    assert_eq!(restored.io_read(PCI_CFG_ADDR_PORT, 4), addr);

    let restored_intx_events = {
        let pci_intx = restored.pci_intx_router().expect("pc platform enabled");
        let pci_intx = pci_intx.borrow();
        let mut sink = RecordingSink::default();
        pci_intx.sync_levels_to_sink(&mut sink);
        sink.events
    };

    assert_eq!(restored_intx_events, expected_intx_events);
}

#[test]
fn snapshot_uses_pci_cfg_device_id_and_restore_accepts_legacy_device_id() {
    let cfg = pc_machine_config();

    let mut src = Machine::new(cfg.clone()).unwrap();
    let pci_cfg = src.pci_config_ports().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let bdf = PciBdf::new(0, 1, 0);
    let mut cfg_space = PciConfigSpace::new(0x1234, 0x5678);
    cfg_space.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg: cfg_space }));

    // Program BAR0 via the standard PCI config mechanism #1 ports.
    cfg_write(&mut src, bdf, 0x10, 4, 0x8000_0000);
    assert_eq!(cfg_read(&mut src, bdf, 0x10, 4), 0x8000_0000);

    let snap = src.take_snapshot_full().unwrap();

    // New snapshots should use the canonical `DeviceId::PCI_CFG` outer id for `PciConfigPorts`.
    let devices = snapshot_devices(&snap);
    assert!(
        devices.iter().any(|d| d.id == snapshot::DeviceId::PCI_CFG),
        "snapshot DEVICES section missing PCI_CFG entry"
    );
    assert!(
        devices.iter().all(|d| d.id != snapshot::DeviceId::PCI),
        "snapshot DEVICES section should not use legacy PCI id for config ports"
    );

    // Legacy snapshots used `DeviceId::PCI` for `PciConfigPorts`; restore should remain compatible.
    let legacy_snap = rewrite_pci_cfg_device_id_to_legacy_pci(&snap);
    let legacy_devices = snapshot_devices(&legacy_snap);
    assert!(
        legacy_devices
            .iter()
            .any(|d| d.id == snapshot::DeviceId::PCI),
        "rewritten snapshot missing legacy PCI entry"
    );
    assert!(
        legacy_devices
            .iter()
            .all(|d| d.id != snapshot::DeviceId::PCI_CFG),
        "rewritten snapshot should no longer contain PCI_CFG entry"
    );

    let mut restored = Machine::new(cfg).unwrap();
    let pci_cfg = restored.pci_config_ports().expect("pc platform enabled");

    let mut cfg_space = PciConfigSpace::new(0x1234, 0x5678);
    cfg_space.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg: cfg_space }));

    restored.restore_snapshot_bytes(&legacy_snap).unwrap();
    assert_eq!(cfg_read(&mut restored, bdf, 0x10, 4), 0x8000_0000);
}

#[test]
fn snapshot_restore_preserves_pci_command_bits_and_pic_pending_interrupt() {
    let mut vm = Machine::new(pc_machine_config()).unwrap();
    let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
    let interrupts = vm.platform_interrupts().expect("pc platform enabled");

    struct TestDev {
        cfg: PciConfigSpace,
    }

    impl PciDevice for TestDev {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    // Install a simple endpoint at 00:01.0 with one MMIO BAR so we can validate both:
    // - guest-programmed BAR base
    // - PCI command register bits (IO/MEM/BME/INTX_DISABLE).
    let bdf = PciBdf::new(0, 1, 0);
    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestDev { cfg }));

    // Program BAR0 base and enable decode + bus mastering + INTx disable.
    cfg_write(&mut vm, bdf, 0x10, 4, 0x8000_0000);
    let command: u16 = 0x0007 | (1 << 10); // IO + MEM + BME + INTX_DISABLE
    cfg_write(&mut vm, bdf, 0x04, 2, u32::from(command));

    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x8000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2) as u16, command);

    // Raise a PIC interrupt (IRQ1 => vector 0x21 after setting offsets).
    let vector = 0x21u8;
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.pic_mut().set_masked(1, false);
        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(vector));
    }

    let snap = vm.take_snapshot_full().unwrap();

    // Mutate the PCI config and clear the interrupt so restore is an observable rewind.
    cfg_write(&mut vm, bdf, 0x10, 4, 0x9000_0000);
    cfg_write(&mut vm, bdf, 0x04, 2, 0);
    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(vector);
        ints.lower_irq(InterruptInput::IsaIrq(1));
        ints.eoi(vector);
        assert_eq!(ints.get_pending(), None);
    }
    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x9000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2), 0);

    vm.restore_snapshot_bytes(&snap).unwrap();

    // PCI config restored.
    assert_eq!(cfg_read(&mut vm, bdf, 0x10, 4), 0x8000_0000);
    assert_eq!(cfg_read(&mut vm, bdf, 0x04, 2) as u16, command);

    // Interrupt restored.
    assert_eq!(interrupts.borrow().get_pending(), Some(vector));
}

#[test]
fn snapshot_restore_syncs_pci_intx_levels_into_interrupt_controller() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let pci_intx = src.pci_intx_router().unwrap();

    // Route IOAPIC GSI10 -> vector 0x51, active-low, level-triggered, masked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = 0x51u32 | (1 << 13) | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    // Assert a PCI INTx line that routes to GSI10 (device 0, INTA#).
    {
        let mut ints = interrupts.borrow_mut();
        pci_intx
            .borrow_mut()
            .assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut *ints);
    }

    // Corrupt the sink state to simulate a snapshot taken at an inconsistent point: the router
    // thinks the line is asserted, but the platform interrupt controller has it deasserted.
    interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(10));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If Machine restore called `PciIntxRouter::sync_levels_to_sink`,
    // the asserted GSI level is re-driven and unmasking delivers immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = 0x51u32 | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(0x51));
}

#[test]
fn snapshot_restore_is_independent_of_devices_section_order() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let pci_intx = src.pci_intx_router().unwrap();

    // Route IOAPIC GSI10 -> vector 0x52, active-low, level-triggered, masked.
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = 0x52u32 | (1 << 13) | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    // Assert a PCI INTx line that routes to GSI10 (device 0, INTA#).
    {
        let mut ints = interrupts.borrow_mut();
        pci_intx
            .borrow_mut()
            .assert_intx(PciBdf::new(0, 0, 0), PciInterruptPin::IntA, &mut *ints);
    }

    // Corrupt the sink state to ensure restore must call `sync_levels_to_sink` even if
    // snapshot device ordering changes.
    interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(10));

    let snap = src.take_snapshot_full().unwrap();
    let snap = reverse_devices_section(&snap);

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If restore is order-independent and the sync fixup ran, unmasking
    // delivers immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = 0x52u32 | (1 << 13) | (1 << 15);
        program_ioapic_entry(&mut ints, 10, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(0x52));
}

#[test]
fn snapshot_restore_preserves_lapic_timer_state() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();

    // APIC mode required for LAPIC delivery.
    interrupts
        .borrow_mut()
        .set_mode(PlatformInterruptMode::Apic);

    // Program LAPIC timer (one-shot, vector 0x40, initial count 10).
    interrupts
        .borrow()
        .lapic_mmio_write(0x3E0, &0xBu32.to_le_bytes()); // Divide config
    interrupts
        .borrow()
        .lapic_mmio_write(0x320, &0x40u32.to_le_bytes()); // LVT Timer
    interrupts
        .borrow()
        .lapic_mmio_write(0x380, &10u32.to_le_bytes()); // Initial count

    interrupts.borrow().tick(9);
    assert_eq!(interrupts.borrow().get_pending(), None);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    interrupts.borrow().tick(1);
    assert_eq!(interrupts.borrow().get_pending(), Some(0x40));
}

#[test]
fn snapshot_restore_syncs_hpet_level_lines() {
    let mut src = Machine::new(pc_machine_config()).unwrap();
    let interrupts = src.platform_interrupts().unwrap();
    let hpet = src.hpet().unwrap();

    // Route GSI2 -> vector 0x61, level-triggered, masked. (GSI2 is active-high in our board wiring.)
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        let low = 0x61u32 | (1 << 15) | (1 << 16);
        program_ioapic_entry(&mut ints, 2, low, 0);
    }

    // Configure HPET timer0 for a level-triggered interrupt and arm it such that it becomes
    // pending immediately once HPET is enabled.
    {
        let mut ints = interrupts.borrow_mut();
        let mut hpet = hpet.borrow_mut();

        // Timer0 config: route=2, level-triggered, interrupt enabled.
        let timer0_cfg = (2u64 << 9) | (1 << 1) | (1 << 2);
        hpet.mmio_write(0x100, 8, timer0_cfg, &mut *ints);

        // Arm comparator at 0 so it's immediately pending once enabled.
        hpet.mmio_write(0x108, 8, 0, &mut *ints);

        // Enable HPET.
        hpet.mmio_write(0x010, 8, 1, &mut *ints);

        let status = hpet.mmio_read(0x020, 8, &mut *ints);
        assert_ne!(status & 1, 0, "timer0 interrupt status must be set");
    }

    // Corrupt the sink state: HPET has an interrupt pending (general_int_status bit set), but the
    // platform interrupt controller line is deasserted. HPET `irq_asserted` is not snapshotted, so
    // restore must explicitly re-drive the level line into the interrupt sink.
    interrupts.borrow_mut().lower_irq(InterruptInput::Gsi(2));

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(pc_machine_config()).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    let interrupts = restored.platform_interrupts().unwrap();
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Unmask the IOAPIC entry. If Machine restore re-drove HPET's level lines after state restore,
    // the asserted level is present and unmasking delivers immediately.
    {
        let mut ints = interrupts.borrow_mut();
        let low = 0x61u32 | (1 << 15); // unmasked
        program_ioapic_entry(&mut ints, 2, low, 0);
    }

    assert_eq!(interrupts.borrow().get_pending(), Some(0x61));
}

fn read_u32_le<R: Read>(r: &mut R) -> u32 {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

fn read_u16_le<R: Read>(r: &mut R) -> u16 {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).unwrap();
    u16::from_le_bytes(buf)
}

fn read_u64_le<R: Read>(r: &mut R) -> u64 {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).unwrap();
    u64::from_le_bytes(buf)
}

fn skip_exact<R: Read>(r: &mut R, mut len: u64) {
    let mut buf = [0u8; 1024];
    while len > 0 {
        let chunk = (len as usize).min(buf.len());
        r.read_exact(&mut buf[..chunk]).unwrap();
        len -= chunk as u64;
    }
}

#[test]
fn snapshot_stores_pci_core_under_split_device_ids() {
    let mut m = Machine::new(pc_machine_config()).unwrap();
    assert!(m.pci_config_ports().is_some());
    assert!(m.pci_intx_router().is_some());

    let snap = m.take_snapshot_full().unwrap();

    // Find the DEVICES section and scan the entry headers without allocating device payloads.
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snap)).unwrap();
    let devices_section = index
        .sections
        .iter()
        .find(|s| s.id == aero_snapshot::SectionId::DEVICES)
        .expect("missing DEVICES section");

    let mut cursor = Cursor::new(&snap);
    cursor
        .seek(SeekFrom::Start(devices_section.offset))
        .unwrap();
    let mut r = cursor.take(devices_section.len);

    let count = read_u32_le(&mut r) as usize;
    let mut pci_cfg_entries = 0usize;
    let mut pci_intx_entries = 0usize;
    let mut pci_entries = 0usize;

    for _ in 0..count {
        let id = aero_snapshot::DeviceId(read_u32_le(&mut r));
        let _version = read_u16_le(&mut r);
        let _flags = read_u16_le(&mut r);
        let len = read_u64_le(&mut r);

        match id {
            aero_snapshot::DeviceId::PCI_CFG => {
                pci_cfg_entries += 1;
                // `aero-io-snapshot` header is 16 bytes. Verify `PciConfigPorts` (`PCPT`).
                let mut hdr = [0u8; 16];
                r.read_exact(&mut hdr).unwrap();
                assert_eq!(&hdr[0..4], b"AERO");
                assert_eq!(&hdr[8..12], b"PCPT");
                skip_exact(&mut r, len.saturating_sub(hdr.len() as u64));
            }
            aero_snapshot::DeviceId::PCI_INTX_ROUTER => {
                pci_intx_entries += 1;
                // Verify `PciIntxRouter` (`INTX`).
                let mut hdr = [0u8; 16];
                r.read_exact(&mut hdr).unwrap();
                assert_eq!(&hdr[0..4], b"AERO");
                assert_eq!(&hdr[8..12], b"INTX");
                skip_exact(&mut r, len.saturating_sub(hdr.len() as u64));
            }
            aero_snapshot::DeviceId::PCI => {
                // Legacy combined PCI snapshots may exist, but canonical machine snapshots
                // should not emit them.
                pci_entries += 1;
                skip_exact(&mut r, len);
            }
            _ => skip_exact(&mut r, len),
        }
    }

    assert_eq!(pci_cfg_entries, 1);
    assert_eq!(pci_intx_entries, 1);
    assert_eq!(pci_entries, 0);
}
