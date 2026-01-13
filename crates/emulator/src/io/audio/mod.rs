//! Legacy emulator audio stack (AC'97 / HDA device models + DSP utilities).
//!
//! ## Status / feature gate
//!
//! This directory is **legacy**: it is retained for reference and for targeted unit/integration
//! testing, but it is **not** used by the browser/WASM runtime path.
//!
//! The entire module is gated behind the `emulator/legacy-audio` feature (i.e. the `legacy-audio`
//! crate feature on `emulator`). It is intentionally disabled by default.
//!
//! If you are looking for the current, canonical implementation, see:
//! - `crates/aero-audio`
//! - `crates/aero-virtio`
//! - `crates/platform::audio`
//!
//! ## Inventory
//!
//! - `ac97`: AC'97 controller model and DMA plumbing.
//! - `hda`: Intel High Definition Audio (HDA) controller/codec model.
//! - `dsp`: PCM decode/convert + channel remixing + resampling (`sinc-resampler` optional).
//!
//! ## Running tests
//!
//! ```bash
//! cargo test -p emulator --features legacy-audio
//! cargo test -p emulator --features "legacy-audio sinc-resampler"
//! ```
//!
//! ## DSP benchmark
//!
//! ```bash
//! cargo bench -p emulator --bench dsp --features legacy-audio
//! ```

pub mod ac97;
pub mod dsp;
pub mod hda;
