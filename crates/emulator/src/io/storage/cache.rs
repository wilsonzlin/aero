use crate::io::storage::disk::{DiskBackend, WriteCachePolicy};
use crate::io::storage::error::{DiskError, DiskResult};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoalescedRange {
    pub lba: u64,
    pub sectors: u64,
}

/// Wrapper that batches adjacent sector reads/writes into larger backend operations.
///
/// This is primarily useful when a controller provides a scatter-gather list with many
/// small segments. Coalescing reduces the number of backend calls at the cost of an
/// extra copy through an internal scratch buffer.
pub struct CoalescingBackend<B> {
    backend: B,
    max_merge_bytes: usize,
    scratch: Vec<u8>,
}

impl<B: DiskBackend> CoalescingBackend<B> {
    pub fn new(backend: B, max_merge_bytes: usize) -> Self {
        Self {
            backend,
            max_merge_bytes,
            scratch: Vec::new(),
        }
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    fn sector_size_usize(&self) -> usize {
        self.backend.sector_size() as usize
    }
}

/// Coalesce sorted sector ranges into larger contiguous spans.
pub fn coalesce_ranges(mut ranges: Vec<CoalescedRange>) -> Vec<CoalescedRange> {
    if ranges.is_empty() {
        return ranges;
    }
    ranges.sort_by_key(|r| r.lba);

    let mut out = Vec::with_capacity(ranges.len());
    let mut cur = ranges[0];
    for next in ranges.into_iter().skip(1) {
        let cur_end = cur.lba.saturating_add(cur.sectors);
        if next.lba == cur_end {
            cur.sectors = cur.sectors.saturating_add(next.sectors);
            continue;
        }
        out.push(cur);
        cur = next;
    }
    out.push(cur);
    out
}

impl<B: DiskBackend> DiskBackend for CoalescingBackend<B> {
    fn sector_size(&self) -> u32 {
        self.backend.sector_size()
    }

    fn total_sectors(&self) -> u64 {
        self.backend.total_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.backend.read_sectors(lba, buf)
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.backend.write_sectors(lba, buf)
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.backend.flush()
    }

    fn readv_sectors(&mut self, mut lba: u64, bufs: &mut [&mut [u8]]) -> DiskResult<()> {
        let sector_size = self.sector_size_usize();
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }

        let max_merge = self.max_merge_bytes.max(sector_size);

        let mut idx = 0usize;
        while idx < bufs.len() {
            // Validate alignment up-front.
            let buf_len = bufs[idx].len();
            if !buf_len.is_multiple_of(sector_size) {
                return Err(DiskError::UnalignedBuffer {
                    len: buf_len,
                    sector_size: sector_size as u32,
                });
            }
            if buf_len > max_merge {
                self.backend.read_sectors(lba, bufs[idx])?;
                let sectors = (buf_len / sector_size) as u64;
                lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
                    lba,
                    sectors,
                    capacity_sectors: self.backend.total_sectors(),
                })?;
                idx += 1;
                continue;
            }

            let start = idx;
            let mut merged_len = 0usize;
            while idx < bufs.len() {
                let next_len = bufs[idx].len();
                if !next_len.is_multiple_of(sector_size) {
                    return Err(DiskError::UnalignedBuffer {
                        len: next_len,
                        sector_size: sector_size as u32,
                    });
                }
                if merged_len != 0 {
                    let next_merged = merged_len
                        .checked_add(next_len)
                        .ok_or(DiskError::Unsupported("coalesced IO too large"))?;
                    if next_merged > max_merge {
                        break;
                    }
                    merged_len = next_merged;
                } else {
                    merged_len = next_len;
                }
                idx += 1;
            }

            if idx - start == 1 {
                self.backend.read_sectors(lba, bufs[start])?;
            } else if merged_len != 0 {
                self.scratch.resize(merged_len, 0);
                self.backend.read_sectors(lba, &mut self.scratch)?;
                let mut off = 0usize;
                for buf in &mut bufs[start..idx] {
                    let len = buf.len();
                    buf.copy_from_slice(&self.scratch[off..off + len]);
                    off += len;
                }
            }

