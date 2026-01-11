//! Minimal Rust-only emulator core used by unit tests and bring-up code.
//!
//! This crate is **not** the primary browser-facing emulator implementation and it does **not**
//! contain the canonical Win7/WDDM AeroGPU device model or ABI.
//!
//! For the real AeroGPU contract, see:
//! - `drivers/aerogpu/protocol/*` (C headers, source of truth)
//! - `emulator/protocol` (Rust/TypeScript mirror)
//! - `crates/emulator/src/devices/pci/aerogpu.rs` (emulator device model)
//!
#![forbid(unsafe_code)]

pub mod bios;
pub mod cpu;
pub mod devices;
pub mod firmware;
pub mod memory;
