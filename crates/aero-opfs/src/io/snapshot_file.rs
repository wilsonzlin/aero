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
            DiskError::NotSupported(_)
            | DiskError::BackendUnavailable
            | DiskError::Unsupported(_) => {
                io::Error::new(io::ErrorKind::Unsupported, err.to_string())
            }
            DiskError::InUse => io::Error::new(io::ErrorKind::WouldBlock, err.to_string()),
            DiskError::QuotaExceeded => io::Error::new(io::ErrorKind::StorageFull, err.to_string()),
            DiskError::InvalidState(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, err.to_string())
            }
            DiskError::UnalignedLength { .. }
            | DiskError::OutOfBounds { .. }
            | DiskError::OffsetOverflow => {
                io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
            }
            DiskError::InvalidSparseHeader(_)
            | DiskError::CorruptSparseImage(_)
            | DiskError::CorruptImage(_) => {
                io::Error::new(io::ErrorKind::InvalidData, err.to_string())
            }
            DiskError::InvalidConfig(_) => {
                io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
            }
            DiskError::Io(_) => io::Error::new(io::ErrorKind::Other, err.to_string()),
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
