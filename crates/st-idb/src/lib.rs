//! IndexedDB-backed sparse disk storage for browser environments.
//!
//! The primary use case is providing a fallback storage backend when OPFS is
//! unavailable (e.g. in older browsers or restricted contexts). The backend
//! stores fixed-size blocks in IndexedDB and uses an in-memory LRU cache to
//! amortize reads/writes.

mod error;
pub mod io;
pub mod platform;

pub use crate::error::{Result, StorageError};
pub use crate::io::storage::backends::indexeddb::{IndexedDbBackend, IndexedDbBackendOptions};
pub use crate::io::storage::{DiskBackend, DiskBackendStats};
