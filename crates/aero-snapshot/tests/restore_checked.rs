#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot::{
    restore_snapshot, restore_snapshot_checked, save_snapshot, Compression, CpuState, DeviceState,
    DiskOverlayRefs, MmuState, RamMode, RamWriteOptions, RestoreOptions, SaveOptions, SectionId,
    SnapshotError, SnapshotMeta, SnapshotSource, SnapshotTarget, SNAPSHOT_ENDIANNESS_LITTLE,
    SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
};

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

#[derive(Clone)]
struct DummySource {
    ram: Vec<u8>,
    dirty_pages: Vec<u64>,
    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,
}

impl DummySource {
    fn new(ram_len: usize) -> Self {
        let mut ram = vec![0u8; ram_len];
        for (idx, b) in ram.iter_mut().enumerate() {
            *b = idx as u8;
        }
        Self {
            ram,
            dirty_pages: Vec::new(),
            next_snapshot_id: 1,
            last_snapshot_id: None,
        }
    }

    fn write_u8(&mut self, addr: usize, val: u8) {
        self.ram[addr] = val;
        self.dirty_pages.push((addr / 4096) as u64);
    }
}

impl SnapshotSource for DummySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id += 1;
        let meta = SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms: 0,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
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
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(std::mem::take(&mut self.dirty_pages))
    }
}

fn snapshot_bytes<S: SnapshotSource>(
    source: &mut S,
    options: SaveOptions,
) -> aero_snapshot::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, options)?;
    Ok(cursor.into_inner())
}

fn push_section(dst: &mut Vec<u8>, id: SectionId, version: u16, flags: u16, payload: &[u8]) {
    dst.extend_from_slice(&id.0.to_le_bytes());
    dst.extend_from_slice(&version.to_le_bytes());
    dst.extend_from_slice(&flags.to_le_bytes());
    dst.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    dst.extend_from_slice(payload);
}

fn push_file_header(dst: &mut Vec<u8>) {
    dst.extend_from_slice(SNAPSHOT_MAGIC);
    dst.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    dst.push(SNAPSHOT_ENDIANNESS_LITTLE);
    dst.push(0);
    dst.extend_from_slice(&0u32.to_le_bytes());
}

#[test]
fn restore_snapshot_checked_validates_dirty_parent() {
    let page_size = 4096usize;
    let ram_len = page_size * 2;
    let mut source = DummySource::new(ram_len);

    source.write_u8(0, 0xAA);
    source.write_u8(page_size, 0xBB);
    let base_mem = source.ram.clone();

    let base_bytes = snapshot_bytes(&mut source, SaveOptions::default()).unwrap();
    let base_snapshot_id = source.last_snapshot_id.unwrap();

    source.write_u8(1, 0xCC);
    let expected_final_mem = source.ram.clone();

    let mut dirty_opts = SaveOptions::default();
    dirty_opts.ram.mode = RamMode::Dirty;
    let diff_bytes = snapshot_bytes(&mut source, dirty_opts).unwrap();

    let mut target = DummyTarget::new(ram_len);
    restore_snapshot(&mut Cursor::new(base_bytes.as_slice()), &mut target).unwrap();
    assert_eq!(target.ram, base_mem);

    let err = restore_snapshot_checked(
        &mut Cursor::new(diff_bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: None,
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("snapshot parent mismatch")
    ));

    let err = restore_snapshot_checked(
        &mut Cursor::new(diff_bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(base_snapshot_id + 1),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("snapshot parent mismatch")
    ));

    restore_snapshot_checked(
        &mut Cursor::new(diff_bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(base_snapshot_id),
        },
    )
    .unwrap();
    assert_eq!(target.ram, expected_final_mem);
}

