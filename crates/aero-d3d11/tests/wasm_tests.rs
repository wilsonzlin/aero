// wasm-specific tests live in `tests/wasm/`.
//
// Cargo only discovers integration tests in `tests/*.rs`, so this file is a tiny
// shim that pulls in the modules under `tests/wasm/` when building for wasm32.

#[cfg(target_arch = "wasm32")]
mod common;
#[cfg(target_arch = "wasm32")]
mod wasm;
