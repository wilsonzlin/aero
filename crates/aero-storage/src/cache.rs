use crate::util::checked_range;
use crate::{DiskError, Result, VirtualDisk};
use lru::LruCache;
use std::num::NonZeroUsize;

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub writebacks: u64,
}

struct CacheEntry {
    data: Vec<u8>,
    dirty: bool,
}

/// A simple LRU, write-back cache in front of a [`VirtualDisk`].
///
/// The cache works in fixed-size blocks (e.g. 1 MiB). This reduces the overhead of
/// calling into browser storage APIs for many tiny sector operations.
pub struct BlockCachedDisk<D> {
    inner: D,
    block_size: usize,
    cache: LruCache<u64, CacheEntry>,
    stats: BlockCacheStats,
}

impl<D: VirtualDisk> BlockCachedDisk<D> {
    pub fn new(inner: D, block_size: usize, max_cached_blocks: usize) -> Result<Self> {
        if block_size == 0 {
            return Err(DiskError::InvalidConfig("block_size must be > 0"));
        }
        let max_cached_blocks = NonZeroUsize::new(max_cached_blocks)
            .ok_or(DiskError::InvalidConfig("max_cached_blocks must be > 0"))?;
        Ok(Self {
            inner,
            block_size,
            cache: LruCache::new(max_cached_blocks),
            stats: BlockCacheStats::default(),
        })
    }

    pub fn stats(&self) -> BlockCacheStats {
        self.stats
    }

    pub fn inner(&self) -> &D {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    pub fn into_inner(self) -> D {
        self.inner
    }

    fn ensure_block_cached(&mut self, block_idx: u64) -> Result<()> {
        if self.cache.get_mut(&block_idx).is_some() {
            self.stats.hits += 1;
            return Ok(());
        }
        self.stats.misses += 1;

        let mut data = Vec::new();
        data.try_reserve_exact(self.block_size)
            .map_err(|_| DiskError::QuotaExceeded)?;
        data.resize(self.block_size, 0);
        let mut entry = CacheEntry {
            data,
            dirty: false,
        };

        let start = block_idx
            .checked_mul(self.block_size as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if start < self.inner.capacity_bytes() {
            let max_len = (self.inner.capacity_bytes() - start).min(self.block_size as u64);
            self.inner
                .read_at(start, &mut entry.data[..max_len as usize])?;
        }

        if let Some((evicted_idx, evicted)) = self.cache.push(block_idx, entry) {
            self.stats.evictions += 1;
            self.write_back_block(evicted_idx, evicted)?;
        }
        Ok(())
    }

    fn write_back_block(&mut self, block_idx: u64, entry: CacheEntry) -> Result<()> {
        if !entry.dirty {
            return Ok(());
        }
        self.stats.writebacks += 1;
        let start = block_idx
            .checked_mul(self.block_size as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        if start >= self.inner.capacity_bytes() {
            return Ok(());
        }
        let max_len = (self.inner.capacity_bytes() - start).min(self.block_size as u64);
        self.inner
            .write_at(start, &entry.data[..max_len as usize])?;
        Ok(())
    }
}

impl<D: VirtualDisk> VirtualDisk for BlockCachedDisk<D> {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / self.block_size as u64;
            let within = (abs % self.block_size as u64) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (self.block_size - within).min(remaining);

            self.ensure_block_cached(block_idx)?;
            let entry = self
                .cache
                .get_mut(&block_idx)
                .ok_or(DiskError::Io(
                    "cache missing block after ensure_block_cached".into(),
                ))?;
            buf[pos..pos + chunk_len].copy_from_slice(&entry.data[within..within + chunk_len]);

            pos += chunk_len;
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / self.block_size as u64;
            let within = (abs % self.block_size as u64) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (self.block_size - within).min(remaining);

            self.ensure_block_cached(block_idx)?;
            let entry = self
                .cache
                .get_mut(&block_idx)
                .ok_or(DiskError::Io(
                    "cache missing block after ensure_block_cached".into(),
                ))?;
            entry.data[within..within + chunk_len].copy_from_slice(&buf[pos..pos + chunk_len]);
            entry.dirty = true;

            pos += chunk_len;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // Snapshot keys so we can iterate while mutating entries.
        let keys: Vec<u64> = self.cache.iter().map(|(k, _)| *k).collect();
        for key in keys {
            let start = key
                .checked_mul(self.block_size as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            if start >= self.inner.capacity_bytes() {
                continue;
            }
            let max_len =
                (self.inner.capacity_bytes() - start).min(self.block_size as u64) as usize;

            let entry = self
                .cache
                .get_mut(&key)
                .ok_or(DiskError::Io("cache missing key during flush".into()))?;
            if !entry.dirty {
                continue;
            }

            self.stats.writebacks += 1;
            self.inner.write_at(start, &entry.data[..max_len])?;
            entry.dirty = false;
        }

        self.inner.flush()
    }
}
