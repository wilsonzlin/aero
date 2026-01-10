pub mod ahci;
pub mod ide;
pub mod backends;
pub mod cache;
pub mod disk;
pub mod error;
pub mod formats;
pub mod metadata;
pub mod rangeset;
pub mod sparse;
pub mod nvme;

pub const SECTOR_SIZE: usize = 512;

pub use cache::{BlockCache, BlockCacheConfig, CoalescedRange};
pub use disk::{ByteStorage, DiskBackend, DiskFormat, VirtualDrive, WriteCachePolicy};
pub use error::{DiskError, DiskResult, StorageError};
