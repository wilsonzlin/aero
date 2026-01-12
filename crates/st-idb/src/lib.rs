//! IndexedDB-backed sparse disk storage for browser environments.
//!
//! The primary use case is providing a fallback storage backend when OPFS is
//! unavailable (e.g. in older browsers or restricted contexts). The backend
//! stores fixed-size blocks in IndexedDB and uses an in-memory LRU cache to
//! amortize reads/writes.
//!
//! Note: IndexedDB is fundamentally async, so this crate exposes an async storage interface
//! (`st_idb::DiskBackend`) and is **not** a synchronous `aero_storage::StorageBackend`.
//! The boot-critical synchronous storage path in the browser uses OPFS sync access handles
//! via `crates/aero-opfs`.
//!
//! Note: the Cargo package name is `st-idb`, but it is imported as `st_idb` in Rust code
//! (hyphens become underscores).

mod error;
pub mod io;
pub mod platform;

pub use crate::error::{Result, StorageError};
pub use crate::io::storage::backends::indexeddb::{IndexedDbBackend, IndexedDbBackendOptions};
pub use crate::io::storage::{DiskBackend, DiskBackendStats};
