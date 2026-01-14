use crate::{DiskError, Result};

#[cfg(not(target_arch = "wasm32"))]
use std::fs::{File, OpenOptions};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

#[cfg(all(not(target_arch = "wasm32"), unix))]
use std::os::unix::fs::FileExt;
#[cfg(all(not(target_arch = "wasm32"), windows))]
use std::os::windows::fs::FileExt;

/// A resizable, byte-addressed backing store for disk images.
///
/// In the browser this is typically implemented by OPFS `FileSystemSyncAccessHandle`
/// (fast, synchronous in a Worker).
///
/// A concrete wasm32 implementation is provided by the `aero-opfs` crate (imported as
/// `aero_opfs` in Rust code):
///
/// ```text
/// aero_opfs::OpfsByteStorage
/// ```
///
/// **Important (wasm32):** this trait is intentionally *synchronous* because it is used by
/// `aero_storage::VirtualDisk` and the Rust device/controller stack (e.g. AHCI/IDE).
///
/// Errors are reported via [`DiskError`]. In particular, [`DiskError::Io`] stores a plain
/// `String` so wasm32 implementations can propagate errors originating from JavaScript/DOM APIs.
///
/// Browser IndexedDB APIs are Promise-based (async) and therefore cannot implement this trait
/// safely in the *same* Worker thread. Supporting IndexedDB would require an explicit split
/// (separate storage worker + RPC/serialization layer), which is currently out of scope.
///
/// For host-layer IndexedDB storage/caching, see the async `st-idb` crate.
///
/// This trait also allows the pure Rust disk image formats to be unit-tested without any
/// browser APIs.
///
/// See `docs/20-storage-trait-consolidation.md` for the repo-wide trait consolidation plan.
pub trait StorageBackend {
    /// Current length in bytes.
    fn len(&mut self) -> Result<u64>;

    fn is_empty(&mut self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Resize to `len` bytes.
    fn set_len(&mut self, len: u64) -> Result<()>;

    /// Read exactly `buf.len()` bytes at `offset`.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Write all `buf.len()` bytes at `offset` (extending the backend if required).
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()>;

    /// Flush any buffered state to durable storage.
    fn flush(&mut self) -> Result<()>;
}

impl<T: StorageBackend + ?Sized> StorageBackend for &mut T {
    fn len(&mut self) -> Result<u64> {
        (**self).len()
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        (**self).set_len(len)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        (**self).write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<()> {
        (**self).flush()
    }
}

impl<T: StorageBackend + ?Sized> StorageBackend for Box<T> {
    fn len(&mut self) -> Result<u64> {
        (**self).len()
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        (**self).set_len(len)
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        (**self).write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<()> {
        (**self).flush()
    }
}

/// Read-only wrapper for a [`StorageBackend`].
///
/// This is useful for enforcing safety (e.g. opening ISO images or base layers) where the caller
/// must not be able to resize or write to the underlying backing store.
pub struct ReadOnlyBackend<B> {
    inner: B,
}

impl<B> ReadOnlyBackend<B> {
    #[must_use]
    pub fn new(inner: B) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn inner(&self) -> &B {
        &self.inner
    }

    #[must_use]
    pub fn into_inner(self) -> B {
        self.inner
    }
}

impl<B: StorageBackend> StorageBackend for ReadOnlyBackend<B> {
    fn len(&mut self) -> Result<u64> {
        self.inner.len()
    }

