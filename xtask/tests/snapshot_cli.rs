use std::fs;
use std::io::Cursor;
use std::io::{Seek, Write};

use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, RamMode, RamWriteOptions,
    SaveOptions, SectionId, SnapshotMeta, SnapshotSource, VcpuSnapshot,
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

struct MultiCpuSource {
    ram: Vec<u8>,
}

impl MultiCpuSource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(17)));
        Self { ram }
    }
}

impl SnapshotSource for MultiCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 2,
            parent_snapshot_id: Some(1),
            created_unix_ms: 0,
            label: Some("xtask-multicpu".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        vec![
            VcpuSnapshot {
                apic_id: 0,
                cpu: CpuState::default(),
                internal_state: vec![0xAA, 0xBB, 0xCC, 0xDD],
            },
            VcpuSnapshot {
                apic_id: 1,
                cpu: CpuState::default(),
                internal_state: Vec::new(),
            },
        ]
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

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

fn corrupt_first_vcpu_internal_len(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let cpus = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .expect("CPUS section missing");
    let start = cpus.offset as usize;
    assert!(start + 4 + 8 <= snapshot.len());
    let count = read_u32_le(&snapshot[start..start + 4]);
    assert!(count > 0);

    // First entry framing: u64 entry_len followed by the vCPU entry itself.
    let entry_len_off = start + 4;
    let entry_len = read_u64_le(&snapshot[entry_len_off..entry_len_off + 8]) as usize;
    let entry_start = entry_len_off + 8;
    assert!(entry_start + entry_len <= snapshot.len());

    // Entry layout v2: apic_id (u32) + CpuState::encode_v2 + internal_len (u64) + internal_state.
    let cpu_len = {
        let mut tmp = Vec::new();
        CpuState::default().encode_v2(&mut tmp).unwrap();
        tmp.len()
    };

    let internal_len_off = entry_start + 4 + cpu_len;
    assert!(internal_len_off + 8 <= snapshot.len());
    let old = read_u64_le(&snapshot[internal_len_off..internal_len_off + 8]);
    let new = old + 1;
    snapshot[internal_len_off..internal_len_off + 8].copy_from_slice(&new.to_le_bytes());
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

#[test]
fn snapshot_validate_supports_multi_cpu_and_rejects_corrupt_internal_len() {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("multi.aerosnap");
    let mut source = MultiCpuSource::new(4096);

    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let bytes = cursor.into_inner();

    // Sanity: snapshot should contain a CPUS section.
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&bytes)).unwrap();
    assert!(index.sections.iter().any(|s| s.id == SectionId::CPUS));

    fs::write(&good, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", good.to_str().unwrap()])
        .assert()
        .success();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", good.to_str().unwrap()])
        .assert()
        .success();

    // Corrupt only the vCPU internal_len field (keep section framing intact).
    let mut corrupt = bytes.clone();
    corrupt_first_vcpu_internal_len(&mut corrupt);
    let corrupt_path = tmp.path().join("corrupt_internal_len.aerosnap");
    fs::write(&corrupt_path, &corrupt).unwrap();

    // Inspect should still succeed (it doesn't decode CPUS payloads).
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", corrupt_path.to_str().unwrap()])
        .assert()
        .success();

    // Validation should detect the truncated internal_state payload implied by internal_len.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", corrupt_path.to_str().unwrap()])
        .assert()
        .failure();
}

struct LargeDirtySource;

impl SnapshotSource for LargeDirtySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 3,
            parent_snapshot_id: Some(2),
            created_unix_ms: 0,
            label: Some("xtask-large-ram".to_string()),
        }
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
        // Exceeds the `--deep` safety limit (512MiB) but should still be cheap to save because
        // dirty mode with an empty dirty-page set does not read RAM.
        512 * 1024 * 1024 + 4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        // Should never be called for this test (no dirty pages), but keep it correct.
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(Vec::new())
    }
}

#[test]
fn snapshot_deep_validate_refuses_large_ram() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("large_dirty.aerosnap");
    let mut source = LargeDirtySource;

    let mut opts = SaveOptions::default();
    opts.ram = RamWriteOptions {
        mode: RamMode::Dirty,
        ..opts.ram
    };

    let mut file = fs::File::create(&snap).unwrap();
    file.rewind().unwrap();
    aero_snapshot::save_snapshot(&mut file, &mut source, opts).unwrap();
    file.flush().unwrap();

    // Non-deep validation should succeed (it doesn't restore RAM).
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .success();

    // Deep validation should refuse before attempting to restore.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("refuses to restore snapshots with RAM >"));
}
