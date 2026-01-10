//! Narrow, dependency-free virtio-gpu (2D) command processing prototype.
//!
//! This crate is intentionally small:
//! - It is **not** a full virtio device implementation (no virtqueue DMA, no virtio-pci).
//! - It focuses on the **control queue command set** needed for basic scanout bring-up.
//! - It is designed to be embedded into Aero's future device-model layer.

pub mod device;
pub mod protocol;

