use std::io::{self, Read, Seek, SeekFrom, Write};

#[cfg(target_arch = "wasm32")]
const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991; // 2^53 - 1

/// Minimal interface needed to turn an OPFS `FileSystemSyncAccessHandle` into a `std::io`
/// `Read`/`Write`/`Seek` stream.
///
/// This is public so that unit tests can provide an in-memory mock handle.
pub trait OpfsSyncFileHandle {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize>;
    fn get_size(&mut self) -> io::Result<u64>;
    fn truncate(&mut self, size: u64) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn close(&mut self) -> io::Result<()>;
}

#[cfg(target_arch = "wasm32")]
mod platform_handle {
    use super::OpfsSyncFileHandle;
    use std::io;

    use crate::platform::storage::opfs as opfs_platform;
    use js_sys::{Object, Reflect};
    use wasm_bindgen::JsValue;

    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991; // 2^53 - 1

    fn u64_to_f64_checked(value: u64) -> io::Result<f64> {
        if value > MAX_SAFE_INTEGER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "offset {value} exceeds JS MAX_SAFE_INTEGER ({MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets"
                ),
            ));
        }
        Ok(value as f64)
    }

    fn js_number_to_u64_checked(value: f64) -> io::Result<u64> {
        if !value.is_finite() || value < 0.0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid OPFS numeric value {value}"),
            ));
        }

        let as_u64 = value as u64;
        if as_u64 as f64 != value {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("non-integer OPFS numeric value {value}"),
            ));
        }

        if as_u64 > MAX_SAFE_INTEGER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "OPFS numeric value {value} exceeds JS MAX_SAFE_INTEGER ({MAX_SAFE_INTEGER})"
                ),
            ));
        }

        Ok(as_u64)
    }

    pub(super) fn disk_error_to_io(err: crate::DiskError) -> io::Error {
        use crate::DiskError;

        match err {
            DiskError::NotSupported(_) | DiskError::Unsupported(_) => {
                io::Error::new(io::ErrorKind::Unsupported, err)
            }
            DiskError::BackendUnavailable => {
                io::Error::new(io::ErrorKind::NotConnected, err)
            }
            DiskError::InUse => io::Error::new(io::ErrorKind::ResourceBusy, err),
            DiskError::QuotaExceeded => io::Error::new(io::ErrorKind::StorageFull, err),
            DiskError::InvalidState(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, err)
            }
            DiskError::UnalignedLength { .. }
            | DiskError::OutOfBounds { .. }
            | DiskError::OffsetOverflow => {
                io::Error::new(io::ErrorKind::InvalidInput, err)
            }
            DiskError::CorruptImage(_)
            | DiskError::InvalidSparseHeader(_)
            | DiskError::CorruptSparseImage(_) => {
                io::Error::new(io::ErrorKind::InvalidData, err)
            }
            DiskError::InvalidConfig(_) => {
                io::Error::new(io::ErrorKind::InvalidInput, err)
            }
            DiskError::Io(_) => io::Error::new(io::ErrorKind::Other, err),
        }
    }

    fn js_error_to_io(err: JsValue) -> io::Error {
        disk_error_to_io(opfs_platform::disk_error_from_js(err))
    }

    fn set_at(opts: &Object, at_key: &JsValue, at: u64) -> io::Result<()> {
        Reflect::set(opts, at_key, &JsValue::from_f64(u64_to_f64_checked(at)?))
            .map_err(js_error_to_io)?;
        Ok(())
    }

    /// Wrapper that carries the reusable `{ at: ... }` options object for OPFS reads/writes.
    #[derive(Clone)]
    pub struct WasmSyncHandle {
        handle: opfs_platform::SyncAccessHandle,
        at_key: JsValue,
        rw_opts: Object,
    }

    impl core::fmt::Debug for WasmSyncHandle {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("WasmSyncHandle").finish_non_exhaustive()
        }
    }

    impl WasmSyncHandle {
        pub fn new(handle: opfs_platform::SyncAccessHandle) -> io::Result<Self> {
            let at_key = JsValue::from_str("at");
            let rw_opts = Object::new();
            Reflect::set(&rw_opts, &at_key, &JsValue::from_f64(0.0)).map_err(js_error_to_io)?;
            Ok(Self {
                handle,
                at_key,
                rw_opts,
            })
        }
    }

    impl OpfsSyncFileHandle for WasmSyncHandle {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            set_at(&self.rw_opts, &self.at_key, offset)?;
            let read = self
                .handle
                .read(buf, self.rw_opts.as_ref())
                .map_err(js_error_to_io)? as usize;
            Ok(read)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize> {
            set_at(&self.rw_opts, &self.at_key, offset)?;
            let wrote = self
                .handle
                .write(buf, self.rw_opts.as_ref())
                .map_err(js_error_to_io)? as usize;
            Ok(wrote)
        }

        fn get_size(&mut self) -> io::Result<u64> {
            let size = self.handle.get_size().map_err(js_error_to_io)?;
            js_number_to_u64_checked(size)
        }

        fn truncate(&mut self, size: u64) -> io::Result<()> {
            self.handle
                .truncate(u64_to_f64_checked(size)?)
                .map_err(js_error_to_io)?;
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.handle.flush().map_err(js_error_to_io)?;
            Ok(())
        }

        fn close(&mut self) -> io::Result<()> {
            self.handle.close().map_err(js_error_to_io)?;
            Ok(())
        }
    }

    pub type DefaultHandle = WasmSyncHandle;
}

