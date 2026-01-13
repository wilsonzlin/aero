//! NVMe (PCIe) controller emulation for the legacy `crates/emulator` device harness.
//!
//! The canonical NVMe device model lives in `crates/aero-devices-nvme`. This module provides an
//! emulator-facing compatibility wrapper so existing tests/benches can keep using
//! `emulator::io::storage::nvme::{NvmeController, NvmePciDevice}` with the historical API surface.

#[cfg(feature = "storage-device-crates")]
mod wrapper;
#[cfg(feature = "storage-device-crates")]
pub use wrapper::{NvmeController, NvmePciDevice};

// Keep the legacy in-tree NVMe implementation around for builds that disable the
// `storage-device-crates` feature.
#[cfg(not(feature = "storage-device-crates"))]
mod legacy;
#[cfg(not(feature = "storage-device-crates"))]
pub use legacy::{NvmeController, NvmePciDevice};
