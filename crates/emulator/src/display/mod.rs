pub use aero_shared::shared_framebuffer as framebuffer;
/// Shared framebuffer protocol used by the browser/WASM runtime.
///
/// This module intentionally re-exports the canonical implementation from
/// `aero_shared` so the Rust<->JS ABI is single-sourced.
///
/// The canonical module is also re-exported as `emulator::display::framebuffer`
/// to keep older module paths working.
pub use aero_shared::shared_framebuffer::*;
