use std::fs;
use std::io::{Seek, Write};

use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, SaveOptions, SnapshotMeta,
    SnapshotSource,
};
use assert_cmd::Command;
use predicates::prelude::*;

struct DummySource {
    ram: Vec<u8>,
}

impl DummySource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(31)));
        Self { ram }
    }
}

impl SnapshotSource for DummySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("xtask-test".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![DeviceState {
            id: DeviceId::SERIAL,
            version: 1,
            flags: 0,
            data: vec![1, 2, 3, 4],
        }]
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

fn write_snapshot(path: &std::path::Path) {
    let mut file = fs::File::create(path).unwrap();
    // Ensure we always start writing at offset 0; Windows tests occasionally reuse handles.
    file.rewind().unwrap();
    let mut source = DummySource::new(4096);
    aero_snapshot::save_snapshot(&mut file, &mut source, SaveOptions::default()).unwrap();
    file.flush().unwrap();
}

#[test]
fn snapshot_inspect_prints_meta_and_ram_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("META:"))
        .stdout(predicate::str::contains("snapshot_id: 1"))
        .stdout(predicate::str::contains("label: xtask-test"))
        .stdout(predicate::str::contains("Sections:"))
        .stdout(predicate::str::contains("offset="))
        .stdout(predicate::str::contains("RAM:"))
        .stdout(predicate::str::contains("mode: full"))
        .stdout(predicate::str::contains("compression: lz4"));
}

#[test]
fn snapshot_validate_and_deep_validate_succeed() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid snapshot"));

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid snapshot"));
}

#[test]
fn snapshot_validate_fails_on_truncated_files() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    let bytes = fs::read(&snap).unwrap();
    assert!(bytes.len() > 16);

    let truncated = tmp.path().join("truncated.aerosnap");
    fs::write(&truncated, &bytes[..bytes.len() - 8]).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", truncated.to_str().unwrap()])
        .assert()
        .failure();
}