#[cfg(not(target_arch = "wasm32"))]
mod platform_handle {
    use super::OpfsSyncFileHandle;
    use std::fs::File;
    use std::io::{self, Read, Seek, SeekFrom, Write};

    impl OpfsSyncFileHandle for File {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.seek(SeekFrom::Start(offset))?;
            self.read(buf)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<usize> {
            self.seek(SeekFrom::Start(offset))?;
            self.write(buf)
        }

        fn get_size(&mut self) -> io::Result<u64> {
            Ok(self.metadata()?.len())
        }

        fn truncate(&mut self, size: u64) -> io::Result<()> {
            self.set_len(size)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.sync_data()
        }

        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    pub type DefaultHandle = File;
}

type DefaultHandle = platform_handle::DefaultHandle;

/// `std::io::{Read, Write, Seek}` wrapper over OPFS `FileSystemSyncAccessHandle` with an internal
/// cursor. On non-wasm targets this wraps a `std::fs::File` for compatibility/testing.
#[derive(Debug)]
pub struct OpfsSyncFile<H: OpfsSyncFileHandle = DefaultHandle> {
    handle: Option<H>,
    pos: u64,
}

impl<H: OpfsSyncFileHandle> OpfsSyncFile<H> {
    pub fn from_handle(handle: H) -> Self {
        Self {
            handle: Some(handle),
            pos: 0,
        }
    }

    fn handle_mut(&mut self) -> io::Result<&mut H> {
        self.handle
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "file is closed"))
    }

    pub fn into_inner(mut self) -> Option<H> {
        self.handle.take()
    }
}

impl OpfsSyncFile
where
    DefaultHandle: OpfsSyncFileHandle,
{
    /// Open an OPFS file for sync access. On wasm this requires `createSyncAccessHandle`, which is
    /// only available from a DedicatedWorker.
    #[cfg(target_arch = "wasm32")]
    pub async fn open(path: &str, create: bool) -> io::Result<Self> {
        use crate::platform::storage::opfs as opfs_platform;

        if !opfs_platform::is_worker_scope() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "OPFS sync access handles are unavailable; this API requires DedicatedWorkerGlobalScope",
            ));
        }

        let file = opfs_platform::open_file(path, create)
            .await
            .map_err(platform_handle::disk_error_to_io)?;

        if !opfs_platform::file_handle_supports_sync_access_handle(&file) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "OPFS sync access handles are not supported in this browser (FileSystemFileHandle.createSyncAccessHandle missing)",
            ));
        }

        let handle = opfs_platform::create_sync_handle(&file)
            .await
            .map_err(platform_handle::disk_error_to_io)?;

        let handle = platform_handle::WasmSyncHandle::new(handle)?;
        Ok(Self::from_handle(handle))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub async fn open(path: &str, create: bool) -> io::Result<Self> {
        use std::fs::OpenOptions;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .open(path)?;
        Ok(Self::from_handle(file))
    }

    /// Create (or truncate) a file suitable for writing a fresh snapshot.
    pub async fn create(path: &str) -> io::Result<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::fs::OpenOptions;

            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            Ok(Self::from_handle(file))
        }

        #[cfg(target_arch = "wasm32")]
        {
            let mut file = Self::open(path, true).await?;
            file.truncate(0)?;
            file.pos = 0;
            Ok(file)
        }
    }
}

impl<H: OpfsSyncFileHandle> OpfsSyncFile<H> {
    pub fn truncate(&mut self, size: u64) -> io::Result<()> {
        self.handle_mut()?.truncate(size)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.handle_mut()?.flush()
    }

