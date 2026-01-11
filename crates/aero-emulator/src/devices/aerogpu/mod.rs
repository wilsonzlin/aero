//! AeroGPU constants.
//!
//! `aero-emulator` does not implement the canonical Win7/WDDM AeroGPU device model. The
//! authoritative device implementations live under `crates/emulator/src/devices/pci/aerogpu*.rs`.
//!
//! This module exists purely to provide stable PCI-facing constants for any low-level glue/tests.

pub mod pci;

pub use pci::*;