    fn set_len(&mut self, _len: u64) -> Result<()> {
        Err(DiskError::NotSupported("read-only".into()))
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> Result<()> {
        Err(DiskError::NotSupported("read-only".into()))
    }

    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

/// In-memory storage backend used for tests and benchmarks.
#[derive(Clone, Debug, Default)]
pub struct MemBackend {
    data: Vec<u8>,
}

impl MemBackend {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// Construct a `MemBackend` from an existing byte vector without copying.
    #[must_use]
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Consume the backend and return the underlying byte vector.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }

    pub fn with_len(len: u64) -> Result<Self> {
        let len_usize: usize = len.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let mut data = Vec::new();
        data.try_reserve_exact(len_usize)
            .map_err(|_| DiskError::QuotaExceeded)?;
        data.resize(len_usize, 0);
        Ok(Self { data })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl StorageBackend for MemBackend {
    fn len(&mut self) -> Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        let len_usize: usize = len.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let cur_len = self.data.len();
        if len_usize > cur_len {
            self.data
                .try_reserve_exact(len_usize - cur_len)
                .map_err(|_| DiskError::QuotaExceeded)?;
        }
        self.data.resize(len_usize, 0);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset_usize: usize = offset.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset_usize
            .checked_add(buf.len())
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.data.len() {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity: self.data.len() as u64,
            });
        }
        buf.copy_from_slice(&self.data[offset_usize..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        let offset_usize: usize = offset.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset_usize
            .checked_add(buf.len())
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.data.len() {
            let end_u64 = u64::try_from(end).map_err(|_| DiskError::OffsetOverflow)?;
            self.set_len(end_u64)?;
        }
        self.data[offset_usize..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Native `std::fs::File`-backed storage backend.
///
/// This backend is available on non-wasm32 targets and is intended for host-side tooling
/// (conversion, inspection, regression tests, etc.).
///
/// I/O is performed using platform-specific `FileExt` offset methods (`read_at`/`write_at` on
/// Unix, `seek_read`/`seek_write` on Windows) so the OS file cursor is not disturbed.
///
/// When opened in read-only mode, [`StorageBackend::write_at`] and [`StorageBackend::set_len`]
/// return [`DiskError::NotSupported`] ("read-only backend"). [`StorageBackend::flush`] is a no-op
/// for read-only handles.
///
/// `flush()` uses [`File::sync_all`] (data + metadata). This is the safest default for disk
/// images, especially when writes may extend the file length.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub struct StdFileBackend {
    file: File,
    path: Option<PathBuf>,
    read_only: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl StdFileBackend {
    /// Open an existing file.
    pub fn open<P: AsRef<Path>>(path: P, read_only: bool) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(path_ref)
            .map_err(|e| {
                DiskError::Io(format!(
                    "failed to open file (path={} read_only={}): {e}",
                    path_ref.display(),
                    read_only
                ))
            })?;
        Ok(Self {
            file,
            path: Some(path_ref.to_path_buf()),
            read_only,
        })
    }

    /// Open an existing file read-only.
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open(path, true)
    }

    /// Open an existing file for reading and writing.
    pub fn open_rw<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open(path, false)
    }

    /// Create/truncate a file and set its length to `size` bytes.
    pub fn create<P: AsRef<Path>>(path: P, size: u64) -> Result<Self> {
        let path_ref = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path_ref)
            .map_err(|e| {
                DiskError::Io(format!(
                    "failed to create file (path={} size={}): {e}",
                    path_ref.display(),
                    size
                ))
            })?;

        let mut backend = Self {
            file,
            path: Some(path_ref.to_path_buf()),
            read_only: false,
        };
        backend.set_len(size)?;
        Ok(backend)
    }

    /// Consume the backend and return the underlying [`File`].
    #[must_use]
    pub fn into_file(self) -> File {
        self.file
    }

    /// Wrap an already-open [`std::fs::File`].
    ///
    /// Note: the returned backend has no path metadata (used only for error messages).
    #[must_use]
    pub fn from_file(file: File) -> Self {
        Self {
            file,
            path: None,
            read_only: false,
        }
    }

    /// Wrap an already-open [`std::fs::File`] and attach a display path for error messages.
    #[must_use]
    pub fn from_file_with_path<P: AsRef<Path>>(file: File, path: P) -> Self {
        Self {
            file,
            path: Some(path.as_ref().to_path_buf()),
            read_only: false,
        }
    }

    /// Configure whether this backend should treat the underlying file as read-only.
    ///
    /// This only affects the pre-flight checks in [`StorageBackend::set_len`] and
    /// [`StorageBackend::write_at`]; it does **not** change OS-level file permissions.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn path_str(&self) -> String {
        self.path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    fn io_err(&self, op: &str, err: std::io::Error) -> DiskError {
        DiskError::Io(format!("{op} failed (path={}): {err}", self.path_str()))
    }

    fn io_err_at(&self, op: &str, offset: u64, len: usize, err: std::io::Error) -> DiskError {
        DiskError::Io(format!(
            "{op} failed (path={} offset={} len={}): {err}",
            self.path_str(),
            offset,
            len
        ))
    }

    #[cfg(unix)]
    fn pread(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        self.file.read_at(buf, offset)
    }

    #[cfg(windows)]
    fn pread(&self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        self.file.seek_read(buf, offset)
    }

    #[cfg(unix)]
    fn pwrite(&self, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        self.file.write_at(buf, offset)
    }

    #[cfg(windows)]
    fn pwrite(&self, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        self.file.seek_write(buf, offset)
    }

    #[cfg(not(any(unix, windows)))]
    fn pread(&mut self, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buf)
    }

