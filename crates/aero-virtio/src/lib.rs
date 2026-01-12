//! Virtio device models and a small virtqueue + virtio-pci framework.
//!
//! This crate is intentionally self-contained so it can be used by the
//! larger emulator without pulling in the rest of the platform early.

pub mod devices;
pub mod memory;
pub mod mmio;
pub mod pci;
pub mod queue;
