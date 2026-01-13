//! Legacy I/O module.
//!
//! This module exists for backwards compatibility and currently only contains a legacy virtio
//! implementation (`aero_devices::io::virtio`).
//!
//! New code should use the canonical `aero_virtio` crate instead.
//!
//! NOTE: We only deprecate the legacy `virtio` module for non-test builds. Deprecating it
//! unconditionally causes the auto-generated unit test harness to emit deprecation warnings when
//! referencing test functions by full path, which would add warnings to the workspace CI.
#[cfg_attr(not(test), deprecated(note = "use aero_virtio crate instead"))]
#[allow(deprecated)]
pub mod virtio;
