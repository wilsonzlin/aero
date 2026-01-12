//! Origin Private File System (OPFS) storage backends for Aero (wasm32).
//!
//! This crate provides wasm32 implementations of `aero-storage` traits on top of browser
//! persistence APIs.
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
