#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Seek, SeekFrom};

use aero_opfs::io::snapshot_file::{OpfsSyncFile, OpfsSyncFileHandle};
use aero_snapshot::{
    CpuState, DiskOverlayRefs, MmuState, RestoreOptions, SaveOptions, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};

#[derive(Default, Debug)]
struct MockHandle {
    data: Vec<u8>,
}

impl OpfsSyncFileHandle for MockHandle {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;
        if offset >= self.data.len() {
            return Ok(0);
        }
        let available = &self.data[offset..];
        let len = available.len().min(buf.len());
        buf[..len].copy_from_slice(&available[..len]);
        Ok(len)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow"))?;

        if end > self.data.len() {
            self.data.resize(end, 0);
        }
        self.data[offset..end].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn get_size(&mut self) -> std::io::Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn truncate(&mut self, size: u64) -> std::io::Result<()> {
        let size: usize = size
            .try_into()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "size overflow"))?;
        self.data.resize(size, 0);
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct DummyVm {
    meta: SnapshotMeta,
    cpu: CpuState,
    mmu: MmuState,
    ram: Vec<u8>,
    dirty_pages: Vec<u64>,
}

impl DummyVm {
    fn new(ram_len: usize) -> Self {
        let mut ram = vec![0u8; ram_len];
        for (i, b) in ram.iter_mut().enumerate() {
            *b = (i as u32).wrapping_mul(31) as u8;
        }

        Self {
            meta: SnapshotMeta {
                snapshot_id: 1,
                parent_snapshot_id: None,
                created_unix_ms: 0,
                label: Some("dummy".to_string()),
            },
            cpu: CpuState {
                rax: 0x1234_5678_9abc_def0,
                rip: 0xdead_beef,
                ..CpuState::default()
            },
            mmu: MmuState {
                cr3: 0xfeed_face,
                ..MmuState::default()
            },
            ram,
            dirty_pages: Vec::new(),
        }
    }
}

impl SnapshotSource for DummyVm {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
    }

    fn device_states(&self) -> Vec<aero_snapshot::DeviceState> {
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
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram range overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(core::mem::take(&mut self.dirty_pages))
    }
}

impl SnapshotTarget for DummyVm {
    fn restore_meta(&mut self, meta: SnapshotMeta) {
        self.meta = meta;
    }

    fn restore_cpu_state(&mut self, state: CpuState) {
        self.cpu = state;
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        self.mmu = state;
    }

    fn restore_device_states(&mut self, _states: Vec<aero_snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram range overflow"))?;
        self.ram[offset..end].copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn snapshot_round_trip_uses_seekable_opfs_file() {
    let mut source = DummyVm::new(128 * 1024);
    let mut file = OpfsSyncFile::from_handle(MockHandle::default());

    aero_snapshot::save_snapshot(&mut file, &mut source, SaveOptions::default()).unwrap();

    // Exercise the same cursor-based reads that OPFS uses (positioned reads with `Seek`).
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut restored = DummyVm::new(128 * 1024);
    restored.ram.fill(0);

    aero_snapshot::restore_snapshot(&mut file, &mut restored).unwrap();

    assert_eq!(restored.meta, source.meta);
    assert_eq!(restored.cpu, source.cpu);
    assert_eq!(restored.mmu, source.mmu);
    assert_eq!(restored.ram, source.ram);

    // Ensure the file was written and is readable via ordinary `Read` APIs too.
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut header = [0u8; 8];
    file.read_exact(&mut header).unwrap();
    assert_eq!(&header, aero_snapshot::SNAPSHOT_MAGIC);
}

#[test]
fn restore_snapshot_with_options_checks_parent_using_opfs_file() {
    let mut source = DummyVm::new(64 * 1024);

    // Base snapshot (id=1, parent=None).
    source.meta.snapshot_id = 1;
    source.meta.parent_snapshot_id = None;
    let mut base_file = OpfsSyncFile::from_handle(MockHandle::default());
    aero_snapshot::save_snapshot(&mut base_file, &mut source, SaveOptions::default()).unwrap();
    let base_bytes = base_file.into_inner().unwrap().data;

    // Mutate RAM + create a dirty snapshot (id=2, parent=1).
    source.ram[0] ^= 0xFF;
    source.dirty_pages = vec![0];
    source.meta.snapshot_id = 2;
    source.meta.parent_snapshot_id = Some(1);

    let mut dirty_opts = SaveOptions::default();
    dirty_opts.ram.mode = aero_snapshot::RamMode::Dirty;
    let mut diff_file = OpfsSyncFile::from_handle(MockHandle::default());
    aero_snapshot::save_snapshot(&mut diff_file, &mut source, dirty_opts).unwrap();
    let diff_bytes = diff_file.into_inner().unwrap().data;

    // Applying the diff without having restored its base should fail fast during the prescan.
    let mut restored = DummyVm::new(64 * 1024);
    restored.ram.fill(0);
    let mut diff_reader = OpfsSyncFile::from_handle(MockHandle { data: diff_bytes.clone() });
    let err = aero_snapshot::restore_snapshot_with_options(
        &mut diff_reader,
        &mut restored,
        RestoreOptions {
            expected_parent_snapshot_id: None,
        },
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("snapshot parent mismatch"),
        "unexpected error: {err}"
    );

    // Restoring base + diff with the correct parent should succeed and apply the RAM change.
    let mut restored = DummyVm::new(64 * 1024);
    restored.ram.fill(0);

    let mut base_reader = OpfsSyncFile::from_handle(MockHandle { data: base_bytes });
    aero_snapshot::restore_snapshot_with_options(
        &mut base_reader,
        &mut restored,
        RestoreOptions {
            expected_parent_snapshot_id: None,
        },
    )
    .unwrap();

    let mut diff_reader = OpfsSyncFile::from_handle(MockHandle { data: diff_bytes });
    aero_snapshot::restore_snapshot_with_options(
        &mut diff_reader,
        &mut restored,
        RestoreOptions {
            expected_parent_snapshot_id: Some(1),
        },
    )
    .unwrap();

    assert_eq!(restored.ram, source.ram);
    assert_eq!(restored.meta.snapshot_id, 2);
    assert_eq!(restored.meta.parent_snapshot_id, Some(1));
}
