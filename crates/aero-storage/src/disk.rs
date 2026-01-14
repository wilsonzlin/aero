use crate::util::checked_range;
use crate::{DiskError, Result, StorageBackend};

pub const SECTOR_SIZE: usize = 512;

/// Internal helper trait: conditionally requires `Send` depending on the target.
///
/// On native targets, disk backends are frequently moved across threads (worker pools, async
/// runtimes). On wasm32, backends may wrap JS/OPFS handles and are therefore often `!Send`, so we
/// intentionally omit the `Send` bound there.
#[cfg(not(target_arch = "wasm32"))]
pub trait VirtualDiskSend: Send {}
#[cfg(target_arch = "wasm32")]
pub trait VirtualDiskSend {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> VirtualDiskSend for T {}
#[cfg(target_arch = "wasm32")]
impl<T> VirtualDiskSend for T {}

/// A fixed-capacity virtual disk.
///
/// Implementations are byte-addressed (`read_at` / `write_at`) for easy composition with
/// block caches and sparse formats, but the emulator-facing API is *sector-based* via
/// `read_sectors` / `write_sectors`.
///
/// See `docs/20-storage-trait-consolidation.md` for the repo-wide trait consolidation plan.
pub trait VirtualDisk: VirtualDiskSend {
    /// Disk capacity in bytes.
    fn capacity_bytes(&self) -> u64;

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;

    /// Best-effort deallocation (discard/TRIM) of the given byte range.
    ///
    /// The default implementation validates that the range is in-bounds and then performs no
    /// operation. Sparse disk formats may override this to actually reclaim storage; callers should
    /// treat failures as non-fatal unless they need strict guarantees.
    ///
    /// Implementations are permitted to deallocate only full allocation units (e.g. discard only
    /// fully covered sparse blocks).
    fn discard_range(&mut self, offset: u64, len: u64) -> Result<()> {
        if len == 0 {
            if offset > self.capacity_bytes() {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: 0,
                    capacity: self.capacity_bytes(),
                });
            }
            return Ok(());
        }

        let end = offset.checked_add(len).ok_or(DiskError::OffsetOverflow)?;
        if end > self.capacity_bytes() {
            return Err(DiskError::OutOfBounds {
                offset,
                len: usize::try_from(len).unwrap_or(usize::MAX),
                capacity: self.capacity_bytes(),
            });
        }
        Ok(())
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        if !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(DiskError::UnalignedLength {
                len: buf.len(),
                alignment: SECTOR_SIZE,
            });
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        self.read_at(offset, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<()> {
        if !buf.len().is_multiple_of(SECTOR_SIZE) {
            return Err(DiskError::UnalignedLength {
                len: buf.len(),
                alignment: SECTOR_SIZE,
            });
        }
        let offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        self.write_at(offset, buf)
    }
}

/// Read-only wrapper for a [`VirtualDisk`].
///
/// This is the disk-oriented companion to [`crate::ReadOnlyBackend`]. It is typically the most
/// convenient way to enforce read-only access for emulator-facing code, since it can turn `flush`
/// into a no-op while still rejecting writes.
pub struct ReadOnlyDisk<D> {
    inner: D,
}

impl<D> ReadOnlyDisk<D> {
    #[must_use]
    pub fn new(inner: D) -> Self {
        Self { inner }
    }

    #[must_use]
    pub fn inner(&self) -> &D {
        &self.inner
    }

    #[must_use]
    pub fn into_inner(self) -> D {
        self.inner
    }
}

impl<D: VirtualDisk> VirtualDisk for ReadOnlyDisk<D> {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, _offset: u64, _buf: &[u8]) -> Result<()> {
        Err(DiskError::NotSupported("read-only".into()))
    }

    fn flush(&mut self) -> Result<()> {
        // Intentionally a no-op: this wrapper exists to prevent writes. Some emulator code calls
        // `flush()` unconditionally, and returning an error would make a read-only disk harder to
        // use in practice.
        Ok(())
    }
}

impl<T: VirtualDisk + ?Sized> VirtualDisk for Box<T> {
    fn capacity_bytes(&self) -> u64 {
        (**self).capacity_bytes()
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

    fn discard_range(&mut self, offset: u64, len: u64) -> Result<()> {
        (**self).discard_range(offset, len)
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        (**self).read_sectors(lba, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<()> {
        (**self).write_sectors(lba, buf)
    }
}

/// A raw disk image stored in a byte backend (OPFS file, ArrayBuffer, etc.).
pub struct RawDisk<B> {
    backend: B,
    capacity: u64,
}

impl<B: StorageBackend> RawDisk<B> {
    pub fn create(mut backend: B, capacity_bytes: u64) -> Result<Self> {
        backend.set_len(capacity_bytes)?;
        Ok(Self {
            backend,
            capacity: capacity_bytes,
        })
    }

    pub fn open(mut backend: B) -> Result<Self> {
        let capacity = backend.len()?;
        Ok(Self { backend, capacity })
    }

    pub fn into_backend(self) -> B {
        self.backend
    }
}

impl<B: StorageBackend + VirtualDiskSend> VirtualDisk for RawDisk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.capacity
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity)?;
        self.backend.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity)?;
        self.backend.write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<()> {
        self.backend.flush()
    }
}
