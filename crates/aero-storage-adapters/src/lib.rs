//! Cross-crate adapter types for using `aero-storage` disks with device models.
//!
//! The Aero codebase has a few different disk traits:
//! - [`aero_storage::VirtualDisk`] (disk image layer; canonical synchronous disk trait)
//! - `aero_devices::storage::DiskBackend` (byte-addressed device-model backend used by parts of the
//!   `aero-devices` stack)
//! - legacy `emulator::io::storage::disk::DiskBackend` (sector-addressed backend used by the
//!   legacy emulator storage stack)
//!
//! This crate provides lightweight wrapper *types* around [`aero_storage::VirtualDisk`].
//! The concrete `DiskBackend` implementations live in the crates that define those
//! traits (Rust orphan rules), but using a shared wrapper type avoids duplicating the
//! underlying disk abstraction.
//!
//! Device crates often re-export these wrapper types as `AeroStorageDiskAdapter` so platform wiring
//! code can use a consistent name across controllers (e.g.
//! `aero_devices::storage::AeroStorageDiskAdapter`).
//!
//! See `docs/20-storage-trait-consolidation.md` for the repo-wide storage trait consolidation plan
//! and guidance on where adapter types vs trait impls should live.
//!
//! ## Virtio-blk (`aero-virtio`)
//!
//! `aero-virtio`'s virtio-blk device model consumes a boxed [`aero_storage::VirtualDisk`] directly,
//! so most call sites do not require any adapter wrapper types.
//!
//! The `aero-devices` stack still uses its own backend trait; see
//! `aero_devices::storage::VirtualDrive::{new_from_aero_virtual_disk, try_new_from_aero_virtual_disk}`
//! for wiring a boxed `VirtualDisk` into that device-model boundary.
//!
//! ## Usage (examples)
//!
//! Wrap an [`aero_storage::VirtualDisk`] for use with crates that expect a `std::io`-style,
//! byte-addressed backend (e.g. `aero_devices::storage::DiskBackend`):
//!
//! ```rust
//! use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
//! use aero_storage_adapters::AeroVirtualDiskAsDeviceBackend;
//!
//! let disk = RawDisk::create(MemBackend::new(), (2 * SECTOR_SIZE) as u64).unwrap();
//! let backend = AeroVirtualDiskAsDeviceBackend::new(Box::new(disk));
//!
//! // The wrapper enforces 512-byte alignment at the device boundary.
//! let mut buf = [0u8; SECTOR_SIZE];
//! backend.read_at_aligned(0, &mut buf).unwrap();
//! ```
//!
//! Wrap an [`aero_storage::VirtualDisk`] for use with a sector-addressed adapter wrapper type:
//!
//! ```rust
//! use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE};
//! use aero_storage_adapters::AeroVirtualDiskAsNvmeBackend;
//!
//! let disk = RawDisk::create(MemBackend::new(), (4 * SECTOR_SIZE) as u64).unwrap();
//! let adapter = AeroVirtualDiskAsNvmeBackend::new(Box::new(disk));
//! assert_eq!(adapter.capacity_bytes(), (4 * SECTOR_SIZE) as u64);
//! ```

use std::io;
use std::sync::Mutex;

use aero_storage::{VirtualDisk, SECTOR_SIZE};

// `VirtualDisk` is conditionally `Send` via `aero_storage::VirtualDiskSend`:
// - native: `dyn VirtualDisk` is `Send`
// - wasm32: `dyn VirtualDisk` may be `!Send` (OPFS/JS-backed handles, etc.)
type NvmeDiskBackend = Box<dyn VirtualDisk>;

/// Adapter wrapper for exposing an [`aero_storage::VirtualDisk`] through a sector-addressed disk
/// backend interface.
///
/// This wrapper is used by legacy sector-addressed storage stacks (e.g. the `crates/emulator` disk
/// models).
pub struct AeroVirtualDiskAsNvmeBackend {
    disk: NvmeDiskBackend,
}

impl AeroVirtualDiskAsNvmeBackend {
    /// NVMe adapters currently only support 512-byte sectors.
    pub const SECTOR_SIZE: u32 = SECTOR_SIZE as u32;

    pub fn new(disk: NvmeDiskBackend) -> Self {
        Self { disk }
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.disk.capacity_bytes()
    }

    pub fn disk_mut(&mut self) -> &mut dyn VirtualDisk {
        &mut *self.disk
    }

    pub fn into_inner(self) -> NvmeDiskBackend {
        self.disk
    }
}

impl From<NvmeDiskBackend> for AeroVirtualDiskAsNvmeBackend {
    fn from(disk: NvmeDiskBackend) -> Self {
        Self::new(disk)
    }
}

