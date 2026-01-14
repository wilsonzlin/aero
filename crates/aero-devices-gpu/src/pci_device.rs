//! PCI device wrapper.
//!
//! The canonical implementation currently lives in [`crate::pci`]. This module exists to provide a
//! stable module name (`pci_device`) for downstream crates while the device model is refactored.

pub use crate::pci::*;