    #[cfg(not(any(unix, windows)))]
    fn pwrite(&mut self, buf: &[u8], offset: u64) -> std::io::Result<usize> {
        use std::io::{Seek, SeekFrom, Write};
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write(buf)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<File> for StdFileBackend {
    fn from(file: File) -> Self {
        Self::from_file(file)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl StorageBackend for StdFileBackend {
    fn len(&mut self) -> Result<u64> {
        self.file
            .metadata()
            .map(|m| m.len())
            .map_err(|e| self.io_err("metadata", e))
    }

    fn set_len(&mut self, len: u64) -> Result<()> {
        if self.read_only {
            return Err(DiskError::NotSupported("read-only backend".to_string()));
        }
        self.file.set_len(len).map_err(|e| {
            DiskError::Io(format!(
                "set_len failed (path={} len={}): {e}",
                self.path_str(),
                len
            ))
        })?;
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let len_u64 = u64::try_from(buf.len()).map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(len_u64)
            .ok_or(DiskError::OffsetOverflow)?;

        let capacity = self.len()?;
        if end > capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: buf.len(),
                capacity,
            });
        }

        let mut read = 0usize;
        while read < buf.len() {
            let off = offset
                .checked_add(u64::try_from(read).map_err(|_| DiskError::OffsetOverflow)?)
                .ok_or(DiskError::OffsetOverflow)?;
            let n = loop {
                match self.pread(&mut buf[read..], off) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(self.io_err_at("read_at", off, buf.len() - read, e));
                    }
                }
            };
            if n == 0 {
                // For regular files this typically indicates EOF (short read).
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: buf.len(),
                    capacity: self.len().unwrap_or(capacity),
                });
            }
            read = read.checked_add(n).ok_or(DiskError::OffsetOverflow)?;
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(DiskError::NotSupported("read-only backend".to_string()));
        }
        let len_u64 = u64::try_from(buf.len()).map_err(|_| DiskError::OffsetOverflow)?;
        let end = offset
            .checked_add(len_u64)
            .ok_or(DiskError::OffsetOverflow)?;

        let capacity = self.len()?;
        if end > capacity {
            self.set_len(end)?;
        }

        let mut written = 0usize;
        while written < buf.len() {
            let off = offset
                .checked_add(u64::try_from(written).map_err(|_| DiskError::OffsetOverflow)?)
                .ok_or(DiskError::OffsetOverflow)?;
            let n = loop {
                match self.pwrite(&buf[written..], off) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        return Err(self.io_err_at("write_at", off, buf.len() - written, e));
                    }
                }
            };
            if n == 0 {
                return Err(DiskError::Io(format!(
                    "write_at wrote 0 bytes (path={} offset={} remaining={})",
                    self.path_str(),
                    off,
                    buf.len() - written
                )));
            }
            written = written.checked_add(n).ok_or(DiskError::OffsetOverflow)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // On some platforms (notably Windows), syncing a file handle opened without write access
        // may fail with a permission error. Since this backend performs no writes in read-only
        // mode, treat flush as a no-op for read-only handles so higher-level abstractions (e.g.
        // COW base disks) can call `flush()` unconditionally.
        if self.read_only {
            return Ok(());
        }
        self.file.sync_all().map_err(|e| self.io_err("sync_all", e))
    }
}

/// Alias for [`StdFileBackend`].
///
/// This exists primarily for ergonomic call sites where `FileBackend` reads better than
/// `StdFileBackend`, especially in native tooling.
#[cfg(not(target_arch = "wasm32"))]
pub type FileBackend = StdFileBackend;
