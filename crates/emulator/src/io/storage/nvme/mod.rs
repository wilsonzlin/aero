//! NVMe (PCIe) controller emulation for the legacy `crates/emulator` device harness.
//!
//! The canonical NVMe device model lives in `crates/aero-devices-nvme`. This module provides an
//! emulator-facing compatibility wrapper so existing tests/benches can keep using
//! `emulator::io::storage::nvme::{NvmeController, NvmePciDevice}` with the historical API surface.

mod wrapper;
pub use wrapper::{NvmeController, NvmePciDevice};
