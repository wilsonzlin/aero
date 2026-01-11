#![cfg(feature = "io-snapshot")]

use std::io::Cursor;

use aero_devices_input::I8042Controller;
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotError, SnapshotMeta, SnapshotSource, SnapshotTarget,
};

const DEVICE_ID_I8042: DeviceId = DeviceId(0x8042);

struct TestSource {
    device_state: DeviceState,
    ram: Vec<u8>,
}

impl SnapshotSource for TestSource {
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
        vec![self.device_state.clone()]
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
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + buf.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TestTarget {
    i8042: I8042Controller,
    ram: Vec<u8>,
}

impl TestTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            i8042: I8042Controller::new(),
            ram: vec![0u8; ram_len],
        }
    }
}

impl SnapshotTarget for TestTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DEVICE_ID_I8042 {
                apply_io_snapshot_to_device(&state, &mut self.i8042).unwrap();
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
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn i8042_io_snapshot_roundtrips_through_aero_snapshot_file() {
    let mut dev = I8042Controller::new();
    dev.inject_browser_key("KeyA", true);
    dev.inject_browser_key("KeyA", false);

    let device_state = device_state_from_io_snapshot(DEVICE_ID_I8042, &dev);

    let mut source = TestSource {
        device_state,
        ram: vec![0u8; 4096],
    };

    let mut save = SaveOptions::default();
    save.ram.compression = Compression::None;
    save.ram.chunk_size = 4096;

    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, save).unwrap();
    let bytes = cursor.into_inner();

    let mut target = TestTarget::new(4096);
    restore_snapshot(&mut Cursor::new(&bytes), &mut target).unwrap();

    assert_eq!(target.i8042.read_port(0x60), 0x1e);
    assert_eq!(target.i8042.read_port(0x60), 0x9e);
    assert_eq!(target.i8042.read_port(0x60), 0x00);
}
