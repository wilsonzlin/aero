pub mod ahci;
pub mod backends;
pub mod cache;
pub mod disk;
pub mod error;
pub mod formats;
pub mod ide;
pub mod metadata;
pub mod nvme;
pub mod rangeset;
pub mod sparse;

pub const SECTOR_SIZE: usize = 512;

pub use cache::{BlockCache, BlockCacheConfig, CoalescedRange, CoalescingBackend};
pub use disk::{ByteStorage, DiskBackend, DiskFormat, VirtualDrive, WriteCachePolicy};
pub use error::{DiskError, DiskResult, StorageError};
