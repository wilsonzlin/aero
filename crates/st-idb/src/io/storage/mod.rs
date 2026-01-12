use crate::Result;
use core::future::Future;
use core::pin::Pin;

pub mod backends;
pub mod cache;

/// A boxed, non-`Send` future.
///
/// `wasm-bindgen` futures are generally not `Send`, so we avoid requiring it.
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskBackendStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub blocks_read: u64,
    pub blocks_written: u64,
}

/// Minimal async storage interface used by the block cache.
///
/// This trait is intentionally async and uses a non-`Send` future (`LocalBoxFuture`) to
/// integrate cleanly with `wasm-bindgen` / browser event loops.
///
/// It is not the same as `aero_storage::StorageBackend`, which is synchronous and is
/// typically backed by OPFS sync access handles in the browser (`crates/aero-opfs`).
///
/// # Canonical trait note
///
/// For async browser-host storage in Rust/wasm, this `st-idb` trait is the canonical abstraction
/// today. For synchronous device/controller models and disk image formats, use
/// `aero_storage::{StorageBackend, VirtualDisk}` instead.
///
/// See `docs/20-storage-trait-consolidation.md`.
pub trait DiskBackend {
    fn capacity(&self) -> u64;
    fn stats(&self) -> DiskBackendStats;

    fn block_size(&self) -> usize;

    fn read_at<'a>(&'a mut self, offset: u64, buf: &'a mut [u8]) -> LocalBoxFuture<'a, Result<()>>;
    fn write_at<'a>(&'a mut self, offset: u64, buf: &'a [u8]) -> LocalBoxFuture<'a, Result<()>>;
    fn flush<'a>(&'a mut self) -> LocalBoxFuture<'a, Result<()>>;
}
