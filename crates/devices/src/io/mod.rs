// NOTE: This module contains unit tests. Deprecating the module unconditionally causes the
// auto-generated test harness to emit deprecation warnings when referencing the test functions.
// We therefore only deprecate it for non-test builds so the workspace stays warning-clean.
#[cfg_attr(not(test), deprecated(note = "use aero_virtio crate instead"))]
#[allow(deprecated)]
pub mod virtio;
