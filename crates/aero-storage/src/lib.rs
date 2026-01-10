//! Virtual disk abstractions and disk image formats used by Aero.
//!
//! The emulator needs a *sector-oriented* disk interface, but browser storage APIs are
//! byte-addressed and often benefit from block-based caching. This crate provides:
//!
//! - [`VirtualDisk`]: byte-addressed disk interface with sector helpers
//! - [`RawDisk`]: maps a resizable byte backend to a fixed-capacity disk (raw images)
//! - [`AeroSparseDisk`]: Aero-specific sparse disk format for huge virtual disks
//! - [`AeroCowDisk`]: copy-on-write overlay on top of a base disk
//! - [`BlockCachedDisk`]: LRU, write-back block cache wrapper
//!
//! Browser backends (OPFS primary, IndexedDB fallback) live in the TypeScript glue layer.

mod backend;
mod cache;
mod cow;
mod disk;
mod error;
mod sparse;
mod util;

pub use backend::{MemBackend, StorageBackend};
pub use cache::{BlockCacheStats, BlockCachedDisk};
pub use cow::AeroCowDisk;
pub use disk::{RawDisk, VirtualDisk, SECTOR_SIZE};
pub use error::{DiskError, Result};
pub use sparse::{AeroSparseConfig, AeroSparseDisk, AeroSparseHeader};

#[cfg(test)]
mod tests;
