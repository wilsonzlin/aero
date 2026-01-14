#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot::{
    save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRef, DiskOverlayRefs,
    MmuState, SaveOptions, SnapshotMeta, SnapshotSource, VcpuMmuSnapshot, VcpuSnapshot,
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

struct CustomSource {
    meta: SnapshotMeta,
    cpu: CpuState,
    mmu: MmuState,
    disks: DiskOverlayRefs,
    ram: Vec<u8>,
}

impl CustomSource {
    fn new(meta: SnapshotMeta, cpu: CpuState, mmu: MmuState, disks: DiskOverlayRefs) -> Self {
        let mut ram = Vec::with_capacity(4096);
        ram.extend((0..4096).map(|i| (i as u8).wrapping_mul(23)));
        Self {
            meta,
            cpu,
            mmu,
            disks,
            ram,
        }
    }
}

impl SnapshotSource for CustomSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
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
        self.disks.clone()
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
fn snapshot_diff_detects_cpu_mmu_disks_and_ram_header_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap_a = tmp.path().join("cpu_a.aerosnap");
    let snap_b = tmp.path().join("cpu_b.aerosnap");

    let mut cursor = Cursor::new(Vec::new());
    let mut source_a = CustomSource::new(
        SnapshotMeta {
            snapshot_id: 10,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("a".to_string()),
        },
        CpuState {
            rip: 0x1111,
            ..Default::default()
        },
        MmuState {
            cr3: 0x1000,
            ..Default::default()
        },
        DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: 0,
                base_image: "base.img".to_string(),
                overlay_image: "overlay.img".to_string(),
            }],
        },
    );
    let mut opts_a = SaveOptions::default();
    opts_a.ram.compression = Compression::None;
    opts_a.ram.chunk_size = 1024;
    save_snapshot(&mut cursor, &mut source_a, opts_a).unwrap();
    std::fs::write(&snap_a, cursor.into_inner()).unwrap();

    let mut cursor = Cursor::new(Vec::new());
    let mut source_b = CustomSource::new(
        SnapshotMeta {
            snapshot_id: 11,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("b".to_string()),
        },
        CpuState {
            rip: 0x2222,
            ..Default::default()
        },
        MmuState {
            cr3: 0x2000,
            ..Default::default()
        },
        DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: 0,
                base_image: "base2.img".to_string(),
                overlay_image: "overlay.img".to_string(),
            }],
        },
    );
    let mut opts_b = SaveOptions::default();
    opts_b.ram.compression = Compression::Lz4;
    opts_b.ram.chunk_size = 2048;
    save_snapshot(&mut cursor, &mut source_b, opts_b).unwrap();
    std::fs::write(&snap_b, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "snapshot",
            "diff",
            snap_a.to_str().unwrap(),
            snap_b.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("diff CPU.rip"))
        .stdout(predicate::str::contains("0x1111"))
        .stdout(predicate::str::contains("0x2222"))
        .stdout(predicate::str::contains("diff MMU.cr3"))
        .stdout(predicate::str::contains("0x1000"))
        .stdout(predicate::str::contains("0x2000"))
        .stdout(predicate::str::contains("diff DISKS[disk_id=0].base_image"))
        .stdout(predicate::str::contains("base2.img"))
        .stdout(predicate::str::contains("diff RAM.compression: A=none B=lz4"))
        .stdout(predicate::str::contains("diff RAM.chunk_size"))
        .stdout(predicate::str::contains("A=1024"))
        .stdout(predicate::str::contains("B=2048"));
}

struct TwoCpuMmusSource {
    meta: SnapshotMeta,
    mmu0_cr3: u64,
    mmu1_cr3: u64,
    ram: Vec<u8>,
}

impl TwoCpuMmusSource {
    fn new(meta: SnapshotMeta, mmu0_cr3: u64, mmu1_cr3: u64) -> Self {
        let mut ram = Vec::with_capacity(4096);
        ram.extend((0..4096).map(|i| (i as u8).wrapping_mul(17)));
        Self {
            meta,
            mmu0_cr3,
            mmu1_cr3,
            ram,
        }
    }
}

impl SnapshotSource for TwoCpuMmusSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        vec![
            VcpuSnapshot {
                apic_id: 0,
                cpu: CpuState::default(),
                internal_state: Vec::new(),
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

    fn mmu_states(&self) -> Vec<VcpuMmuSnapshot> {
        vec![
            VcpuMmuSnapshot {
                apic_id: 0,
                mmu: MmuState {
                    cr3: self.mmu0_cr3,
                    ..Default::default()
                },
            },
            VcpuMmuSnapshot {
                apic_id: 1,
                mmu: MmuState {
                    cr3: self.mmu1_cr3,
                    ..Default::default()
                },
            },
        ]
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
fn snapshot_diff_detects_mmus_entry_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap_a = tmp.path().join("mmus_a.aerosnap");
    let snap_b = tmp.path().join("mmus_b.aerosnap");

    let mut cursor = Cursor::new(Vec::new());
    let mut source_a = TwoCpuMmusSource::new(
        SnapshotMeta {
            snapshot_id: 20,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("mmus-a".to_string()),
        },
        0x1000,
        0x2000,
    );
    save_snapshot(&mut cursor, &mut source_a, SaveOptions::default()).unwrap();
    std::fs::write(&snap_a, cursor.into_inner()).unwrap();

    let mut cursor = Cursor::new(Vec::new());
    let mut source_b = TwoCpuMmusSource::new(
        SnapshotMeta {
            snapshot_id: 21,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("mmus-b".to_string()),
        },
        0x1000,
        0x3000,
    );
    save_snapshot(&mut cursor, &mut source_b, SaveOptions::default()).unwrap();
    std::fs::write(&snap_b, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args([
            "snapshot",
            "diff",
            snap_a.to_str().unwrap(),
            snap_b.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stdout(predicate::str::contains("diff MMUS[apic_id=1].cr3"))
        .stdout(predicate::str::contains("A=0x2000"))
        .stdout(predicate::str::contains("B=0x3000"));
}
