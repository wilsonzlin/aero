//! Legacy emulator-local PCI framework.
//!
//! This module is **not** the canonical PCI/device layer. New PCI device models
//! and platform wiring should target:
//!
//! - `crates/devices` (`aero_devices::pci::*`)
//! - `crates/platform` / `crates/aero-pc-platform`
//! - `crates/aero-machine`
//!
//! The remaining PCI code here exists to support the legacy emulator device
//! stack (notably the AeroGPU device model) while the repo converges on the
//! canonical machine stack. See `docs/21-emulator-crate-migration.md`.

pub mod aerogpu;
#[cfg(feature = "aerogpu-legacy")]
pub mod aerogpu_legacy;