#[test]
fn restore_snapshot_checked_full_ignores_expected_parent() {
    let mut source = DummySource::new(4096);
    source.write_u8(7, 0xAA);
    let expected = source.ram.clone();

    let bytes = snapshot_bytes(&mut source, SaveOptions::default()).unwrap();
    let mut target = DummyTarget::new(4096);
    restore_snapshot_checked(
        &mut Cursor::new(bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(12345),
        },
    )
    .unwrap();
    assert_eq!(target.ram, expected);
}

#[test]
fn dirty_snapshot_requires_meta_before_ram() {
    const EXPECTED_ERR: &str = "dirty snapshot requires META section before RAM";

    let total_len = 4096u64;

    let mut bytes_missing_meta = Vec::new();
    push_file_header(&mut bytes_missing_meta);

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes_missing_meta, SectionId::CPU, 2, 0, &cpu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&total_len.to_le_bytes());
    ram_payload.extend_from_slice(&4096u32.to_le_bytes());
    ram_payload.push(RamMode::Dirty as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // dirty count
    push_section(&mut bytes_missing_meta, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(total_len as usize);
    let err =
        restore_snapshot(&mut Cursor::new(bytes_missing_meta.as_slice()), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt(EXPECTED_ERR)));

    let err = restore_snapshot_checked(
        &mut Cursor::new(bytes_missing_meta.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(1),
        },
    )
    .unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt(EXPECTED_ERR)));

    let mut bytes_meta_after = Vec::new();
    push_file_header(&mut bytes_meta_after);
    push_section(&mut bytes_meta_after, SectionId::CPU, 2, 0, &cpu_payload);
    push_section(&mut bytes_meta_after, SectionId::RAM, 1, 0, &ram_payload);

    let meta = SnapshotMeta {
        snapshot_id: 2,
        parent_snapshot_id: Some(1),
        created_unix_ms: 0,
        label: None,
    };
    let mut meta_payload = Vec::new();
    meta.encode(&mut meta_payload).unwrap();
    push_section(&mut bytes_meta_after, SectionId::META, 1, 0, &meta_payload);

    let err =
        restore_snapshot(&mut Cursor::new(bytes_meta_after.as_slice()), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt(EXPECTED_ERR)));

    let err = restore_snapshot_checked(
        &mut Cursor::new(bytes_meta_after.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(1),
        },
    )
    .unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt(EXPECTED_ERR)));
}

#[test]
fn dirty_snapshot_requires_parent_snapshot_id() {
    let total_len = 4096u64;

    let mut bytes = Vec::new();
    push_file_header(&mut bytes);

    let meta = SnapshotMeta {
        snapshot_id: 2,
        parent_snapshot_id: None,
        created_unix_ms: 0,
        label: None,
    };
    let mut meta_payload = Vec::new();
    meta.encode(&mut meta_payload).unwrap();
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&total_len.to_le_bytes());
    ram_payload.extend_from_slice(&4096u32.to_le_bytes());
    ram_payload.push(RamMode::Dirty as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&0u64.to_le_bytes());
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(total_len as usize);
    let err = restore_snapshot(&mut Cursor::new(bytes.as_slice()), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("dirty snapshot missing parent_snapshot_id")
    ));

    let err = restore_snapshot_checked(
        &mut Cursor::new(bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(1),
        },
    )
    .unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("dirty snapshot missing parent_snapshot_id")
    ));
}

#[test]
fn restore_snapshot_checked_respects_custom_dirty_page_size() {
    // Regression test: restore_snapshot_checked should still decode RAM headers correctly with
    // non-default page/chunk sizes.
    let mut source = DummySource::new(8192);
    source.write_u8(0, 1);

    let opts = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Full,
            compression: Compression::None,
            page_size: 4096,
            chunk_size: 1024,
        },
    };
    let bytes = snapshot_bytes(&mut source, opts).unwrap();

    let mut target = DummyTarget::new(8192);
    restore_snapshot_checked(
        &mut Cursor::new(bytes.as_slice()),
        &mut target,
        RestoreOptions {
            expected_parent_snapshot_id: Some(999),
        },
    )
    .unwrap();
    assert_eq!(target.ram, source.ram);
}
