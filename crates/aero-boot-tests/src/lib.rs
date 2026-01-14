//! Workspace-level boot/integration tests.
//!
//! This crate exists only as a lightweight registration point for the QEMU/boot
//! integration tests that live under the workspace root `tests/` directory.
//!
//! Keeping these tests out of `crates/emulator` makes it explicit that they
//! exercise the canonical integration stack, not emulator-specific code.
