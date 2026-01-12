use crate::{DiskError, Result};

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
