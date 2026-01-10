//! Aero virtual GPU device model + guest↔host command ABI.
//!
//! This crate contains three layers:
//! 1. **ABI definitions** (`abi`): structs/constants that define the stable
//!    byte-level protocol used by the guest driver/runtime.
//! 2. **Transport/device model** (`device`): a simple PCI/MMIO-style device with
//!    doorbells and an interrupt line.
//! 3. **Command processing backend** (`backend`): an abstract GPU backend plus a
//!    deterministic software implementation used by tests.
//!
//! The intent is for the command stream to be produced by a Windows WDDM
//! paravirtual driver and consumed by the emulator, which then forwards into a
//! DirectX→WebGPU translation layer.

#![deny(unsafe_code)]

pub mod abi;
pub mod backend;
pub mod device;
pub mod guest;
pub mod guest_memory;
pub mod ring;
