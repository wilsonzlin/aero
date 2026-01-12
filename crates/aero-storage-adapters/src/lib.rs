//! Cross-crate adapter types for using `aero-storage` disks with device models.
//!
//! The Aero codebase has a few different disk traits:
//! - [`aero_storage::VirtualDisk`] (disk image layer)
//! - `aero_devices_nvme::DiskBackend` (NVMe controller)
//! - `aero_devices::storage::DiskBackend` (virtio-blk and other device models)
//!
//! This crate provides lightweight wrapper *types* around [`aero_storage::VirtualDisk`].
//! The concrete `DiskBackend` implementations live in the crates that define those
//! traits (Rust orphan rules), but using a shared wrapper type avoids duplicating the
//! underlying disk abstraction.
//!
//! ## Usage (examples)
//!
//! Wrap an [`aero_storage::VirtualDisk`] for use with the NVMe device model:
//!
//! ```rust,ignore
//! use aero_devices_nvme::{from_virtual_disk, NvmeController};
//! use aero_storage::{MemBackend, RawDisk};
//!
//! let disk = RawDisk::create(MemBackend::new(), 1024 * 512).unwrap();
//! let mut ctrl = NvmeController::new(from_virtual_disk(Box::new(disk)).unwrap());
//! ```
//!
//! Wrap an [`aero_storage::VirtualDisk`] for use with `aero-devices` virtio-blk:
//!
//! ```rust,ignore
//! use aero_devices::storage::VirtualDrive;
//! use aero_storage::{MemBackend, RawDisk};
//! use aero_storage_adapters::AeroVirtualDiskAsDeviceBackend;
//!
//! let disk = RawDisk::create(MemBackend::new(), 1024 * 512).unwrap();
//! let backend = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk));
//! let drive = VirtualDrive::new(512, Box::new(backend));
//! ```

use std::io;
use std::sync::Mutex;

use aero_storage::{VirtualDisk, SECTOR_SIZE};

/// Adapter wrapper for exposing an [`aero_storage::VirtualDisk`] as an NVMe disk backend.
///
/// The actual `aero_devices_nvme::DiskBackend` implementation is provided by the
/// `aero-devices-nvme` crate. That crate re-exports this wrapper as
/// `aero_devices_nvme::AeroStorageDiskAdapter` for ergonomic use at call sites.
pub struct AeroVirtualDiskAsNvmeBackend {
    disk: Box<dyn VirtualDisk + Send>,
}

impl AeroVirtualDiskAsNvmeBackend {
    /// NVMe adapters currently only support 512-byte sectors.
    pub const SECTOR_SIZE: u32 = SECTOR_SIZE as u32;

    pub fn new(disk: Box<dyn VirtualDisk + Send>) -> Self {
        Self { disk }
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.disk.capacity_bytes()
    }

    pub fn disk_mut(&mut self) -> &mut (dyn VirtualDisk + Send) {
        &mut *self.disk
    }

    pub fn into_inner(self) -> Box<dyn VirtualDisk + Send> {
        self.disk
    }
}

impl From<Box<dyn VirtualDisk + Send>> for AeroVirtualDiskAsNvmeBackend {
    fn from(disk: Box<dyn VirtualDisk + Send>) -> Self {
        Self::new(disk)
    }
}

/// Adapter wrapper for exposing an [`aero_storage::VirtualDisk`] as a byte-addressed
/// `aero-devices` disk backend.
///
/// The actual `aero_devices::storage::DiskBackend` implementation is provided by the
/// `aero-devices` crate.
pub struct AeroVirtualDiskAsDeviceBackend {
    disk: Mutex<Box<dyn VirtualDisk + Send>>,
}

impl AeroVirtualDiskAsDeviceBackend {
    /// `aero-devices` is byte-addressed, but we still enforce 512-byte sector alignment.
    pub const SECTOR_SIZE: u64 = SECTOR_SIZE as u64;

    pub fn new(disk: Box<dyn VirtualDisk + Send>) -> Self {
        Self {
            disk: Mutex::new(disk),
        }
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.with_disk(|disk| disk.capacity_bytes())
    }

    /// Read exactly `buf.len()` bytes at `offset` (bytes), enforcing 512-byte alignment.
    pub fn read_at_aligned(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.check_access(offset, buf.len())?;
        self.with_disk_mut(|disk| disk.read_at(offset, buf))
            .map_err(map_aero_storage_error_to_io)
    }

