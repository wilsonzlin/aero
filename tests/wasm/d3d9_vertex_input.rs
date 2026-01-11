// NOTE: This file is intentionally `wasm32`-only. The native render tests live in
// `tests/d3d9_vertex_input.rs` (wired up via `crates/aero-d3d9`'s Cargo manifest).
//
// A browser harness (wasm-bindgen-test or custom) is expected to execute these.

#![cfg(target_arch = "wasm32")]

// This module is kept minimal so the project can add a browser harness later
// without refactoring the vertex input translation layer. The native tests cover:
// - position+color+uv
// - multi-stream vertex fetch
// - instanced rendering with per-instance color
