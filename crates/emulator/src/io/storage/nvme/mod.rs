//! Compatibility wrappers for the canonical NVMe device model (`aero-devices-nvme`).
//!
//! The emulator historically had its own NVMe implementation under this module. That code has been
//! removed in favour of a thin shim that:
//! - Accepts legacy emulator disk backends (`crate::io::storage::disk::DiskBackend`)
//! - Exposes the legacy module path (`emulator::io::storage::nvme::{NvmeController, NvmePciDevice}`)
//! - Implements the emulator-facing PCI/MMIO traits (`crate::io::pci::{PciDevice, MmioDevice}`)
//! - Preserves the legacy "immediate progress" behaviour by calling `process()` after MMIO.

mod wrapper;
pub use wrapper::{NvmeController, NvmePciDevice};
