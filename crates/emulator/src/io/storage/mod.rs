//! Storage subsystem helpers and controller models.
//!
//! Note: The in-tree legacy IDE and NVMe implementations have been removed in favor of the
//! canonical device crates (`aero-devices-storage` / `aero-devices-nvme`). AHCI/NVMe remain as thin
//! compatibility wrappers where needed for the legacy emulator device harness.

pub mod adapters;
pub mod ahci;
pub mod cache;
pub mod disk;
pub mod error;
pub mod formats;
pub mod ide;
pub mod nvme;
pub mod pci_compat;

pub const SECTOR_SIZE: usize = 512;

pub use cache::{BlockCache, BlockCacheConfig, CoalescedRange, CoalescingBackend};
#[allow(deprecated)]
pub use disk::ByteStorage;
pub use disk::{DiskBackend, DiskFormat, VirtualDrive, WriteCachePolicy};
pub use error::{DiskError, DiskResult};
pub use pci_compat::PciConfigSpaceCompat;