impl From<Box<dyn VirtualDisk + Send>> for AeroVirtualDiskAsNvmeBackend {
    fn from(disk: Box<dyn VirtualDisk + Send>) -> Self {
        // Drop the explicit `Send` auto-trait. On native, `dyn VirtualDisk` is already `Send`
        // via `VirtualDiskSend`; on wasm32 the `Send` bound is intentionally omitted.
        let disk: Box<dyn VirtualDisk> = disk;
        Self::new(disk)
    }
}

/// Adapter wrapper for exposing an [`aero_storage::VirtualDisk`] as a byte-addressed
/// `aero-devices` disk backend.
///
/// The actual `aero_devices::storage::DiskBackend` implementation is provided by the
/// `aero-devices` crate. That crate re-exports this wrapper as
/// `aero_devices::storage::AeroStorageDiskAdapter` for ergonomic use at call sites.
pub struct AeroVirtualDiskAsDeviceBackend {
    disk: Mutex<Box<dyn VirtualDisk>>,
}

impl AeroVirtualDiskAsDeviceBackend {
    /// `aero-devices` is byte-addressed, but we still enforce 512-byte sector alignment.
    pub const SECTOR_SIZE: u64 = SECTOR_SIZE as u64;

    ///
    /// Note: `aero_storage::VirtualDisk` is conditionally `Send` (it is `Send` on native targets,
    /// but may be `!Send` on wasm32). This wrapper intentionally accepts `Box<dyn VirtualDisk>`
    /// without requiring `Send` so browser backends like OPFS can be wired in without an unsound
    /// `unsafe impl Send`.
    pub fn new(disk: Box<dyn VirtualDisk>) -> Self {
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

impl From<Box<dyn VirtualDisk>> for AeroVirtualDiskAsDeviceBackend {
    fn from(disk: Box<dyn VirtualDisk>) -> Self {
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
        err @ (aero_storage::DiskError::Unsupported(_)
        | aero_storage::DiskError::NotSupported(_)) => {
            io::Error::new(io::ErrorKind::Unsupported, err)
        }
        err @ aero_storage::DiskError::QuotaExceeded => {
            io::Error::new(io::ErrorKind::StorageFull, err)
        }
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

        // Offset arithmetic overflow (but still sector-aligned).
        let offset = u64::MAX - (SECTOR_SIZE as u64 - 1);
        let err = adapter.read_at_aligned(offset, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn map_aero_storage_error_to_io_classifies_unsupported_and_corrupt() {
        let err = map_aero_storage_error_to_io(aero_storage::DiskError::Unsupported("feature"));
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::Unsupported("feature"))
        ));

        let err =
            map_aero_storage_error_to_io(aero_storage::DiskError::NotSupported("backend".into()));
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::NotSupported(msg)) if msg == "backend"
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::CorruptImage("bad"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::CorruptImage("bad"))
        ));
    }

    #[test]
    fn map_aero_storage_error_to_io_preserves_invalid_input_and_bounds_errors() {
        let err = map_aero_storage_error_to_io(aero_storage::DiskError::UnalignedLength {
            len: 1,
            alignment: SECTOR_SIZE,
        });
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::UnalignedLength {
                len: 1,
                alignment: SECTOR_SIZE
            })
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::OffsetOverflow);
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::OffsetOverflow)
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::OutOfBounds {
            offset: 4,
            len: 1,
            capacity: 4,
        });
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::OutOfBounds {
                offset: 4,
                len: 1,
                capacity: 4
            })
        ));
    }

    #[test]
    fn map_aero_storage_error_to_io_classifies_alignment_and_bounds() {
        let err = map_aero_storage_error_to_io(aero_storage::DiskError::UnalignedLength {
            len: 1,
            alignment: SECTOR_SIZE,
        });
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::OffsetOverflow);
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::InvalidConfig("bad"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::InvalidSparseHeader("bad"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::OutOfBounds {
            offset: 0,
            len: 1,
            capacity: 0,
        });
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::CorruptSparseImage("bad"));
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn map_aero_storage_error_to_io_classifies_browser_storage_failures() {
        let err = map_aero_storage_error_to_io(aero_storage::DiskError::QuotaExceeded);
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::QuotaExceeded)
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::InUse);
        assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::InUse)
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::BackendUnavailable);
        assert_eq!(err.kind(), io::ErrorKind::NotConnected);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::BackendUnavailable)
        ));

        let err =
            map_aero_storage_error_to_io(aero_storage::DiskError::InvalidState("closed".into()));
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::InvalidState(msg)) if msg == "closed"
        ));

        let err = map_aero_storage_error_to_io(aero_storage::DiskError::Io("boom".into()));
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert!(matches!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<aero_storage::DiskError>()),
            Some(aero_storage::DiskError::Io(msg)) if msg == "boom"
        ));
    }
}