    pub fn close(&mut self) -> io::Result<()> {
        let mut handle = match self.handle.take() {
            Some(handle) => handle,
            None => return Ok(()),
        };

        let flush_res = handle.flush();
        let close_res = handle.close();
        // Ensure the handle is dropped even if close fails.
        drop(handle);
        flush_res.and(close_res)
    }
}

impl<H: OpfsSyncFileHandle> Drop for OpfsSyncFile<H> {
    fn drop(&mut self) {
        let mut handle = match self.handle.take() {
            Some(handle) => handle,
            None => return,
        };

        let _ = handle.flush();
        let _ = handle.close();
    }
}

impl<H> Read for OpfsSyncFile<H>
where
    H: OpfsSyncFileHandle,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(target_arch = "wasm32")]
        if self.pos > JS_MAX_SAFE_INTEGER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "offset {} exceeds JS MAX_SAFE_INTEGER ({JS_MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets",
                    self.pos
                ),
            ));
        }

        let pos = self.pos;

        #[cfg(target_arch = "wasm32")]
        let buf = {
            let remaining = JS_MAX_SAFE_INTEGER - pos;
            let max = (buf.len() as u64).min(remaining) as usize;
            &mut buf[..max]
        };

        let read = self.handle_mut()?.read_at(pos, buf)?;
        self.pos = self.pos.checked_add(read as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "stream position overflow")
        })?;
        Ok(read)
    }
}

impl<H> Write for OpfsSyncFile<H>
where
    H: OpfsSyncFileHandle,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(target_arch = "wasm32")]
        {
            if self.pos > JS_MAX_SAFE_INTEGER {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "offset {} exceeds JS MAX_SAFE_INTEGER ({JS_MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets",
                        self.pos
                    ),
                ));
            }

            let remaining = JS_MAX_SAFE_INTEGER - self.pos;
            if (buf.len() as u64) > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "write of {} bytes at offset {} exceeds JS MAX_SAFE_INTEGER ({JS_MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets",
                        buf.len(),
                        self.pos
                    ),
                ));
            }
        }

        let pos = self.pos;
        let wrote = self.handle_mut()?.write_at(pos, buf)?;
        self.pos = self.pos.checked_add(wrote as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "stream position overflow")
        })?;
        Ok(wrote)
    }

    fn flush(&mut self) -> io::Result<()> {
        OpfsSyncFile::<H>::flush(self)
    }
}

