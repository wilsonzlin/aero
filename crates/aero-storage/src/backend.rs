use crate::{DiskError, Result};

/// A resizable, byte-addressed backing store for disk images.
///
/// In the browser this is typically implemented by OPFS `FileSystemSyncAccessHandle`
/// (fast, synchronous in a Worker).
///
/// A concrete wasm32 implementation is provided by the `aero-opfs` crate:
///
/// ```text
/// aero_opfs::OpfsByteStorage
/// ```
///
/// IndexedDB-backed storage is generally async and therefore does not currently implement
/// this synchronous trait.
///
/// This trait also allows the pure Rust disk image formats to be unit-tested without any
/// browser APIs.
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

    pub fn with_len(len: u64) -> Result<Self> {
        let len_usize: usize = len.try_into().map_err(|_| DiskError::OffsetOverflow)?;
        Ok(Self {
            data: vec![0; len_usize],
        })
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
            self.data.resize(end, 0);
        }
        self.data[offset_usize..end].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
