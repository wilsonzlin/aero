//! Virtio device models and a small virtqueue + virtio-pci framework.
//!
//! This crate is intentionally self-contained so it can be used by the
//! larger emulator without pulling in the rest of the platform early.
//!
//! ## Storage backends (virtio-blk)
//!
//! The repo-wide canonical synchronous disk trait is [`aero_storage::VirtualDisk`]. The virtio-blk
//! device model in this crate (`devices::blk`) keeps a separate [`devices::blk::BlockBackend`] trait
//! for device ergonomics, but most platform wiring should pass a boxed `VirtualDisk` at the boundary
//! and use:
//!
//! - [`devices::blk::VirtioBlkDisk`] (a `VirtioBlk<Box<dyn VirtualDisk>>` type alias)
//! - `impl<T: VirtualDisk> BlockBackend for Box<T>` (plus impls for `Box<dyn VirtualDisk>` /
//!   `Box<dyn VirtualDisk + Send>`, so `Box<dyn VirtualDisk>` is a valid backend)
//!
//! If you need the reverse direction (wrapping an existing `BlockBackend` so you can layer
//! `aero-storage` disk wrappers such as caches/overlays on top), use
//! [`devices::blk::BlockBackendAsAeroVirtualDisk`].
//!
//! See `docs/20-storage-trait-consolidation.md` for the repo-wide trait consolidation plan and
//! adapter layering guidance.

pub mod devices;
pub mod memory;
pub mod mmio;
pub mod pci;
pub mod queue;
