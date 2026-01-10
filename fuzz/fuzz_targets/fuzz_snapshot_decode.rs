#![no_main]

use aero_snapshot::{
    restore_snapshot, CpuState, DeviceState, DiskOverlayRefs, MmuState, SnapshotTarget,
};
use libfuzzer_sys::fuzz_target;

#[derive(Default)]
struct DummyTarget {
    ram: Vec<u8>,
}

impl DummyTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            ram: vec![0u8; ram_len],
        }
    }
}

impl SnapshotTarget for DummyTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}
    fn restore_mmu_state(&mut self, _state: MmuState) {}
    fn restore_device_states(&mut self, _states: Vec<DeviceState>) {}
    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        let offset = offset as usize;
        if offset.saturating_add(data.len()) > self.ram.len() {
            return Err(aero_snapshot::SnapshotError::Corrupt(
                "ram write out of bounds",
            ));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let mut target = DummyTarget::new(1024 * 1024);
    let _ = restore_snapshot(&mut std::io::Cursor::new(data), &mut target);
});
