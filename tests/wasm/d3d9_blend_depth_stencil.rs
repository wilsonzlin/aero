// NOTE: This file is intentionally `wasm32`-only. The native render tests live
// in `tests/d3d9_blend_depth_stencil.rs`.
//
// A browser harness (wasm-bindgen-test or custom) is expected to execute these.

#![cfg(target_arch = "wasm32")]

// This module is kept minimal so the project can add a browser harness later
// without refactoring the state translation layer. The native tests cover the
// same blend/depth/stencil cases via a headless WGPU backend.
