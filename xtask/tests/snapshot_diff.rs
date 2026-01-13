#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot::{
    save_snapshot, CpuState, DeviceState, DiskOverlayRefs, MmuState, SaveOptions, SnapshotMeta,
    SnapshotSource,
};
use assert_cmd::Command;
use predicates::prelude::*;

struct LabelSource {
    label: String,
    ram: Vec<u8>,
}

impl LabelSource {
    fn new(label: &str) -> Self {
        // Small synthetic RAM to keep snapshots tiny.
        let mut ram = Vec::with_capacity(4096);
        ram.extend((0..4096).map(|i| (i as u8).wrapping_mul(31)));
        Self {
            label: label.to_string(),
            ram,
        }
    }
}

impl SnapshotSource for LabelSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some(self.label.clone()),
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

#[test]
fn snapshot_diff_detects_meta_and_section_difference() {
    let tmp = tempfile::tempdir().unwrap();
    let snap_a = tmp.path().join("a.aerosnap");
    let snap_b = tmp.path().join("b.aerosnap");

    let mut cursor = Cursor::new(Vec::new());
    let mut source_a = LabelSource::new("label_a");
    save_snapshot(&mut cursor, &mut source_a, SaveOptions::default()).unwrap();
    std::fs::write(&snap_a, cursor.into_inner()).unwrap();

    let mut cursor = Cursor::new(Vec::new());
    let mut source_b = LabelSource::new("label_b");
    save_snapshot(&mut cursor, &mut source_b, SaveOptions::default()).unwrap();
    let mut bytes_b = cursor.into_inner();

    // Append an unknown section so the diff has a section-table mismatch to report.
    bytes_b.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // id
    bytes_b.extend_from_slice(&1u16.to_le_bytes()); // version
    bytes_b.extend_from_slice(&0u16.to_le_bytes()); // flags
    bytes_b.extend_from_slice(&4u64.to_le_bytes()); // len
    bytes_b.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    std::fs::write(&snap_b, &bytes_b).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "snapshot",
            "diff",
            snap_a.to_str().unwrap(),
            snap_b.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("diff META.label"))
        .stdout(predicate::str::contains("label_a"))
        .stdout(predicate::str::contains("label_b"))
        .stdout(predicate::str::contains("SectionId(3735928559)"));
}

