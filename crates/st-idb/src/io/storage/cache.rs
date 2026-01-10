use lru::LruCache;
use std::num::NonZeroUsize;

/// Fixed-size block held in the cache.
#[derive(Debug)]
pub struct CachedBlock {
    pub data: Vec<u8>,
    pub dirty: bool,
}

/// In-memory LRU cache tuned for fixed-size blocks.
///
/// `max_resident_bytes` is rounded down to a whole number of blocks (with a
/// minimum of 1 block) so resident memory is bounded even for large sparse
/// disks.
pub struct BlockCache {
    block_size: usize,
    max_blocks: NonZeroUsize,
    lru: LruCache<u64, CachedBlock>,
}

impl BlockCache {
    pub fn new(block_size: usize, max_resident_bytes: usize) -> Self {
        assert!(block_size > 0);

        let mut max_blocks = max_resident_bytes / block_size;
        if max_blocks == 0 {
            max_blocks = 1;
        }
        let max_blocks = NonZeroUsize::new(max_blocks).expect("non-zero");
        Self {
            block_size,
            max_blocks,
            lru: LruCache::new(max_blocks),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn max_blocks(&self) -> NonZeroUsize {
        self.max_blocks
    }

    pub fn len(&self) -> usize {
        self.lru.len()
    }

    pub fn contains(&self, idx: &u64) -> bool {
        self.lru.contains(idx)
    }

    pub fn get(&mut self, idx: &u64) -> Option<&CachedBlock> {
        self.lru.get(idx)
    }

    pub fn get_mut(&mut self, idx: &u64) -> Option<&mut CachedBlock> {
        self.lru.get_mut(idx)
    }

    pub fn peek(&self, idx: &u64) -> Option<&CachedBlock> {
        self.lru.peek(idx)
    }

    pub fn peek_mut(&mut self, idx: &u64) -> Option<&mut CachedBlock> {
        self.lru.peek_mut(idx)
    }

    pub fn put(&mut self, idx: u64, block: CachedBlock) -> Option<CachedBlock> {
        self.lru.put(idx, block)
    }

    pub fn pop_lru(&mut self) -> Option<(u64, CachedBlock)> {
        self.lru.pop_lru()
    }

    pub fn pop(&mut self, idx: &u64) -> Option<CachedBlock> {
        self.lru.pop(idx)
    }
}
