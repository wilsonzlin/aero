//! Workspace-visible entrypoint for fuzzing-related code.
//!
//! The actual libFuzzer (`cargo-fuzz`) harness lives in `./fuzz/` (which has its own toolchain and
//! lockfile). This crate exists so CI/automation can build `-p aero-fuzz` on the stable workspace
//! toolchain without pulling in libFuzzer-specific build requirements.
