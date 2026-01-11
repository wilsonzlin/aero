//! Minimal utilities shared by Aerogpu runtime components.
//!
//! This crate intentionally stays small: the D3D11 bring-up path needs a simple
//! guest memory abstraction to upload resources into wgpu.

pub mod guest_memory;

