//! Virtio device models and a small virtqueue + virtio-pci framework.
//!
//! This crate is intentionally self-contained so it can be used by the
//! larger emulator without pulling in the rest of the platform early.
//!
//! ## Storage backends (virtio-blk)
//!
//! The repo-wide canonical synchronous disk trait is [`aero_storage::VirtualDisk`]. The virtio-blk
//! device model in this crate (`devices::blk`) consumes a boxed `VirtualDisk` directly.
//!
//! See `docs/20-storage-trait-consolidation.md` for the repo-wide trait consolidation plan and
//! adapter layering guidance.

pub mod devices;
pub mod memory;
pub mod mmio;
pub mod pci;
pub mod queue;
