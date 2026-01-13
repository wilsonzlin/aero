#![allow(dead_code)]

// Shared helpers for wgpu-backed integration tests.
//
// This file is both:
// - an integration test crate (`cargo test` will build it), and
// - a module imported by other tests via `mod wgpu_common;`.
//
// Keep it free of `#[test]` functions; it exists only to share setup glue.

// When this file is loaded as a module (e.g. `mod wgpu_common;`), Rust would normally resolve
// sibling modules relative to `tests/wgpu_common/`. Use explicit paths so we can share the
// `tests/common/wgpu.rs` helper implementation.
#[path = "common/wgpu.rs"]
mod common_wgpu;

pub use common_wgpu::create_device_queue;
