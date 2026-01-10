use crate::Result;

pub mod backends;
pub mod cache;

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskBackendStats {
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub blocks_read: u64,
    pub blocks_written: u64,
}

/// Minimal async storage interface used by the block cache.
///
/// The surrounding project (ST-CORE) can adopt this trait, or this crate can be
/// adapted to match ST-CORE's final interface.
pub trait DiskBackend {
    fn capacity(&self) -> u64;
    fn stats(&self) -> DiskBackendStats;

    fn block_size(&self) -> usize;

    fn read_at<'a>(
        &'a mut self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> impl std::future::Future<Output = Result<()>> + 'a;
    fn write_at<'a>(
        &'a mut self,
        offset: u64,
        buf: &'a [u8],
    ) -> impl std::future::Future<Output = Result<()>> + 'a;
    fn flush<'a>(&'a mut self) -> impl std::future::Future<Output = Result<()>> + 'a;
}
