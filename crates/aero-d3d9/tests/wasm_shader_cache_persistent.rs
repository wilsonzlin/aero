// wasm-specific shader cache tests live in `tests/wasm/`.
//
// Cargo only discovers integration tests in `tests/*.rs`, so this file is a tiny
// shim that pulls in the module under `tests/wasm/` when building for wasm32.

#[cfg(target_arch = "wasm32")]
#[path = "wasm/d3d9_shader_cache_persistent.rs"]
mod d3d9_shader_cache_persistent;
