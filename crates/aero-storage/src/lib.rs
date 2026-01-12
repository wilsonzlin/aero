//! Virtual disk abstractions and disk image formats used by Aero.
//!
//! The emulator needs a *sector-oriented* disk interface, but browser storage APIs are
//! byte-addressed and often benefit from block-based caching. This crate provides:
//!
//! - [`VirtualDisk`]: byte-addressed disk interface with sector helpers
//! - [`RawDisk`]: maps a resizable byte backend to a fixed-capacity disk (raw images)
//! - [`AeroSparseDisk`]: Aero-specific sparse disk format for huge virtual disks
//! - [`Qcow2Disk`]: QCOW2 v2/v3 (subset) support for common developer images
//! - [`VhdDisk`]: VHD (fixed + dynamic) support
//! - [`AeroCowDisk`]: copy-on-write overlay on top of a base disk
//! - [`BlockCachedDisk`]: LRU, write-back block cache wrapper
//! - [`DiskImage`]: auto-detect + open wrapper for multiple formats
//!
//! ## Example: open with format detection
//!
//! ```rust,no_run
//! use aero_storage::{DiskImage, MemBackend, VirtualDisk};
//!
//! // In production this could be an OPFS backend such as `aero_opfs::OpfsByteStorage` (wasm32).
//! // IndexedDB-based storage is generally async and is not currently exposed as a sync
//! // `aero_storage::StorageBackend` in this crate; see `docs/19-indexeddb-storage-story.md` and
//! // `docs/20-storage-trait-consolidation.md`.
//! let backend = MemBackend::with_len(1024 * 1024).unwrap();
//! let mut disk = DiskImage::open_auto(backend).unwrap();
//!
//! let mut sector = [0u8; 512];
//! disk.read_sectors(0, &mut sector).unwrap();
//! ```
//!
//! In the browser, local persistence is typically backed by OPFS. Aero provides a
//! Rust/wasm32 OPFS backend implementation in `crates/aero-opfs` that implements
//! [`StorageBackend`] (byte-addressed) and [`VirtualDisk`] (disk-oriented).
//!
//! Higher-level orchestration such as remote HTTP streaming, caching policy, and UI
//! integration may still be handled by the TypeScript host layer.
//!
//! For host-side testing and development, this crate also includes an optional
//! (native-only) HTTP Range streaming helper.
//!
//! Note: IndexedDB-based storage is generally async and is not currently exposed as a
//! synchronous [`StorageBackend`] in this crate. The async IndexedDB block store lives in
//! `crates/st-idb`. See `docs/19-indexeddb-storage-story.md` and
//! `docs/20-storage-trait-consolidation.md`.
//!
//! ## Errors
//!
//! Fallible operations return [`Result`], which uses the unified [`DiskError`] type. `DiskError`
//! is shared across both native and wasm32 backends (including `crates/aero-opfs`), and its
//! [`DiskError::Io`] variant intentionally stores a human-readable `String` so browser backends
//! can surface JavaScript/DOM errors without requiring `std::io::Error`.

mod backend;
mod cache;
mod cow;
mod disk;
mod error;
mod formats;
mod qcow2;
mod sparse;
mod util;
mod vhd;

pub use backend::{MemBackend, StorageBackend};
pub use cache::{BlockCacheStats, BlockCachedDisk};
pub use cow::AeroCowDisk;
pub use disk::{RawDisk, VirtualDisk, SECTOR_SIZE};
pub use error::{DiskError, Result};
pub use formats::{detect_format, DiskFormat, DiskImage};
pub use qcow2::Qcow2Disk;
pub use sparse::{AeroSparseConfig, AeroSparseDisk, AeroSparseHeader};
pub use vhd::VhdDisk;

#[cfg(test)]
mod tests;

#[cfg(not(target_arch = "wasm32"))]
mod range_set;
#[cfg(not(target_arch = "wasm32"))]
mod streaming;

#[cfg(not(target_arch = "wasm32"))]
pub use range_set::{ByteRange, RangeSet};
#[cfg(not(target_arch = "wasm32"))]
pub use streaming::{
    CacheStatus, ChunkManifest, ChunkStore, DirectoryChunkStore, SparseFileChunkStore,
    StreamingCacheBackend, StreamingDisk, StreamingDiskConfig, StreamingDiskError,
    StreamingDiskOptions, StreamingTelemetrySnapshot, DEFAULT_CHUNK_SIZE, DEFAULT_SECTOR_SIZE,
};