impl<H> Seek for OpfsSyncFile<H>
where
    H: OpfsSyncFileHandle,
{
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if self.handle.is_none() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "file is closed"));
        }

        let current_pos = self.pos;

        let base: i128 = match pos {
            SeekFrom::Start(offset) => {
                #[cfg(target_arch = "wasm32")]
                if offset > JS_MAX_SAFE_INTEGER {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!(
                            "seek position {offset} exceeds JS MAX_SAFE_INTEGER ({JS_MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets"
                        ),
                    ));
                }

                self.pos = offset;
                return Ok(offset);
            }
            SeekFrom::Current(_) => i128::from(current_pos),
            SeekFrom::End(_) => {
                let size = self.handle_mut()?.get_size()?;
                i128::from(size)
            }
        };

        let delta: i128 = match pos {
            SeekFrom::Current(delta) | SeekFrom::End(delta) => i128::from(delta),
            SeekFrom::Start(_) => unreachable!("handled above"),
        };

        let next = base
            .checked_add(delta)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek position overflow"))?;

        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid seek to a negative position",
            ));
        }

        let next: u64 = next
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek position overflow"))?;

        #[cfg(target_arch = "wasm32")]
        if next > JS_MAX_SAFE_INTEGER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "seek position {next} exceeds JS MAX_SAFE_INTEGER ({JS_MAX_SAFE_INTEGER}); OPFS sync access handles use f64 offsets"
                ),
            ));
        }

        self.pos = next;
        Ok(next)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{OpfsSyncFile, OpfsSyncFileHandle};
    use std::io::{Read, Seek, SeekFrom, Write};

    use aero_snapshot::{
        CpuState, DiskOverlayRefs, MmuState, RestoreOptions, SaveOptions, SnapshotMeta,
        SnapshotSource, SnapshotTarget,
    };

    #[derive(Default, Debug)]
    struct MockHandle {
        data: Vec<u8>,
    }

    impl OpfsSyncFileHandle for MockHandle {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let offset: usize = offset.try_into().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;
            if offset >= self.data.len() {
                return Ok(0);
            }
            let available = &self.data[offset..];
            let len = available.len().min(buf.len());
            buf[..len].copy_from_slice(&available[..len]);
            Ok(len)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
            let offset: usize = offset.try_into().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;
            let end = offset.checked_add(buf.len()).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;

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
            let size: usize = size.try_into().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "size overflow")
            })?;
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

    /// Sparse in-memory handle that can simulate multi-GB offsets without allocating a multi-GB
    /// `Vec`.
    #[derive(Default, Debug)]
    struct SparseMockHandle {
        size: u64,
        bytes: std::collections::BTreeMap<u64, u8>,
    }

    impl OpfsSyncFileHandle for SparseMockHandle {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            if offset >= self.size {
                return Ok(0);
            }

            let max_len = (self.size - offset).min(buf.len() as u64) as usize;
            buf[..max_len].fill(0);

            let end = offset.checked_add(max_len as u64).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;

            for (pos, byte) in self.bytes.range(offset..end) {
                let idx: usize = (*pos - offset).try_into().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
                })?;
                buf[idx] = *byte;
            }

            Ok(max_len)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
            let end = offset.checked_add(buf.len() as u64).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
            })?;
            self.size = self.size.max(end);

            for (idx, byte) in buf.iter().copied().enumerate() {
                let pos = offset.checked_add(idx as u64).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "offset overflow")
                })?;
                self.bytes.insert(pos, byte);
            }

            Ok(buf.len())
        }

        fn get_size(&mut self) -> std::io::Result<u64> {
            Ok(self.size)
        }

        fn truncate(&mut self, size: u64) -> std::io::Result<()> {
            self.size = size;

            let keys: Vec<u64> = self.bytes.range(size..).map(|(k, _)| *k).collect();
            for k in keys {
                self.bytes.remove(&k);
            }

            Ok(())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn close(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn read_to_end_seek_start<R: Read + Seek>(mut r: R) -> Vec<u8> {
        r.seek(SeekFrom::Start(0)).unwrap();
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn sequential_write_then_read_back() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"hello").unwrap();
        file.write_all(b" world").unwrap();

        file.seek(SeekFrom::Start(0)).unwrap();
        let mut buf = [0u8; 11];
        file.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello world");
    }

    #[test]
    fn seek_and_overwrite() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"abcdef").unwrap();

        file.seek(SeekFrom::Start(2)).unwrap();
        file.write_all(b"ZZ").unwrap();

        assert_eq!(read_to_end_seek_start(&mut file), b"abZZef");
    }

    #[test]
    fn seek_from_end_reads_tail() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"hello world").unwrap();

        let pos = file.seek(SeekFrom::End(-5)).unwrap();
        assert_eq!(pos, 6);

        let mut tail = [0u8; 5];
        file.read_exact(&mut tail).unwrap();
        assert_eq!(&tail, b"world");
    }

    #[test]
    fn truncate_then_write() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"abcdefghij").unwrap();

        file.truncate(5).unwrap();
        let pos = file.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(pos, 5);

        file.write_all(b"XYZ").unwrap();
        assert_eq!(read_to_end_seek_start(&mut file), b"abcdeXYZ");
    }

    #[test]
    fn seek_before_start_errors() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        let err = file.seek(SeekFrom::Current(-1)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn seek_after_close_errors() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"abc").unwrap();
        file.close().unwrap();
        let err = file.seek(SeekFrom::Start(0)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn read_after_close_errors() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.write_all(b"abc").unwrap();
        file.close().unwrap();

        let mut buf = [0u8; 1];
        let err = file.read(&mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn write_after_close_errors() {
        let mut file = OpfsSyncFile::from_handle(MockHandle::default());
        file.close().unwrap();

        let err = file.write(b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn large_seek_uses_u64_offsets() {
        let mut file = OpfsSyncFile::from_handle(SparseMockHandle::default());
        let offset = 5u64 * 1024 * 1024 * 1024; // 5 GiB, exercises >u32 offsets.

        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(b"hello").unwrap();

        assert_eq!(file.seek(SeekFrom::End(0)).unwrap(), offset + 5);

        file.seek(SeekFrom::Start(offset - 2)).unwrap();
        let mut buf = [0u8; 7];
        file.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"\0\0hello");

        file.truncate(offset + 2).unwrap();
        assert_eq!(file.seek(SeekFrom::End(0)).unwrap(), offset + 2);

        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut head = [0u8; 2];
        file.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"he");
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
        let mut diff_reader = OpfsSyncFile::from_handle(MockHandle {
            data: diff_bytes.clone(),
        });
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
}
