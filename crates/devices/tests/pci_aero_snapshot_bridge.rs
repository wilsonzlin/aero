use std::io::Cursor;
use std::cell::RefCell;

use aero_devices::pci::{
    GsiLevelSink, MsiCapability, PciBarDefinition, PciBdf, PciBus, PciConfigPorts, PciConfigSpace,
    PciCoreSnapshot, PciDevice, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
};
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotMeta, SnapshotSource, SnapshotTarget,
};

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn cfg_read(ports: &mut PciConfigPorts, bdf: PciBdf, offset: u16, size: u8) -> u32 {
    ports.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    ports.io_read(0xCFC + (offset & 3), size)
}

fn cfg_write(ports: &mut PciConfigPorts, bdf: PciBdf, offset: u16, size: u8, value: u32) {
    ports.io_write(0xCF8, 4, cfg_addr(bdf, offset));
    ports.io_write(0xCFC + (offset & 3), size, value);
}

struct TestDevice {
    cfg: PciConfigSpace,
}

impl TestDevice {
    fn new() -> Self {
        let mut cfg = PciConfigSpace::new(0x1234, 0x0001);
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: 0x1000,
                prefetchable: false,
            },
        );
        cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
        cfg.add_capability(Box::new(MsiCapability::new()));
        Self { cfg }
    }
}

impl PciDevice for TestDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

fn make_bus() -> (PciBus, PciBdf) {
    let mut bus = PciBus::new();
    let bdf = PciBdf::new(0, 1, 0);
    bus.add_device(bdf, Box::new(TestDevice::new()));
    (bus, bdf)
}

#[derive(Default)]
struct MockSink {
    events: Vec<(u32, bool)>,
}

impl GsiLevelSink for MockSink {
    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        self.events.push((gsi, level));
    }
}

struct TestSource {
    meta: SnapshotMeta,
    pci_cfg: RefCell<PciConfigPorts>,
    pci_intx: RefCell<PciIntxRouter>,
    ram: Vec<u8>,
}

impl SnapshotSource for TestSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Keep meta deterministic so save_snapshot output is stable for this test.
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let mut pci_intx = self.pci_intx.borrow_mut();
        let core = PciCoreSnapshot::new(&mut *pci_cfg, &mut *pci_intx);
        vec![device_state_from_io_snapshot(DeviceId::PCI, &core)]
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

struct TestTarget {
    pci_cfg: PciConfigPorts,
    pci_intx: PciIntxRouter,
    ram: Vec<u8>,
}

impl SnapshotTarget for TestTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::PCI {
                let mut core = PciCoreSnapshot::new(&mut self.pci_cfg, &mut self.pci_intx);
                apply_io_snapshot_to_device(&state, &mut core).unwrap();
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
fn pci_io_snapshot_roundtrips_through_aero_snapshot_file() {
    let (bus, bdf) = make_bus();
    let mut pci_cfg = PciConfigPorts::with_bus(bus);

    // BAR probe/program + decode enable.
    cfg_write(&mut pci_cfg, bdf, 0x10, 4, 0xFFFF_FFFF);
    assert_eq!(cfg_read(&mut pci_cfg, bdf, 0x10, 4), 0xFFFF_F000);
    cfg_write(&mut pci_cfg, bdf, 0x10, 4, 0x1234_5000);
    cfg_write(&mut pci_cfg, bdf, 0x14, 4, 0x0000_C200);
    cfg_write(&mut pci_cfg, bdf, 0x04, 2, 0x0003);

    // Leave the config address latch on an arbitrary register so we can validate it restores.
    pci_cfg.io_write(0xCF8, 4, cfg_addr(bdf, 0x3c));
    let expected_latch = pci_cfg.io_read(0xCF8, 4);

    let expected_cfg: Vec<u32> = (0..256u16)
        .step_by(4)
        .map(|offset| pci_cfg.bus_mut().read_config(bdf, offset, 4))
        .collect();
    let expected_mapped = pci_cfg.bus_mut().mapped_bars();

    // INTx router: assert one line so snapshot contains a non-empty routing level state.
    let mut pci_intx = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut sink = MockSink::default();
    pci_intx.assert_intx(bdf, PciInterruptPin::IntA, &mut sink);

    let mut expected_intx_levels = MockSink::default();
    pci_intx.sync_levels_to_sink(&mut expected_intx_levels);

    let mut source = TestSource {
        meta: SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        },
        pci_cfg: RefCell::new(pci_cfg),
        pci_intx: RefCell::new(pci_intx),
        ram: vec![0u8; 4096],
    };

    let snap1 = save_bytes(&mut source);
    let snap2 = save_bytes(&mut source);
    assert_eq!(snap1, snap2, "snapshot bytes must be deterministic");

    let (bus2, _) = make_bus();
    let mut target = TestTarget {
        pci_cfg: PciConfigPorts::with_bus(bus2),
        pci_intx: PciIntxRouter::new(PciIntxRouterConfig::default()),
        ram: vec![0u8; 4096],
    };

    restore_snapshot(&mut Cursor::new(&snap1), &mut target).unwrap();

    assert_eq!(target.pci_cfg.io_read(0xCF8, 4), expected_latch);
    for (idx, offset) in (0..256u16).step_by(4).enumerate() {
        assert_eq!(
            target.pci_cfg.bus_mut().read_config(bdf, offset, 4),
            expected_cfg[idx],
            "config mismatch at offset {offset:#04x}"
        );
    }
    assert_eq!(target.pci_cfg.bus_mut().mapped_bars(), expected_mapped);

    let mut restored_levels = MockSink::default();
    target.pci_intx.sync_levels_to_sink(&mut restored_levels);
    assert_eq!(restored_levels.events, expected_intx_levels.events);
}
