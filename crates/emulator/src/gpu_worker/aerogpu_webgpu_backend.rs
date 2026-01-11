//! Compatibility shim for the WebGPU-backed AeroGPU executor.
//!
//! The original integration work landed under the `aerogpu-exec` feature and the
//! `aerogpu_wgpu_backend` module. The task spec (and some downstream users) refer to this as
//! "aerogpu-webgpu", so provide a thin re-export to keep the naming stable.

pub use super::aerogpu_wgpu_backend::AerogpuWgpuBackend as WebgpuAeroGpuBackend;
