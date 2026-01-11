use std::io::{Cursor, Read, Seek};

use aero_snapshot::{
    inspect_snapshot, save_snapshot, Compression, CpuState, DiskOverlayRefs, MmuState, RamMode,
    RamWriteOptions, Result, SaveOptions, SnapshotError, SnapshotMeta, SnapshotSource,
};

struct DummySource {
    meta: SnapshotMeta,
    ram_len: usize,
}

impl SnapshotSource for DummySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<aero_snapshot::DeviceState> {
        Vec::new()
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram_len
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset
            .checked_add(buf.len())
            .ok_or(SnapshotError::Corrupt("ram read overflow"))?
            > self.ram_len
        {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }

    fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n;
        Ok(n)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn inspect_reads_only_headers() -> Result<()> {
    let ram_len = 16 * 1024 * 1024;
    let chunk_size = 1024 * 1024;

    let meta = SnapshotMeta {
        snapshot_id: 42,
        parent_snapshot_id: None,
        created_unix_ms: 123456789,
        label: Some("test snapshot".to_string()),
    };

    let mut source = DummySource { meta, ram_len };

    let opts = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Full,
            compression: Compression::None,
            page_size: 4096,
            chunk_size,
        },
    };

    let mut buf = Cursor::new(Vec::new());
    save_snapshot(&mut buf, &mut source, opts)?;
    let snapshot_bytes = buf.into_inner();
    assert!(snapshot_bytes.len() > ram_len / 2);

    let mut reader = CountingReader::new(Cursor::new(snapshot_bytes));
    let index = inspect_snapshot(&mut reader)?;

    let inspected_meta = index.meta.expect("expected META section");
    assert_eq!(inspected_meta.snapshot_id, 42);
    assert_eq!(inspected_meta.label.as_deref(), Some("test snapshot"));

    let ram = index.ram.expect("expected RAM section");
    assert_eq!(ram.total_len, ram_len as u64);
    assert_eq!(ram.page_size, 4096);
    assert_eq!(ram.mode, RamMode::Full);
    assert_eq!(ram.compression, Compression::None);
    assert_eq!(ram.chunk_size, Some(chunk_size));
    assert_eq!(ram.dirty_count, None);

    assert!(
        reader.bytes_read() < 4096,
        "inspection unexpectedly read {} bytes",
        reader.bytes_read()
    );

    Ok(())
}

