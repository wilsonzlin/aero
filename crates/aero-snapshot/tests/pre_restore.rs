#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot::{
    restore_snapshot, restore_snapshot_with_options, save_snapshot, CpuState, DeviceState,
    DiskOverlayRefs, MmuState, RestoreOptions, SaveOptions, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};

#[derive(Clone)]
struct TinySource {
    ram: Vec<u8>,
}

impl TinySource {
    fn new(ram_len: usize) -> Self {
        let mut ram = vec![0u8; ram_len];
        for (idx, b) in ram.iter_mut().enumerate() {
            *b = (idx as u8).wrapping_mul(13);
        }
        Self { ram }
    }
}

impl SnapshotSource for TinySource {
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
        Vec::new()
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[derive(Default)]
struct PreRestoreTarget {
    pre_restore_calls: usize,
    restored_cpu: bool,
    restored_mmu: bool,
    restored_devices: bool,
    restored_disks: bool,
    ram: Vec<u8>,
}

impl PreRestoreTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            ram: vec![0u8; ram_len],
            ..Default::default()
        }
    }
}

impl SnapshotTarget for PreRestoreTarget {
    fn pre_restore(&mut self) {
        self.pre_restore_calls += 1;
    }

    fn restore_cpu_state(&mut self, _state: CpuState) {
        assert_eq!(
            self.pre_restore_calls, 1,
            "pre_restore must be called before any restore_* hooks"
        );
        self.restored_cpu = true;
    }

    fn restore_mmu_state(&mut self, _state: MmuState) {
        assert_eq!(
            self.pre_restore_calls, 1,
            "pre_restore must be called before any restore_* hooks"
        );
        self.restored_mmu = true;
    }

    fn restore_device_states(&mut self, _states: Vec<DeviceState>) {
        assert_eq!(
            self.pre_restore_calls, 1,
            "pre_restore must be called before any restore_* hooks"
        );
        self.restored_devices = true;
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {
        assert_eq!(
            self.pre_restore_calls, 1,
            "pre_restore must be called before any restore_* hooks"
        );
        self.restored_disks = true;
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        assert_eq!(
            self.pre_restore_calls, 1,
            "pre_restore must be called before any restore_* hooks"
        );
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram write overflow"))?;
        if end > self.ram.len() {
            return Err(aero_snapshot::SnapshotError::Corrupt(
                "ram write out of bounds",
            ));
        }
        self.ram[offset..end].copy_from_slice(data);
        Ok(())
    }
}

fn save_tiny_snapshot() -> Vec<u8> {
    let mut source = TinySource::new(256);
    let mut w = Cursor::new(Vec::new());
    save_snapshot(&mut w, &mut source, SaveOptions::default()).expect("save_snapshot should succeed");
    w.into_inner()
}

#[test]
fn pre_restore_is_called_on_success() {
    let bytes = save_tiny_snapshot();

    let mut target = PreRestoreTarget::new(256);
    restore_snapshot(&mut Cursor::new(&bytes), &mut target).expect("restore_snapshot should succeed");

    assert_eq!(target.pre_restore_calls, 1);
    assert!(target.restored_cpu);
    assert!(target.restored_mmu);
    assert!(target.restored_devices);
    assert!(target.restored_disks);
}

#[test]
fn pre_restore_is_called_on_success_for_restore_snapshot_with_options() {
    let bytes = save_tiny_snapshot();

    let mut target = PreRestoreTarget::new(256);
    restore_snapshot_with_options(
        &mut Cursor::new(&bytes),
        &mut target,
        RestoreOptions::default(),
    )
    .expect("restore_snapshot_with_options should succeed");

    assert_eq!(target.pre_restore_calls, 1);
    assert!(target.restored_cpu);
    assert!(target.restored_mmu);
    assert!(target.restored_devices);
    assert!(target.restored_disks);
}

#[test]
fn pre_restore_is_called_even_if_restore_fails_after_header() {
    // Header is valid, but the file ends immediately after the header (no sections).
    let bytes = save_tiny_snapshot();
    let bytes = bytes[..16].to_vec();

    let mut target = PreRestoreTarget::new(256);
    assert!(restore_snapshot(&mut Cursor::new(&bytes), &mut target).is_err());
    assert_eq!(target.pre_restore_calls, 1);
}

#[test]
fn pre_restore_is_not_called_for_invalid_header() {
    let mut bytes = save_tiny_snapshot();
    bytes[0] ^= 0xFF;

    let mut target = PreRestoreTarget::new(256);
    assert!(restore_snapshot(&mut Cursor::new(&bytes), &mut target).is_err());
    assert_eq!(target.pre_restore_calls, 0);
}
