//! Unit tests for `aero-dxbc`.
//!
//! These tests live in `src/` (instead of `crates/aero-dxbc/tests/`) so they can
//! use `crate::test_utils`, which is only compiled when `cfg(test)` is enabled.

#[path = "tests_parse.rs"]
mod parse;

#[path = "tests_rdef.rs"]
mod rdef;

#[path = "tests_rdef_ctab.rs"]
mod rdef_ctab;

#[path = "tests_signature.rs"]
mod signature;

#[path = "tests_sm4.rs"]
mod sm4;
