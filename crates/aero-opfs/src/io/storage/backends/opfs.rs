use crate::{DiskError, DiskResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpfsBackendMode {
    /// Uses `FileSystemSyncAccessHandle` (fast, worker-only).
    SyncAccessHandle,
    /// Uses Promise-based OPFS APIs (`getFile` + `createWritable`).
    AsyncOpfs,
    /// Uses IndexedDB block storage (fallback when OPFS is unavailable).
    ///
    /// Note: IndexedDB is async and this mode does not currently implement the synchronous
    /// `aero_storage::{StorageBackend, VirtualDisk}` traits used by the boot-critical Rust
    /// controller path.
    ///
    /// See `docs/19-indexeddb-storage-story.md` and `docs/20-storage-trait-consolidation.md`.
    IndexedDb,
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use crate::platform::storage::opfs as opfs_platform;
    use js_sys::{Object, Reflect, Uint8Array};
    use st_idb::{
        DiskBackend as StIdbDiskBackend, IndexedDbBackend as StIndexedDbBackend,
        IndexedDbBackendOptions, StorageError as StIdbError,
    };
    use wasm_bindgen::JsValue;

    const DEFAULT_SECTOR_SIZE: u32 = 512;
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991; // 2^53 - 1

    fn u64_to_f64_checked(value: u64) -> DiskResult<f64> {
        if value > MAX_SAFE_INTEGER {
            return Err(DiskError::Io(format!(
                "offset {value} exceeds JS MAX_SAFE_INTEGER"
            )));
        }
        Ok(value as f64)
    }

    fn js_number_to_u64_checked(value: f64) -> DiskResult<u64> {
        if !value.is_finite() || value < 0.0 {
            return Err(DiskError::Io(format!("invalid size value {value}")));
        }
        let as_u64 = value as u64;
        if as_u64 as f64 != value {
            return Err(DiskError::Io(format!("non-integer size value {value}")));
        }
        Ok(as_u64)
    }

    fn set_at(opts: &Object, at_key: &JsValue, at: u64) -> DiskResult<()> {
        Reflect::set(opts, at_key, &JsValue::from_f64(u64_to_f64_checked(at)?))
            .map_err(opfs_platform::disk_error_from_js)?;
        Ok(())
    }

    fn disk_error_from_idb(err: StIdbError) -> DiskError {
        match err {
            StIdbError::IndexedDbUnavailable => DiskError::BackendUnavailable,
            StIdbError::QuotaExceeded => DiskError::QuotaExceeded,
            StIdbError::OutOfBounds {
                offset,
                len,
                capacity,
            } => DiskError::OutOfBounds {
                offset,
                len,
                capacity,
            },
            StIdbError::Corrupt(msg) => DiskError::Io(format!("indexeddb corrupt: {msg}")),
            StIdbError::UnsupportedFormat(version) => {
                DiskError::NotSupported(format!("unsupported indexeddb format version {version}"))
            }
            StIdbError::Js(err) => opfs_platform::disk_error_from_js(err),
        }
    }

    pub struct OpfsByteStorage {
        // Keep the `FileHandle` alive for the lifetime of the sync access handle.
        // (The handle itself is the only thing we actively use for IO.)
        _file: opfs_platform::FileHandle,
        handle: opfs_platform::SyncAccessHandle,
        at_key: JsValue,
        rw_opts: Object,
        closed: bool,
    }

    impl core::fmt::Debug for OpfsByteStorage {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("OpfsByteStorage")
                .field("closed", &self.closed)
                .finish_non_exhaustive()
        }
    }

    impl Drop for OpfsByteStorage {
        fn drop(&mut self) {
            if self.closed {
                return;
            }
            let _ = self.handle.flush();
            let _ = self.handle.close();
            self.closed = true;
        }
    }

    impl OpfsByteStorage {
        pub async fn open(path: &str, create: bool) -> DiskResult<Self> {
            if !opfs_platform::is_opfs_supported() {
                return Err(DiskError::NotSupported(
                    "OPFS is unavailable (navigator.storage.getDirectory missing)".to_string(),
                ));
            }

            let file = opfs_platform::open_file(path, create).await?;

            if !opfs_platform::is_worker_scope()
                || !opfs_platform::file_handle_supports_sync_access_handle(&file)
            {
                return Err(DiskError::NotSupported(
                    "OPFS sync access handles are unavailable; use OpfsAsyncBackend instead"
                        .to_string(),
                ));
            }

            let at_key = JsValue::from_str("at");
            let rw_opts = Object::new();
            Reflect::set(&rw_opts, &at_key, &JsValue::from_f64(0.0))
                .map_err(opfs_platform::disk_error_from_js)?;

            let handle = opfs_platform::create_sync_handle(&file).await?;

            Ok(Self {
                _file: file,
                handle,
                at_key,
                rw_opts,
                closed: false,
            })
        }

        pub fn is_closed(&self) -> bool {
            self.closed
        }

        pub fn close(&mut self) -> DiskResult<()> {
            if self.closed {
                return Ok(());
            }
            self.flush()?;
            self.handle
                .close()
                .map_err(opfs_platform::disk_error_from_js)?;
            self.closed = true;
            Ok(())
        }

        fn read_exact(&mut self, mut offset: u64, mut buf: &mut [u8]) -> DiskResult<()> {
            while !buf.is_empty() {
                set_at(&self.rw_opts, &self.at_key, offset)?;
                let cap = buf.len();
                let read =
                    self.handle
                        .read(buf, self.rw_opts.as_ref())
                        .map_err(opfs_platform::disk_error_from_js)? as usize;
                if read > cap {
                    return Err(DiskError::Io(format!(
                        "OPFS SyncAccessHandle.read returned {read} bytes for buffer len {cap}"
                    )));
                }
                if read == 0 {
                    return Err(DiskError::Io("short read (0 bytes)".to_string()));
                }
                offset += read as u64;
                buf = &mut buf[read..];
            }
            Ok(())
        }

        fn write_all(&mut self, mut offset: u64, mut buf: &[u8]) -> DiskResult<()> {
            while !buf.is_empty() {
                set_at(&self.rw_opts, &self.at_key, offset)?;
                let cap = buf.len();
                let wrote =
                    self.handle
                        .write(buf, self.rw_opts.as_ref())
                        .map_err(opfs_platform::disk_error_from_js)? as usize;
                if wrote > cap {
                    return Err(DiskError::Io(format!(
                        "OPFS SyncAccessHandle.write returned {wrote} bytes for buffer len {cap}"
                    )));
                }
                if wrote == 0 {
                    return Err(DiskError::Io("short write (0 bytes)".to_string()));
                }
                offset += wrote as u64;
                buf = &buf[wrote..];
            }
            Ok(())
        }

        fn ensure_capacity(&mut self, len: u64) -> DiskResult<()> {
            let current_size = js_number_to_u64_checked(
                self.handle
                    .get_size()
                    .map_err(opfs_platform::disk_error_from_js)?,
            )?;
            if len <= current_size {
                return Ok(());
            }
            self.handle
                .truncate(u64_to_f64_checked(len)?)
                .map_err(opfs_platform::disk_error_from_js)?;
            Ok(())
        }
    }

    impl OpfsByteStorage {
        pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            let end = offset
                .checked_add(buf.len() as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let size = self.len()?;
            if end > size {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: buf.len(),
                    capacity: size,
                });
            }
            self.read_exact(offset, buf)
        }

        pub fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            let end = offset
                .checked_add(buf.len() as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            self.ensure_capacity(end)?;
            self.write_all(offset, buf)
        }

        pub fn flush(&mut self) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            self.handle
                .flush()
                .map_err(opfs_platform::disk_error_from_js)?;
            Ok(())
        }

        pub fn len(&mut self) -> DiskResult<u64> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            js_number_to_u64_checked(
                self.handle
                    .get_size()
                    .map_err(opfs_platform::disk_error_from_js)?,
            )
        }

        pub fn is_empty(&mut self) -> DiskResult<bool> {
            Ok(self.len()? == 0)
        }

        pub fn set_len(&mut self, len: u64) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            self.handle
                .truncate(u64_to_f64_checked(len)?)
                .map_err(opfs_platform::disk_error_from_js)?;
            Ok(())
        }
    }

    impl aero_storage::StorageBackend for OpfsByteStorage {
        fn len(&mut self) -> aero_storage::Result<u64> {
            OpfsByteStorage::len(self)
        }

        fn set_len(&mut self, len: u64) -> aero_storage::Result<()> {
            OpfsByteStorage::set_len(self, len)
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            OpfsByteStorage::read_at(self, offset, buf)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
            OpfsByteStorage::write_at(self, offset, buf)
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            OpfsByteStorage::flush(self)
        }
    }

    pub struct OpfsBackend {
        file: opfs_platform::FileHandle,
        handle: opfs_platform::SyncAccessHandle,
        sector_size: u32,
        total_sectors: u64,
        size_bytes: u64,
        at_key: JsValue,
        rw_opts: Object,
        closed: bool,
    }

    impl core::fmt::Debug for OpfsBackend {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("OpfsBackend")
                .field("sector_size", &self.sector_size)
                .field("total_sectors", &self.total_sectors)
                .field("size_bytes", &self.size_bytes)
                .field("closed", &self.closed)
                .finish_non_exhaustive()
        }
    }

    impl Drop for OpfsBackend {
        fn drop(&mut self) {
            if self.closed {
                return;
            }
            let _ = self.handle.flush();
            let _ = self.handle.close();
            self.closed = true;
        }
    }

    impl OpfsBackend {
        pub fn mode(&self) -> OpfsBackendMode {
            OpfsBackendMode::SyncAccessHandle
        }

        pub fn is_closed(&self) -> bool {
            self.closed
        }

        pub fn close(&mut self) -> DiskResult<()> {
            if self.closed {
                return Ok(());
            }
            self.flush()?;
            self.handle
                .close()
                .map_err(opfs_platform::disk_error_from_js)?;
            self.closed = true;
            Ok(())
        }

        pub fn resize_bytes(&mut self, new_size: u64) -> DiskResult<()> {
            if !new_size.is_multiple_of(self.sector_size as u64) {
                return Err(DiskError::Io(format!(
                    "disk size {new_size} is not a multiple of sector size {}",
                    self.sector_size
                )));
            }
            self.handle
                .truncate(u64_to_f64_checked(new_size)?)
                .map_err(opfs_platform::disk_error_from_js)?;
            self.size_bytes = new_size;
            self.total_sectors = new_size / self.sector_size as u64;
            Ok(())
        }

        pub async fn reopen(&mut self) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            let _ = self.handle.close();
            self.handle = opfs_platform::create_sync_handle(&self.file).await?;
            Ok(())
        }

        pub async fn open(path: &str, create: bool, size_bytes: u64) -> DiskResult<Self> {
            Self::open_with_progress(path, create, size_bytes, None).await
        }

        pub async fn open_with_progress(
            path: &str,
            create: bool,
            size_bytes: u64,
            progress: Option<&js_sys::Function>,
        ) -> DiskResult<Self> {
            if !size_bytes.is_multiple_of(DEFAULT_SECTOR_SIZE as u64) {
                return Err(DiskError::Io(format!(
                    "disk size {size_bytes} is not a multiple of sector size {DEFAULT_SECTOR_SIZE}"
                )));
            }

            if !opfs_platform::is_opfs_supported() {
                return Err(DiskError::NotSupported(
                    "OPFS is unavailable (navigator.storage.getDirectory missing)".to_string(),
                ));
            }

            let file = opfs_platform::open_file(path, create).await?;

            if !opfs_platform::is_worker_scope()
                || !opfs_platform::file_handle_supports_sync_access_handle(&file)
            {
                return Err(DiskError::NotSupported(
                    "OPFS sync access handles are unavailable; use OpfsAsyncBackend instead"
                        .to_string(),
                ));
            }

            if let Some(cb) = progress {
                let _ = cb.call1(&JsValue::NULL, &JsValue::from_f64(0.0));
            }

            let at_key = JsValue::from_str("at");
            let rw_opts = Object::new();
            Reflect::set(&rw_opts, &at_key, &JsValue::from_f64(0.0))
                .map_err(opfs_platform::disk_error_from_js)?;

            let handle = opfs_platform::create_sync_handle(&file).await?;
            let mut backend = Self {
                file,
                handle,
                sector_size: DEFAULT_SECTOR_SIZE,
                total_sectors: 0,
                size_bytes: 0,
                at_key,
                rw_opts,
                closed: false,
            };

            let current_size = js_number_to_u64_checked(
                backend
                    .handle
                    .get_size()
                    .map_err(opfs_platform::disk_error_from_js)?,
            )?;
            backend.size_bytes = current_size;
            backend.total_sectors = current_size / DEFAULT_SECTOR_SIZE as u64;

            if current_size != size_bytes {
                backend.resize_bytes(size_bytes)?;
            }

            if let Some(cb) = progress {
                let _ = cb.call1(&JsValue::NULL, &JsValue::from_f64(1.0));
            }

            Ok(backend)
        }

        fn check_io_bounds(&self, lba: u64, len_bytes: usize) -> DiskResult<(u64, u64)> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }

            if !(len_bytes as u64).is_multiple_of(self.sector_size as u64) {
                return Err(DiskError::UnalignedLength {
                    len: len_bytes,
                    alignment: self.sector_size as usize,
                });
            }

            let offset = lba
                .checked_mul(self.sector_size as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let end = offset
                .checked_add(len_bytes as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if end > self.size_bytes {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: len_bytes,
                    capacity: self.size_bytes,
                });
            }

            Ok((offset, end))
        }

        fn read_exact(&mut self, mut offset: u64, mut buf: &mut [u8]) -> DiskResult<()> {
            while !buf.is_empty() {
                set_at(&self.rw_opts, &self.at_key, offset)?;
                let cap = buf.len();
                let read =
                    self.handle
                        .read(buf, self.rw_opts.as_ref())
                        .map_err(opfs_platform::disk_error_from_js)? as usize;
                if read > cap {
                    return Err(DiskError::Io(format!(
                        "OPFS SyncAccessHandle.read returned {read} bytes for buffer len {cap}"
                    )));
                }
                if read == 0 {
                    return Err(DiskError::Io("short read (0 bytes)".to_string()));
                }
                offset += read as u64;
                buf = &mut buf[read..];
            }
            Ok(())
        }

        fn write_all(&mut self, mut offset: u64, mut buf: &[u8]) -> DiskResult<()> {
            while !buf.is_empty() {
                set_at(&self.rw_opts, &self.at_key, offset)?;
                let cap = buf.len();
                let wrote =
                    self.handle
                        .write(buf, self.rw_opts.as_ref())
                        .map_err(opfs_platform::disk_error_from_js)? as usize;
                if wrote > cap {
                    return Err(DiskError::Io(format!(
                        "OPFS SyncAccessHandle.write returned {wrote} bytes for buffer len {cap}"
                    )));
                }
                if wrote == 0 {
                    return Err(DiskError::Io("short write (0 bytes)".to_string()));
                }
                offset += wrote as u64;
                buf = &buf[wrote..];
            }
            Ok(())
        }
    }

    impl OpfsBackend {
        pub fn sector_size(&self) -> u32 {
            self.sector_size
        }

        pub fn total_sectors(&self) -> u64 {
            self.total_sectors
        }

        pub fn size_bytes(&self) -> u64 {
            self.size_bytes
        }

        pub fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
            let (offset, _) = self.check_io_bounds(lba, buf.len())?;
            self.read_exact(offset, buf)
        }

        pub fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
            let (offset, _) = self.check_io_bounds(lba, buf.len())?;
            self.write_all(offset, buf)
        }

        pub fn flush(&mut self) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            self.handle
                .flush()
                .map_err(opfs_platform::disk_error_from_js)?;
            Ok(())
        }
    }

    impl aero_storage::VirtualDisk for OpfsBackend {
        fn capacity_bytes(&self) -> u64 {
            self.size_bytes
        }

        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }

            let end = offset
                .checked_add(buf.len() as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if end > self.size_bytes {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: buf.len(),
                    capacity: self.size_bytes,
                });
            }

            self.read_exact(offset, buf)
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }

            let end = offset
                .checked_add(buf.len() as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if end > self.size_bytes {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: buf.len(),
                    capacity: self.size_bytes,
                });
            }

            self.write_all(offset, buf)
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            OpfsBackend::flush(self)
        }
    }

    /// Async OPFS backend implemented using Promise-based APIs (`getFile` + `createWritable`).
    ///
    /// This backend is useful in environments where `FileSystemSyncAccessHandle` is unavailable
    /// (e.g. main thread). It is async-only and does not implement `aero_storage::VirtualDisk`.
    pub struct OpfsAsyncBackend {
        file: opfs_platform::FileHandle,
        writable: Option<opfs_platform::WritableStream>,
        sector_size: u32,
        total_sectors: u64,
        size_bytes: u64,
        closed: bool,
        dirty: bool,
    }

    impl core::fmt::Debug for OpfsAsyncBackend {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("OpfsAsyncBackend")
                .field("sector_size", &self.sector_size)
                .field("total_sectors", &self.total_sectors)
                .field("size_bytes", &self.size_bytes)
                .field("closed", &self.closed)
                .field("dirty", &self.dirty)
                .finish_non_exhaustive()
        }
    }

    impl OpfsAsyncBackend {
        pub fn mode(&self) -> OpfsBackendMode {
            OpfsBackendMode::AsyncOpfs
        }

        pub async fn open(path: &str, create: bool, size_bytes: u64) -> DiskResult<Self> {
            Self::open_with_progress(path, create, size_bytes, None).await
        }

        pub async fn open_with_progress(
            path: &str,
            create: bool,
            size_bytes: u64,
            progress: Option<&js_sys::Function>,
        ) -> DiskResult<Self> {
            if !size_bytes.is_multiple_of(DEFAULT_SECTOR_SIZE as u64) {
                return Err(DiskError::Io(format!(
                    "disk size {size_bytes} is not a multiple of sector size {DEFAULT_SECTOR_SIZE}"
                )));
            }

            if !opfs_platform::is_opfs_supported() {
                return Err(DiskError::NotSupported(
                    "OPFS is unavailable (navigator.storage.getDirectory missing)".to_string(),
                ));
            }

            if let Some(cb) = progress {
                let _ = cb.call1(&JsValue::NULL, &JsValue::from_f64(0.0));
            }

            let file = opfs_platform::open_file(path, create).await?;
            let file_obj = opfs_platform::get_file_obj(&file).await?;
            let current_size = file_obj.size() as u64;
            if current_size != size_bytes {
                let stream = opfs_platform::create_writable_stream(&file, true).await?;
                opfs_platform::writable_truncate(&stream, u64_to_f64_checked(size_bytes)?).await?;
                opfs_platform::writable_close(&stream).await?;
            }

            if let Some(cb) = progress {
                let _ = cb.call1(&JsValue::NULL, &JsValue::from_f64(1.0));
            }

            Ok(Self {
                file,
                writable: None,
                sector_size: DEFAULT_SECTOR_SIZE,
                total_sectors: size_bytes / DEFAULT_SECTOR_SIZE as u64,
                size_bytes,
                closed: false,
                dirty: false,
            })
        }

        fn check_io_bounds(&self, lba: u64, len_bytes: usize) -> DiskResult<(u64, u64)> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }

            if !(len_bytes as u64).is_multiple_of(self.sector_size as u64) {
                return Err(DiskError::UnalignedLength {
                    len: len_bytes,
                    alignment: self.sector_size as usize,
                });
            }

            let offset = lba
                .checked_mul(self.sector_size as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let end = offset
                .checked_add(len_bytes as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if end > self.size_bytes {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: len_bytes,
                    capacity: self.size_bytes,
                });
            }

            Ok((offset, end))
        }

        async fn ensure_writable(&mut self) -> DiskResult<&opfs_platform::WritableStream> {
            if self.writable.is_none() {
                self.writable =
                    Some(opfs_platform::create_writable_stream(&self.file, true).await?);
            }
            Ok(self.writable.as_ref().expect("writable just set"))
        }

        async fn flush_writable_if_dirty(&mut self) -> DiskResult<()> {
            if !self.dirty {
                return Ok(());
            }
            if let Some(stream) = self.writable.take() {
                opfs_platform::writable_close(&stream).await?;
            }
            self.dirty = false;
            Ok(())
        }

        pub async fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
            let (offset, end) = self.check_io_bounds(lba, buf.len())?;
            self.flush_writable_if_dirty().await?;

            if buf.is_empty() {
                return Ok(());
            }

            let file_obj = opfs_platform::get_file_obj(&self.file).await?;
            let start = u64_to_f64_checked(offset)?;
            let end = u64_to_f64_checked(end)?;
            let blob = file_obj
                .slice_with_f64_and_f64(start, end)
                .map_err(opfs_platform::disk_error_from_js)?;

            let ab = wasm_bindgen_futures::JsFuture::from(blob.array_buffer())
                .await
                .map_err(opfs_platform::disk_error_from_js)?;
            let arr = Uint8Array::new(&ab);
            let got = arr.length() as usize;
            if got != buf.len() {
                return Err(DiskError::Io(format!(
                    "short read from OPFS async backend: expected {} bytes got {got}",
                    buf.len()
                )));
            }
            arr.copy_to(buf);
            Ok(())
        }

        pub async fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
            let (offset, _) = self.check_io_bounds(lba, buf.len())?;

            let stream = self.ensure_writable().await?.clone();
            opfs_platform::writable_seek(&stream, u64_to_f64_checked(offset)?).await?;
            let data = unsafe { Uint8Array::view(buf) };
            opfs_platform::writable_write(&stream, &data.into()).await?;
            self.dirty = true;
            Ok(())
        }

        pub async fn flush(&mut self) -> DiskResult<()> {
            self.flush_writable_if_dirty().await
        }

        pub async fn close(&mut self) -> DiskResult<()> {
            if self.closed {
                return Ok(());
            }
            self.flush().await?;
            self.closed = true;
            Ok(())
        }
    }

    /// IndexedDB-backed block/sector storage fallback for browser environments.
    ///
    /// This backend uses the `st-idb` crate and is **async-only** (Promise-based). It exists
    /// primarily as a portability fallback when OPFS APIs are unavailable.
    ///
    /// IMPORTANT: IndexedDB cannot back `aero_storage::StorageBackend` / `aero_storage::VirtualDisk`
    /// (used by Aero's synchronous Rust AHCI/IDE controller path) in the same Worker, because
    /// IndexedDB does not provide synchronous read/write semantics.
    ///
    /// See `docs/19-indexeddb-storage-story.md` and `docs/20-storage-trait-consolidation.md`.
    ///
    /// As a guardrail, this type intentionally does **not** implement
    /// [`aero_storage::StorageBackend`]:
    ///
    /// ```compile_fail,E0277
    /// use aero_storage::StorageBackend;
    /// use aero_opfs::io::storage::backends::opfs::OpfsIndexedDbBackend;
    ///
    /// fn assert_sync_backend<T: StorageBackend>() {}
    ///
    /// assert_sync_backend::<OpfsIndexedDbBackend>();
    /// ```
    pub struct OpfsIndexedDbBackend {
        inner: StIndexedDbBackend,
        sector_size: u32,
        total_sectors: u64,
        size_bytes: u64,
        closed: bool,
    }

    impl core::fmt::Debug for OpfsIndexedDbBackend {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("OpfsIndexedDbBackend")
                .field("sector_size", &self.sector_size)
                .field("total_sectors", &self.total_sectors)
                .field("size_bytes", &self.size_bytes)
                .field("closed", &self.closed)
                .finish_non_exhaustive()
        }
    }

    impl OpfsIndexedDbBackend {
        pub fn mode(&self) -> OpfsBackendMode {
            OpfsBackendMode::IndexedDb
        }

        pub async fn open(db_name: &str, size_bytes: u64) -> DiskResult<Self> {
            if !size_bytes.is_multiple_of(DEFAULT_SECTOR_SIZE as u64) {
                return Err(DiskError::Io(format!(
                    "disk size {size_bytes} is not a multiple of sector size {DEFAULT_SECTOR_SIZE}"
                )));
            }

            let inner =
                StIndexedDbBackend::open(db_name, size_bytes, IndexedDbBackendOptions::default())
                    .await
                    .map_err(disk_error_from_idb)?;

            Ok(Self {
                inner,
                sector_size: DEFAULT_SECTOR_SIZE,
                total_sectors: size_bytes / DEFAULT_SECTOR_SIZE as u64,
                size_bytes,
                closed: false,
            })
        }

        fn check_io_bounds(&self, lba: u64, len_bytes: usize) -> DiskResult<(u64, u64)> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }

            if !(len_bytes as u64).is_multiple_of(self.sector_size as u64) {
                return Err(DiskError::UnalignedLength {
                    len: len_bytes,
                    alignment: self.sector_size as usize,
                });
            }

            let offset = lba
                .checked_mul(self.sector_size as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let end = offset
                .checked_add(len_bytes as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if end > self.size_bytes {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: len_bytes,
                    capacity: self.size_bytes,
                });
            }

            Ok((offset, end))
        }

        pub async fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
            let (offset, _) = self.check_io_bounds(lba, buf.len())?;
            self.inner
                .read_at(offset, buf)
                .await
                .map_err(disk_error_from_idb)
        }

        pub async fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
            let (offset, _) = self.check_io_bounds(lba, buf.len())?;
            self.inner
                .write_at(offset, buf)
                .await
                .map_err(disk_error_from_idb)
        }

        pub async fn flush(&mut self) -> DiskResult<()> {
            if self.closed {
                return Err(DiskError::InvalidState(
                    "backend already closed".to_string(),
                ));
            }
            self.inner.flush().await.map_err(disk_error_from_idb)
        }

        pub async fn close(&mut self) -> DiskResult<()> {
            if self.closed {
                return Ok(());
            }
            self.flush().await?;
            self.closed = true;
            Ok(())
        }
    }

    #[derive(Debug)]
    /// Convenience wrapper that selects the best available browser persistence backend.
    ///
    /// The `Sync` variant uses `FileSystemSyncAccessHandle` and is suitable for synchronous
    /// disk/controller paths. The `Async` and `IndexedDb` variants are async-only.
    pub enum OpfsStorage {
        Sync(OpfsBackend),
        Async(OpfsAsyncBackend),
        /// Async IndexedDB-backed storage fallback.
        ///
        /// NOTE: This is *not* suitable for the boot-critical synchronous Rust
        /// storage/controller stack (`aero_storage::VirtualDisk` + AHCI/IDE). It
        /// should only be used by async paths.
        IndexedDb(OpfsIndexedDbBackend),
    }

    impl OpfsStorage {
        pub fn mode(&self) -> OpfsBackendMode {
            match self {
                Self::Sync(_) => OpfsBackendMode::SyncAccessHandle,
                Self::Async(_) => OpfsBackendMode::AsyncOpfs,
                Self::IndexedDb(_) => OpfsBackendMode::IndexedDb,
            }
        }

        /// Open a browser persistence backend, selecting the best available mode.
        ///
        /// # Warning: may return async-only backends
        ///
        /// This function may fall back to [`OpfsAsyncBackend`] or [`OpfsIndexedDbBackend`] when
        /// `FileSystemSyncAccessHandle` is unavailable (e.g. main thread, missing browser support).
        /// Those variants are **async-only** and do **not** implement the synchronous
        /// `aero_storage::{StorageBackend, VirtualDisk}` traits used by the boot-critical Rust
        /// controller path.
        ///
        /// If you *require* a synchronous backend (to back `aero-storage` disk images/controllers),
        /// call [`OpfsBackend::open`] / [`OpfsByteStorage::open`] directly and handle
        /// `DiskError::NotSupported` instead of assuming `OpfsStorage::open(...).await.into_sync()`
        /// will succeed.
        ///
        /// See `docs/20-storage-trait-consolidation.md` and `docs/19-indexeddb-storage-story.md`.
        pub async fn open(path: &str, create: bool, size_bytes: u64) -> DiskResult<Self> {
            match OpfsBackend::open(path, create, size_bytes).await {
                Ok(backend) => Ok(Self::Sync(backend)),
                Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => {
                    match OpfsAsyncBackend::open(path, create, size_bytes).await {
                        Ok(backend) => Ok(Self::Async(backend)),
                        Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => {
                            Ok(Self::IndexedDb(
                                OpfsIndexedDbBackend::open(
                                    &format!("aero-opfs:{path}"),
                                    size_bytes,
                                )
                                .await?,
                            ))
                        }
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }

        /// Extract the synchronous OPFS backend, if present.
        ///
        /// # Warning
        ///
        /// [`OpfsStorage::open`] may fall back to async-only variants when sync access handles are
        /// unavailable. Callers that require a synchronous backend (to back
        /// `aero_storage::{StorageBackend, VirtualDisk}`) should not assume this returns `Some`.
        /// Prefer calling [`OpfsBackend::open`] directly and handling `DiskError::NotSupported`.
        pub fn into_sync(self) -> Option<OpfsBackend> {
            match self {
                Self::Sync(backend) => Some(backend),
                Self::Async(_) | Self::IndexedDb(_) => None,
            }
        }

        pub fn into_async(self) -> Option<OpfsAsyncBackend> {
            match self {
                Self::Sync(_) | Self::IndexedDb(_) => None,
                Self::Async(backend) => Some(backend),
            }
        }

        pub fn into_indexeddb(self) -> Option<OpfsIndexedDbBackend> {
            match self {
                Self::IndexedDb(backend) => Some(backend),
                Self::Sync(_) | Self::Async(_) => None,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use aero_storage::{AeroSparseConfig, AeroSparseDisk, VirtualDisk as _};
        use wasm_bindgen_test::wasm_bindgen_test;

        fn unique_path(prefix: &str) -> String {
            let now = js_sys::Date::now() as u64;
            format!("tests/{prefix}-{now}.img")
        }

        fn unique_aerospar_path(prefix: &str) -> String {
            let now = js_sys::Date::now() as u64;
            format!("tests/{prefix}-{now}.aerospar")
        }

        fn fill_deterministic(buf: &mut [u8], seed: u32) {
            let mut x = seed;
            for b in buf {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                *b = (x & 0xff) as u8;
            }
        }

        async fn write_sectors(storage: &mut OpfsStorage, lba: u64, buf: &[u8]) {
            match storage {
                OpfsStorage::Sync(backend) => backend.write_sectors(lba, buf).unwrap(),
                OpfsStorage::Async(backend) => backend.write_sectors(lba, buf).await.unwrap(),
                OpfsStorage::IndexedDb(backend) => backend.write_sectors(lba, buf).await.unwrap(),
            }
        }

        async fn read_sectors(storage: &mut OpfsStorage, lba: u64, buf: &mut [u8]) {
            match storage {
                OpfsStorage::Sync(backend) => backend.read_sectors(lba, buf).unwrap(),
                OpfsStorage::Async(backend) => backend.read_sectors(lba, buf).await.unwrap(),
                OpfsStorage::IndexedDb(backend) => backend.read_sectors(lba, buf).await.unwrap(),
            }
        }

        async fn flush(storage: &mut OpfsStorage) {
            match storage {
                OpfsStorage::Sync(backend) => backend.flush().unwrap(),
                OpfsStorage::Async(backend) => backend.flush().await.unwrap(),
                OpfsStorage::IndexedDb(backend) => backend.flush().await.unwrap(),
            }
        }

        #[wasm_bindgen_test(async)]
        async fn opfs_roundtrip_small() {
            let path = unique_path("roundtrip");
            let size = 8 * 1024 * 1024u64;

            let mut backend = match OpfsStorage::open(&path, true, size).await {
                Ok(b) => b,
                Err(DiskError::NotSupported(_)) => return,
                Err(DiskError::BackendUnavailable) => return,
                Err(e) => panic!("open failed: {e:?}"),
            };

            let lba = 7u64;
            let mut write_buf = vec![0u8; 4096];
            fill_deterministic(&mut write_buf, 0x1234_5678);
            write_sectors(&mut backend, lba, &write_buf).await;
            flush(&mut backend).await;

            let mut backend = OpfsStorage::open(&path, false, size).await.unwrap();
            let mut read_buf = vec![0u8; 4096];
            read_sectors(&mut backend, lba, &mut read_buf).await;
            assert_eq!(read_buf, write_buf);
        }

        #[wasm_bindgen_test(async)]
        async fn opfs_large_offset_over_2gb() {
            let path = unique_path("large-offset");
            let size = 2 * 1024 * 1024 * 1024u64 + 16 * 1024 * 1024;

            let mut backend = match OpfsStorage::open(&path, true, size).await {
                Ok(b) => b,
                Err(DiskError::NotSupported(_)) => return,
                Err(DiskError::QuotaExceeded) => return,
                Err(DiskError::BackendUnavailable) => return,
                Err(e) => panic!("open failed: {e:?}"),
            };

            let write_offset = 2 * 1024 * 1024 * 1024u64 + 4 * 1024 * 1024;
            let lba = write_offset / 512;

            let mut write_buf = vec![0u8; 8192];
            fill_deterministic(&mut write_buf, 0xA5A5_5A5A);
            write_sectors(&mut backend, lba, &write_buf).await;
            flush(&mut backend).await;

            let mut backend = OpfsStorage::open(&path, false, size).await.unwrap();
            let mut read_buf = vec![0u8; 8192];
            read_sectors(&mut backend, lba, &mut read_buf).await;
            assert_eq!(read_buf, write_buf);
        }

        #[wasm_bindgen_test(async)]
        async fn opfs_aerospar_roundtrip() {
            let path = unique_aerospar_path("aerospar-roundtrip");

            let storage = match OpfsByteStorage::open(&path, true).await {
                Ok(s) => s,
                Err(DiskError::NotSupported(_)) => return,
                Err(DiskError::QuotaExceeded) => return,
                Err(DiskError::BackendUnavailable) => return,
                Err(e) => panic!("open failed: {e:?}"),
            };

            let mut disk = AeroSparseDisk::create(
                storage,
                AeroSparseConfig {
                    disk_size_bytes: 1024 * 1024,
                    block_size_bytes: 32 * 1024,
                },
            )
            .unwrap();

            let mut write_buf = vec![0u8; 4096];
            fill_deterministic(&mut write_buf, 0x55AA_1234);
            disk.write_sectors(7, &write_buf).unwrap();
            disk.flush().unwrap();

            let mut storage = disk.into_backend();
            storage.close().unwrap();

            let storage = OpfsByteStorage::open(&path, false).await.unwrap();
            let mut disk = AeroSparseDisk::open(storage).unwrap();
            let mut read_buf = vec![0u8; write_buf.len()];
            disk.read_sectors(7, &mut read_buf).unwrap();
            assert_eq!(read_buf, write_buf);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{OpfsAsyncBackend, OpfsBackend, OpfsByteStorage, OpfsIndexedDbBackend, OpfsStorage};

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;

    #[derive(Debug)]
    pub struct OpfsBackend;

    impl OpfsBackend {
        pub fn mode(&self) -> OpfsBackendMode {
            OpfsBackendMode::AsyncOpfs
        }

        pub async fn open(_path: &str, _create: bool, _size_bytes: u64) -> DiskResult<Self> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn sector_size(&self) -> u32 {
            512
        }

        pub fn total_sectors(&self) -> u64 {
            0
        }

        pub fn size_bytes(&self) -> u64 {
            0
        }

        pub fn read_sectors(&mut self, _lba: u64, _buf: &mut [u8]) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn write_sectors(&mut self, _lba: u64, _buf: &[u8]) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn flush(&mut self) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }
    }

    impl aero_storage::VirtualDisk for OpfsBackend {
        fn capacity_bytes(&self) -> u64 {
            0
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }
    }

    #[derive(Debug)]
    pub struct OpfsAsyncBackend;

    #[derive(Debug)]
    pub struct OpfsByteStorage;

    impl OpfsByteStorage {
        pub async fn open(_path: &str, _create: bool) -> DiskResult<Self> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn is_closed(&self) -> bool {
            true
        }

        pub fn close(&mut self) -> DiskResult<()> {
            Ok(())
        }

        pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn flush(&mut self) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn len(&mut self) -> DiskResult<u64> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        pub fn is_empty(&mut self) -> DiskResult<bool> {
            Ok(self.len()? == 0)
        }

        pub fn set_len(&mut self, _len: u64) -> DiskResult<()> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }
    }

    impl aero_storage::StorageBackend for OpfsByteStorage {
        fn len(&mut self) -> aero_storage::Result<u64> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn set_len(&mut self, _len: u64) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }

        fn flush(&mut self) -> aero_storage::Result<()> {
            Err(aero_storage::DiskError::NotSupported(
                "OPFS is wasm-only".to_string(),
            ))
        }
    }

    /// Stub for [`OpfsIndexedDbBackend`] on non-wasm targets.
    ///
    /// The real IndexedDB implementation is wasm32-only. This stub exists so the crate can
    /// compile on non-wasm platforms.
    ///
    /// IndexedDB is async-only and this type intentionally does **not** implement
    /// [`aero_storage::StorageBackend`]. See:
    ///
    /// - `docs/19-indexeddb-storage-story.md`
    /// - `docs/20-storage-trait-consolidation.md`
    ///
    /// ```compile_fail,E0277
    /// use aero_storage::StorageBackend;
    /// use aero_opfs::io::storage::backends::opfs::OpfsIndexedDbBackend;
    ///
    /// fn assert_sync_backend<T: StorageBackend>() {}
    ///
    /// assert_sync_backend::<OpfsIndexedDbBackend>();
    /// ```
    #[derive(Debug)]
    pub struct OpfsIndexedDbBackend;

    #[derive(Debug)]
    pub enum OpfsStorage {
        Sync(OpfsBackend),
        Async(OpfsAsyncBackend),
        IndexedDb(OpfsIndexedDbBackend),
    }

    impl OpfsStorage {
        pub fn mode(&self) -> OpfsBackendMode {
            match self {
                Self::Sync(_) => OpfsBackendMode::SyncAccessHandle,
                Self::Async(_) => OpfsBackendMode::AsyncOpfs,
                Self::IndexedDb(_) => OpfsBackendMode::IndexedDb,
            }
        }

        /// Open a browser persistence backend, selecting the best available mode.
        ///
        /// See the wasm32 implementation for details and warnings about async-only fallbacks.
        pub async fn open(_path: &str, _create: bool, _size_bytes: u64) -> DiskResult<Self> {
            Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
        }

        /// Extract the synchronous OPFS backend, if present.
        pub fn into_sync(self) -> Option<OpfsBackend> {
            match self {
                Self::Sync(backend) => Some(backend),
                Self::Async(_) | Self::IndexedDb(_) => None,
            }
        }

        pub fn into_async(self) -> Option<OpfsAsyncBackend> {
            match self {
                Self::Async(backend) => Some(backend),
                Self::Sync(_) | Self::IndexedDb(_) => None,
            }
        }

        pub fn into_indexeddb(self) -> Option<OpfsIndexedDbBackend> {
            match self {
                Self::IndexedDb(backend) => Some(backend),
                Self::Sync(_) | Self::Async(_) => None,
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    OpfsAsyncBackend, OpfsBackend, OpfsByteStorage, OpfsIndexedDbBackend, OpfsStorage,
};
