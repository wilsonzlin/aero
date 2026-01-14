/// Shared framebuffer protocol used by the browser/WASM runtime.
///
/// This module intentionally re-exports the canonical implementation from
/// `aero_shared` so the Rust<->JS ABI is single-sourced.
pub mod framebuffer {
    pub use aero_shared::shared_framebuffer::*;
}
