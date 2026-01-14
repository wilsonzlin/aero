//! Target-portable signal numbers.
//!
//! The conformance harness compares crash/fault termination between different backends. On Unix
//! hosts, those faults are reported as POSIX signal numbers (e.g. `SIGSEGV`). However, when the
//! crate is compiled for targets without signals (such as `wasm32-unknown-unknown`), `libc` does
//! not provide these constants.
//!
//! CI builds the test targets for wasm in "no-run" mode, so we provide dummy values on non-Unix
//! targets to keep the crate compiling. These values are never observed in practice because the
//! host reference backend is not available on wasm.
//!
//! Keep this module as the only place that refers to `libc::SIG*` so the rest of the crate can be
//! target-agnostic.
#[cfg(unix)]
pub(crate) const SIGILL: i32 = libc::SIGILL;
#[cfg(not(unix))]
pub(crate) const SIGILL: i32 = 0;

#[cfg(unix)]
pub(crate) const SIGFPE: i32 = libc::SIGFPE;
#[cfg(not(unix))]
pub(crate) const SIGFPE: i32 = 0;

#[cfg(unix)]
pub(crate) const SIGSEGV: i32 = libc::SIGSEGV;
#[cfg(not(unix))]
pub(crate) const SIGSEGV: i32 = 0;
