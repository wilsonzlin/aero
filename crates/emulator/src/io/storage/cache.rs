use crate::io::storage::disk::{DiskBackend, WriteCachePolicy};
use crate::io::storage::error::{DiskError, DiskResult};
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoalescedRange {
    pub lba: u64,
    pub sectors: u64,
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
    pub fn new(backend: B, config: BlockCacheConfig) -> DiskResult<Self> {
        let sector_size = backend.sector_size();
        if config.block_size == 0 || (config.block_size as u64) % sector_size as u64 != 0 {
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

    fn touch(&mut self, block: u64) {
        self.touch_counter = self.touch_counter.wrapping_add(1);
        let t = self.touch_counter;
        if let Some(entry) = self.entries.get_mut(&block) {
            entry.last_touch = t;
        }
        self.lru.push_back((block, t));
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

    fn read_block(&mut self, block: u64) -> DiskResult<()> {
        if self.entries.contains_key(&block) {
            self.touch(block);
            return Ok(());
        }
        let mut data = vec![0u8; self.config.block_size as usize];
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
        self.touch_counter = self.touch_counter.wrapping_add(1);
        let t = self.touch_counter;
        self.entries.insert(
            block,
            CacheEntry {
                data,
                dirty: false,
                last_touch: t,
            },
        );
        self.lru.push_back((block, t));
        self.evict_if_needed()
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
        if buf.len() % self.sector_size as usize != 0 {
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
            self.read_block(block)?;
            let off = self.offset_in_block_bytes(cur_lba);
            let max_in_block = self.config.block_size as usize - off;
            let to_copy = max_in_block.min(remaining.len());
            let entry = self.entries.get(&block).unwrap();
            remaining[..to_copy].copy_from_slice(&entry.data[off..off + to_copy]);
            self.touch(block);
            remaining = &mut remaining[to_copy..];
            cur_lba += (to_copy as u64) / self.sector_size as u64;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        if buf.len() % self.sector_size as usize != 0 {
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
            self.read_block(block)?;
            let off = self.offset_in_block_bytes(cur_lba);
            let max_in_block = self.config.block_size as usize - off;
            let to_copy = max_in_block.min(remaining.len());
            self.touch_counter = self.touch_counter.wrapping_add(1);
            let touch = self.touch_counter;
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

            remaining = &remaining[to_copy..];
            cur_lba += (to_copy as u64) / self.sector_size as u64;
        }
        self.evict_if_needed()
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
}