            let sectors = (merged_len / sector_size) as u64;
            lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.backend.total_sectors(),
            })?;
        }
        Ok(())
    }

    fn writev_sectors(&mut self, mut lba: u64, bufs: &[&[u8]]) -> DiskResult<()> {
        let sector_size = self.sector_size_usize();
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }
        let max_merge = self.max_merge_bytes.max(sector_size);

        let mut idx = 0usize;
        while idx < bufs.len() {
            let buf_len = bufs[idx].len();
            if !buf_len.is_multiple_of(sector_size) {
                return Err(DiskError::UnalignedBuffer {
                    len: buf_len,
                    sector_size: sector_size as u32,
                });
            }
            if buf_len > max_merge {
                self.backend.write_sectors(lba, bufs[idx])?;
                let sectors = (buf_len / sector_size) as u64;
                lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
                    lba,
                    sectors,
                    capacity_sectors: self.backend.total_sectors(),
                })?;
                idx += 1;
                continue;
            }

            let start = idx;
            let mut merged_len = 0usize;
            while idx < bufs.len() {
                let next_len = bufs[idx].len();
                if !next_len.is_multiple_of(sector_size) {
                    return Err(DiskError::UnalignedBuffer {
                        len: next_len,
                        sector_size: sector_size as u32,
                    });
                }
                if merged_len != 0 {
                    let next_merged = merged_len
                        .checked_add(next_len)
                        .ok_or(DiskError::Unsupported("coalesced IO too large"))?;
                    if next_merged > max_merge {
                        break;
                    }
                    merged_len = next_merged;
                } else {
                    merged_len = next_len;
                }
                idx += 1;
            }

            if idx - start == 1 {
                self.backend.write_sectors(lba, bufs[start])?;
            } else if merged_len != 0 {
                self.scratch.resize(merged_len, 0);
                let mut off = 0usize;
                for buf in &bufs[start..idx] {
                    let len = buf.len();
                    self.scratch[off..off + len].copy_from_slice(buf);
                    off += len;
                }
                self.backend.write_sectors(lba, &self.scratch)?;
            }

            let sectors = (merged_len / sector_size) as u64;
            lba = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.backend.total_sectors(),
            })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct BlockCacheConfig {
    pub block_size: u32,
    pub max_blocks: usize,
    pub write_policy: WriteCachePolicy,
}

impl BlockCacheConfig {
    pub fn new(block_size: u32, max_blocks: usize) -> Self {
        Self {
            block_size,
            max_blocks,
            write_policy: WriteCachePolicy::WriteBack,
        }
    }

    pub fn write_policy(mut self, policy: WriteCachePolicy) -> Self {
        self.write_policy = policy;
        self
    }
}

struct CacheEntry {
    data: Vec<u8>,
    dirty: bool,
    last_touch: u64,
}

pub struct BlockCache<B> {
    backend: B,
    sector_size: u32,
    capacity_sectors: u64,
    config: BlockCacheConfig,
    entries: HashMap<u64, CacheEntry>,
    lru: VecDeque<(u64, u64)>,
    touch_counter: u64,
    dirty: HashSet<u64>,
}

impl<B: DiskBackend> BlockCache<B> {
    const LRU_COMPACT_FACTOR: usize = 16;

    fn try_clone_bytes(src: &[u8]) -> DiskResult<Vec<u8>> {
        let mut out = Vec::new();
        out.try_reserve_exact(src.len())
            .map_err(|_| DiskError::QuotaExceeded)?;
        out.extend_from_slice(src);
        Ok(out)
    }