    /// Write all `buf.len()` bytes at `offset` (bytes), enforcing 512-byte alignment.
    pub fn write_at_aligned(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.check_access(offset, buf.len())?;
        self.with_disk_mut(|disk| disk.write_at(offset, buf))
            .map_err(map_aero_storage_error_to_io)
    }

    /// Flush the underlying disk.
    pub fn flush(&self) -> io::Result<()> {
        self.with_disk_mut(|disk| disk.flush())
            .map_err(map_aero_storage_error_to_io)
    }

    fn check_access(&self, offset: u64, len: usize) -> io::Result<()> {
        let len_u64 = u64::try_from(len).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "length does not fit in u64")
        })?;
        if !offset.is_multiple_of(Self::SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unaligned offset {offset} (expected multiple of {})",
                    Self::SECTOR_SIZE
                ),
            ));
        }
        if !len_u64.is_multiple_of(Self::SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unaligned length {len} (expected multiple of {})",
                    Self::SECTOR_SIZE
                ),
            ));
        }
        let end = offset
            .checked_add(len_u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;
        let cap = self.capacity_bytes();
        if end > cap {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("out of bounds: offset={offset} len={len} capacity={cap}"),
            ));
        }
        Ok(())
    }

    fn with_disk<R>(&self, f: impl FnOnce(&dyn VirtualDisk) -> R) -> R {
        let guard = self
            .disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&**guard)
    }

    fn with_disk_mut<R>(&self, f: impl FnOnce(&mut dyn VirtualDisk) -> R) -> R {
        let mut guard = self
            .disk
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f(&mut **guard)
    }
}

impl From<Box<dyn VirtualDisk + Send>> for AeroVirtualDiskAsDeviceBackend {
    fn from(disk: Box<dyn VirtualDisk + Send>) -> Self {
        Self::new(disk)
    }
}

fn map_aero_storage_error_to_io(err: aero_storage::DiskError) -> io::Error {
    match err {
        err @ (aero_storage::DiskError::UnalignedLength { .. }
        | aero_storage::DiskError::OffsetOverflow
        | aero_storage::DiskError::InvalidConfig(_)
        | aero_storage::DiskError::InvalidSparseHeader(_)) => {
            io::Error::new(io::ErrorKind::InvalidInput, err)
        }
        err @ (aero_storage::DiskError::CorruptImage(_)
        | aero_storage::DiskError::CorruptSparseImage(_)) => {
            io::Error::new(io::ErrorKind::InvalidData, err)
        }
        err @ aero_storage::DiskError::OutOfBounds { .. } => {
            io::Error::new(io::ErrorKind::UnexpectedEof, err)
        }
        err @ (aero_storage::DiskError::Unsupported(_) | aero_storage::DiskError::NotSupported(_)) => {
            io::Error::new(io::ErrorKind::Unsupported, err)
        }
        err @ aero_storage::DiskError::QuotaExceeded => io::Error::new(io::ErrorKind::StorageFull, err),
        err @ aero_storage::DiskError::InUse => io::Error::new(io::ErrorKind::ResourceBusy, err),
        err @ aero_storage::DiskError::InvalidState(_) => io::Error::other(err),
        err @ aero_storage::DiskError::BackendUnavailable => {
            io::Error::new(io::ErrorKind::NotConnected, err)
        }
        err @ aero_storage::DiskError::Io(_) => io::Error::other(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_storage::{MemBackend, RawDisk};

    #[test]
    fn device_backend_adapter_enforces_alignment_and_bounds() {
        let cap = 4 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), cap).unwrap();
        let adapter = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk));

        // Unaligned length.
        let mut buf = [0u8; 1];
        let err = adapter.read_at_aligned(0, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // Unaligned offset.
        let mut buf = [0u8; SECTOR_SIZE];
        let err = adapter.read_at_aligned(1, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        // Out of bounds.
        let mut buf = [0u8; SECTOR_SIZE];
        let err = adapter.read_at_aligned(cap, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn map_aero_storage_error_to_io_classifies_unsupported_and_corrupt() {
        let err = map_aero_storage_error_to_io(aero_storage::DiskError::Unsupported("feature"));
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let err =
            map_aero_storage_error_to_io(aero_storage::DiskError::NotSupported("backend".into()));
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::CorruptImage("bad"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
