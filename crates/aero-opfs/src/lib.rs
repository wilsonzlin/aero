//! Origin Private File System (OPFS) storage backends for Aero (wasm32).
//!
//! This crate provides wasm32 implementations of `aero-storage` traits on top of browser
//! persistence APIs.
//!
//! Note: the Cargo package name is `aero-opfs`, but it is imported as `aero_opfs` in Rust code
//! (hyphens become underscores).
//!
//! The primary, boot-critical storage path is OPFS `FileSystemSyncAccessHandle` (fast and
//! synchronous in a Dedicated Worker). Some additional backends (async OPFS APIs, IndexedDB)
//! exist as async-only fallbacks for host-side tooling and environments where sync handles
//! are unavailable.
//!
//! Main types:
//!
//! - [`OpfsByteStorage`]: implements [`aero_storage::StorageBackend`] using OPFS
//!   `FileSystemSyncAccessHandle` when available.
//! - [`OpfsBackend`]: implements [`aero_storage::VirtualDisk`] for disk-oriented I/O.
//! - [`OpfsStorage`]: convenience wrapper that chooses the best available persistence backend
//!   (sync OPFS when possible; otherwise async fallbacks).
//! - [`OpfsSyncFile`]: wraps `FileSystemSyncAccessHandle` with a cursor and implements
//!   `std::io::{Read, Write, Seek}` for streaming snapshot read/write.
//!
//! Most APIs are meaningful only on wasm32; non-wasm builds provide stubs that return
//! [`DiskError::NotSupported`].
//!
//! ## Errors
//!
//! All public APIs in this crate use the canonical [`DiskError`] type from
//! [`aero_storage`], re-exported here for convenience.

pub mod io;
pub mod platform;

mod error;
pub use error::{DiskError, DiskResult};

pub use crate::io::snapshot_file::OpfsSyncFile;
pub use crate::io::storage::backends::opfs::{
    OpfsBackend, OpfsBackendMode, OpfsByteStorage, OpfsStorage,
};

// wasm-bindgen-test defaults to running under Node. OPFS requires a browser environment,
// so configure wasm-only tests to run in a browser once per crate.
#[cfg(all(test, target_arch = "wasm32"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