    pub fn new(backend: B, config: BlockCacheConfig) -> DiskResult<Self> {
        let sector_size = backend.sector_size();
        if sector_size == 0 {
            return Err(DiskError::Unsupported("sector size must be non-zero"));
        }
        if config.block_size == 0 || !(config.block_size as u64).is_multiple_of(sector_size as u64)
        {
            return Err(DiskError::Unsupported(
                "cache block size must be a multiple of backend sector size",
            ));
        }
        if config.max_blocks == 0 {
            return Err(DiskError::Unsupported("cache must have at least 1 block"));
        }
        let capacity_sectors = backend.capacity_sectors();
        Ok(Self {
            backend,
            sector_size,
            capacity_sectors,
            config,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            touch_counter: 0,
            dirty: HashSet::new(),
        })
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    pub fn flush_some(&mut self, max_blocks: usize) -> DiskResult<usize> {
        let mut flushed = 0usize;
        for _ in 0..max_blocks {
            let key = match self.dirty.iter().next().copied() {
                Some(k) => k,
                None => break,
            };
            self.flush_block(key)?;
            flushed += 1;
        }
        Ok(flushed)
    }

    fn next_touch(&mut self) -> u64 {
        self.touch_counter = self.touch_counter.wrapping_add(1);
        self.touch_counter
    }

    fn touch(&mut self, block: u64) {
        let t = self.next_touch();
        if let Some(entry) = self.entries.get_mut(&block) {
            entry.last_touch = t;
        }
        self.lru.push_back((block, t));
        self.maybe_compact_lru();
    }

    fn maybe_compact_lru(&mut self) {
        // `VecDeque` of (block,touch) is cheap to append, but if the cache hot set fits
        // within `max_blocks` the queue can grow without bound. Periodically rebuild
        // it from `entries` to keep memory usage stable.
        let threshold = self
            .config
            .max_blocks
            .saturating_mul(Self::LRU_COMPACT_FACTOR);
        if threshold == 0 || self.lru.len() <= threshold {
            return;
        }

        let mut pairs: Vec<(u64, u64)> = self
            .entries
            .iter()
            .map(|(&block, entry)| (block, entry.last_touch))
            .collect();
        pairs.sort_by_key(|&(_, touch)| touch);
        self.lru = VecDeque::from(pairs);
    }

    fn evict_if_needed(&mut self) -> DiskResult<()> {
        while self.entries.len() > self.config.max_blocks {
            let (block, touch) = match self.lru.pop_front() {
                Some(v) => v,
                None => break,
            };
            let should_evict = self
                .entries
                .get(&block)
                .map(|e| e.last_touch == touch)
                .unwrap_or(false);
            if !should_evict {
                continue;
            }
            if self.dirty.contains(&block) {
                self.flush_block(block)?;
            }
            self.entries.remove(&block);
        }
        Ok(())
    }

    fn flush_block(&mut self, block: u64) -> DiskResult<()> {
        let sectors_per_block = self.sectors_per_block();
        let entry = match self.entries.get_mut(&block) {
            Some(e) => e,
            None => {
                self.dirty.remove(&block);
                return Ok(());
            }
        };
        if !entry.dirty {
            self.dirty.remove(&block);
            return Ok(());
        }
        let lba = block.saturating_mul(sectors_per_block);
        self.backend.write_sectors(lba, &entry.data)?;
        entry.dirty = false;
        self.dirty.remove(&block);
        Ok(())
    }

    fn sectors_per_block(&self) -> u64 {
        (self.config.block_size as u64) / self.sector_size as u64
    }

    fn load_block(&mut self, block: u64) -> DiskResult<()> {
        if self.entries.contains_key(&block) {
            return Ok(());
        }
        let block_size = self.config.block_size as usize;
        let mut data = Vec::new();
        data.try_reserve_exact(block_size)
            .map_err(|_| DiskError::QuotaExceeded)?;
        data.resize(block_size, 0);
        let lba = block.saturating_mul(self.sectors_per_block());
        // Don't read past end of disk.
        let max_lba = self.capacity_sectors;
        if lba < max_lba {
            let remaining_sectors = max_lba - lba;
            let want_sectors = self.sectors_per_block();
            let read_sectors = remaining_sectors.min(want_sectors);
            let bytes = (read_sectors as usize) * self.sector_size as usize;
            self.backend.read_sectors(lba, &mut data[..bytes])?;
            if bytes < data.len() {
                data[bytes..].fill(0);
            }
        }
        self.entries.insert(
            block,
            CacheEntry {
                data,
                dirty: false,
                last_touch: 0,
            },
        );
        Ok(())
    }

    fn block_for_lba(&self, lba: u64) -> u64 {
        lba / self.sectors_per_block()
    }

    fn offset_in_block_bytes(&self, lba: u64) -> usize {
        let sector_off = (lba % self.sectors_per_block()) as usize;
        sector_off * self.sector_size as usize
    }
}

impl<B: DiskBackend> DiskBackend for BlockCache<B> {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        if !buf.len().is_multiple_of(self.sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buf.len() / self.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.capacity_sectors,
        })?;
        if end > self.capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.capacity_sectors,
            });
        }

        let mut remaining = buf;
        let mut cur_lba = lba;
        while !remaining.is_empty() {
            let block = self.block_for_lba(cur_lba);
            self.load_block(block)?;
            let off = self.offset_in_block_bytes(cur_lba);
            let max_in_block = self.config.block_size as usize - off;
            let to_copy = max_in_block.min(remaining.len());
            let entry = self.entries.get(&block).unwrap();
            remaining[..to_copy].copy_from_slice(&entry.data[off..off + to_copy]);
            self.touch(block);
            self.evict_if_needed()?;
            remaining = &mut remaining[to_copy..];
            cur_lba += (to_copy as u64) / self.sector_size as u64;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        if !buf.len().is_multiple_of(self.sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: buf.len(),
                sector_size: self.sector_size,
            });
        }
        let sectors = (buf.len() / self.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.capacity_sectors,
        })?;
        if end > self.capacity_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.capacity_sectors,
            });
        }

        let mut remaining = buf;
        let mut cur_lba = lba;
        let sectors_per_block = self.sectors_per_block();
        while !remaining.is_empty() {
            let block = self.block_for_lba(cur_lba);
            let off = self.offset_in_block_bytes(cur_lba);
            let max_in_block = self.config.block_size as usize - off;
            let to_copy = max_in_block.min(remaining.len());
            let touch = self.next_touch();

            // Fast path: full-block overwrite can skip the read.
            if off == 0
                && to_copy == self.config.block_size as usize
                && !self.entries.contains_key(&block)
            {
                match self.config.write_policy {
                    WriteCachePolicy::WriteThrough => {
                        let start_lba = block.saturating_mul(sectors_per_block);
                        self.backend
                            .write_sectors(start_lba, &remaining[..to_copy])?;
                        self.entries.insert(
                            block,
                            CacheEntry {
                                data: Self::try_clone_bytes(&remaining[..to_copy])?,
                                dirty: false,
                                last_touch: touch,
                            },
                        );
                    }
                    WriteCachePolicy::WriteBack => {
                        self.entries.insert(
                            block,
                            CacheEntry {
                                data: Self::try_clone_bytes(&remaining[..to_copy])?,
                                dirty: true,
                                last_touch: touch,
                            },
                        );
                        self.dirty.insert(block);
                    }
                }
                self.lru.push_back((block, touch));
                self.maybe_compact_lru();
                self.evict_if_needed()?;

                remaining = &remaining[to_copy..];
                cur_lba += sectors_per_block;
                continue;
            }

            self.load_block(block)?;
            {
                let entry = self.entries.get_mut(&block).unwrap();
                entry.data[off..off + to_copy].copy_from_slice(&remaining[..to_copy]);
                entry.last_touch = touch;

                match self.config.write_policy {
                    WriteCachePolicy::WriteThrough => {
                        let start_lba = block.saturating_mul(sectors_per_block);
                        self.backend.write_sectors(start_lba, &entry.data)?;
                        entry.dirty = false;
                        self.dirty.remove(&block);
                    }
                    WriteCachePolicy::WriteBack => {
                        if !entry.dirty {
                            entry.dirty = true;
                            self.dirty.insert(block);
                        }
                    }
                }
            }
            self.lru.push_back((block, touch));
            self.maybe_compact_lru();
            self.evict_if_needed()?;

            remaining = &remaining[to_copy..];
            cur_lba += (to_copy as u64) / self.sector_size as u64;
        }
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        // Flush dirty blocks in LBA order for determinism.
        let mut dirty_blocks: Vec<u64> = self.dirty.iter().copied().collect();
        dirty_blocks.sort_unstable();
        for block in dirty_blocks {
            self.flush_block(block)?;
        }
        self.backend.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::storage::disk::DiskBackend;

    #[derive(Default)]
    struct RecordingDisk {
        sector_size: u32,
        sectors: u64,
        data: Vec<u8>,
        log: Vec<String>,
    }

    impl RecordingDisk {
        fn new(sector_size: u32, sectors: u64) -> Self {
            Self {
                sector_size,
                sectors,
                data: vec![0u8; sectors as usize * sector_size as usize],
                log: Vec::new(),
            }
        }
    }

    impl DiskBackend for RecordingDisk {
        fn sector_size(&self) -> u32 {
            self.sector_size
        }

        fn total_sectors(&self) -> u64 {
            self.sectors
        }

        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
            self.log.push(format!("read:{lba}:{}", buf.len()));
            let offset = lba as usize * self.sector_size as usize;
            buf.copy_from_slice(&self.data[offset..offset + buf.len()]);
            Ok(())
        }

        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
            self.log.push(format!("write:{lba}:{}", buf.len()));
            let offset = lba as usize * self.sector_size as usize;
            self.data[offset..offset + buf.len()].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> DiskResult<()> {
            self.log.push("flush".to_string());
            Ok(())
        }
    }

    #[test]
    fn coalesce_merges_contiguous_ranges() {
        let ranges = vec![
            CoalescedRange { lba: 0, sectors: 1 },
            CoalescedRange { lba: 1, sectors: 2 },
            CoalescedRange {
                lba: 10,
                sectors: 1,
            },
            CoalescedRange {
                lba: 11,
                sectors: 1,
            },
        ];
        let out = coalesce_ranges(ranges);
        assert_eq!(
            out,
            vec![
                CoalescedRange { lba: 0, sectors: 3 },
                CoalescedRange {
                    lba: 10,
                    sectors: 2
                }
            ]
        );
    }

    #[test]
    fn dirty_flush_happens_on_eviction() {
        let backend = RecordingDisk::new(512, 16);
        let config = BlockCacheConfig::new(512, 1).write_policy(WriteCachePolicy::WriteBack);
        let mut cache = BlockCache::new(backend, config).unwrap();

        cache.write_sectors(0, &[1u8; 512]).unwrap();
        assert!(cache.entries.contains_key(&0));
        assert!(cache.dirty.contains(&0));
        // Writing to a different block forces eviction of block 0 (capacity 1).
        cache.write_sectors(1, &[2u8; 512]).unwrap();

        let backend = cache.into_backend();
        // We should have flushed block 0 exactly once due to eviction, but not necessarily block 1.
        assert!(backend.log.iter().any(|l| l.starts_with("write:0:")));
    }

    #[test]
    fn flush_writes_before_backend_flush() {
        let backend = RecordingDisk::new(512, 8);
        let config = BlockCacheConfig::new(512, 4).write_policy(WriteCachePolicy::WriteBack);
        let mut cache = BlockCache::new(backend, config).unwrap();
        cache.write_sectors(0, &[3u8; 512]).unwrap();
        cache.write_sectors(1, &[4u8; 512]).unwrap();
        cache.flush().unwrap();
        let backend = cache.into_backend();
        let flush_pos = backend.log.iter().position(|l| l == "flush").unwrap();
        assert!(backend.log[..flush_pos]
            .iter()
            .any(|l| l.starts_with("write:0:")));
    }

    #[test]
    fn lru_does_not_grow_without_bound() {
        let backend = RecordingDisk::new(512, 8);
        let config = BlockCacheConfig::new(512, 2).write_policy(WriteCachePolicy::WriteBack);
        let mut cache = BlockCache::new(backend, config).unwrap();

        let mut buf = vec![0u8; 512];
        for _ in 0..1024 {
            cache.read_sectors(0, &mut buf).unwrap();
        }

        // The implementation periodically rebuilds the LRU queue, so its size should remain
        // proportional to `max_blocks` even under repeated hits.
        assert!(
            cache.lru.len()
                <= cache.config.max_blocks * BlockCache::<RecordingDisk>::LRU_COMPACT_FACTOR
        );
    }

    #[test]
    fn coalescing_backend_merges_writev_calls() {
        let backend = RecordingDisk::new(512, 8);
        let mut disk = CoalescingBackend::new(backend, 4096);

        let buf0 = vec![1u8; 512];
        let buf1 = vec![2u8; 512];
        disk.writev_sectors(0, &[buf0.as_slice(), buf1.as_slice()])
            .unwrap();

        let backend = disk.into_backend();
        let writes: Vec<_> = backend
            .log
            .iter()
            .filter(|l| l.starts_with("write:"))
            .collect();
        assert_eq!(writes, vec!["write:0:1024"]);
    }

    #[test]
    fn coalescing_backend_merges_readv_calls() {
        let mut backend = RecordingDisk::new(512, 8);
        backend.data[0..512].fill(0xAA);
        backend.data[512..1024].fill(0xBB);

        let mut disk = CoalescingBackend::new(backend, 4096);

        let mut a = vec![0u8; 512];
        let mut b = vec![0u8; 512];
        let mut bufs: Vec<&mut [u8]> = vec![a.as_mut_slice(), b.as_mut_slice()];
        disk.readv_sectors(0, &mut bufs).unwrap();

        assert!(a.iter().all(|v| *v == 0xAA));
        assert!(b.iter().all(|v| *v == 0xBB));

        let backend = disk.into_backend();
        let reads: Vec<_> = backend
            .log
            .iter()
            .filter(|l| l.starts_with("read:"))
            .collect();
        assert_eq!(reads, vec!["read:0:1024"]);
    }

    #[test]
    fn block_cache_rejects_zero_sector_size_backend() {
        struct ZeroSectorSizeDisk;

        impl DiskBackend for ZeroSectorSizeDisk {
            fn sector_size(&self) -> u32 {
                0
            }

            fn total_sectors(&self) -> u64 {
                1
            }

            fn read_sectors(&mut self, _lba: u64, _buf: &mut [u8]) -> DiskResult<()> {
                Ok(())
            }

            fn write_sectors(&mut self, _lba: u64, _buf: &[u8]) -> DiskResult<()> {
                Ok(())
            }

            fn flush(&mut self) -> DiskResult<()> {
                Ok(())
            }
        }

        let backend = ZeroSectorSizeDisk;
        let config = BlockCacheConfig::new(512, 1);
        assert!(matches!(
            BlockCache::new(backend, config),
            Err(DiskError::Unsupported("sector size must be non-zero"))
        ));
    }
}
