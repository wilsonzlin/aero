//! Re-exported helpers shared across wgpu-based integration tests.
//!
//! Most tests use `tests/common/wgpu.rs` via `common::wgpu::*`, but a handful of older
//! integration tests expect a `wgpu_common` module at the crate root.

pub use crate::common::wgpu::*;

