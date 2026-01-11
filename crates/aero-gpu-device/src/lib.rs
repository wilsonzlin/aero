//! Prototype GPU device model + guest↔host command ABI (AGRN/AGPC).
//!
//! This crate is **experimental** and exists as a self-contained harness for backend bring-up,
//! deterministic tests, and GPU trace recording/replay.
//!
//! It is **not** the Windows 7 / WDDM AeroGPU protocol. The Win7/WDDM target ABI is defined in
//! `drivers/aerogpu/protocol/*` and implemented by `crates/emulator` (see
//! `docs/graphics/aerogpu-protocols.md`).
//!
//! This crate contains three layers:
//! 1. **ABI definitions** (`abi`): structs/constants that define the stable
//!    byte-level protocol used by the guest driver/runtime.
//! 2. **Transport/device model** (`device`): a simple PCI/MMIO-style device with
//!    doorbells and an interrupt line.
//! 3. **Command processing backend** (`backend`): an abstract GPU backend plus a
//!    deterministic software implementation used by tests.

#![deny(unsafe_code)]

pub mod abi;
pub mod backend;
pub mod device;
pub mod guest;
pub mod guest_memory;
pub mod ring;
pub mod trace;
