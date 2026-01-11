//! Shared WASM ABI surface for Aero JIT-generated modules.
//!
//! This module intentionally only exposes stable import/export names and legacy
//! baseline codegen (behind `legacy-baseline`). Tier-1/Tier-2 code generators
//! live under [`crate::tier1::wasm_codegen`] and [`crate::tier2::wasm_codegen`].

pub mod abi;

#[cfg(feature = "legacy-baseline")]
pub mod legacy;

pub use abi::*;
