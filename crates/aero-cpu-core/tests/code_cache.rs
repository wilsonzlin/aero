use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};
use std::collections::{HashMap, VecDeque};

fn handle(entry_rip: u64, byte_len: u32) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index: entry_rip as u32,
        meta: CompiledBlockMeta {
            code_paddr: entry_rip,
            byte_len,
            page_versions_generation: 0,
            page_versions: Vec::new(),
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        },
    }
}

#[test]
fn code_cache_get_cloned_updates_recency() {
    // Ensure recency affects eviction even when eviction is driven by the *byte* cap rather than
    // max block count.
    let mut cache = CodeCache::new(10, 25);
    assert!(cache.insert(handle(0, 10)).is_empty());
    assert!(cache.insert(handle(1, 10)).is_empty());
    assert_eq!(cache.current_bytes(), 20);

    // Touch the LRU entry to make it MRU; the next insert should evict `1`, not `0`.
    assert!(cache.get_cloned(0).is_some());

    let evicted = cache.insert(handle(2, 10));
    assert_eq!(evicted, vec![1]);
    assert!(cache.contains(0));
    assert!(!cache.contains(1));
    assert!(cache.contains(2));
    assert_eq!(cache.current_bytes(), 20);
}

#[test]
fn code_cache_insert_replacing_entry_updates_bytes_and_recency() {
    let mut cache = CodeCache::new(2, 0);
    assert!(cache.insert(handle(0, 10)).is_empty());
    assert!(cache.insert(handle(1, 10)).is_empty());
    assert_eq!(cache.current_bytes(), 20);

    // Replacing an existing entry should:
    // - update byte accounting (subtract old size, add new size)
    // - treat the entry as MRU (so it won't be evicted next)
    let evicted = cache.insert(handle(0, 5));
    assert!(evicted.is_empty());
    assert_eq!(cache.current_bytes(), 15);
    assert_eq!(cache.get_cloned(0).unwrap().meta.byte_len, 5);

    let evicted = cache.insert(handle(2, 10));
    assert_eq!(evicted, vec![1]);
    assert!(cache.contains(0));
    assert!(cache.contains(2));
    assert!(!cache.contains(1));
    assert_eq!(cache.current_bytes(), 15);
}

#[test]
fn code_cache_matches_reference_lru_model() {
    #[derive(Debug)]
    struct RefCache {
        max_blocks: usize,
        max_bytes: usize,
        current_bytes: usize,
        map: HashMap<u64, u32>,
        lru: VecDeque<u64>,
    }

    impl RefCache {
        fn new(max_blocks: usize, max_bytes: usize) -> Self {
            Self {
                max_blocks,
                max_bytes,
                current_bytes: 0,
                map: HashMap::new(),
                lru: VecDeque::new(),
            }
        }

        fn remove_from_lru(&mut self, entry_rip: u64) {
            if let Some(pos) = self.lru.iter().position(|&k| k == entry_rip) {
                self.lru.remove(pos);
            }
        }

        fn touch(&mut self, entry_rip: u64) {
            self.remove_from_lru(entry_rip);
            self.lru.push_front(entry_rip);
        }

        fn get(&mut self, entry_rip: u64) -> Option<u32> {
            if self.map.contains_key(&entry_rip) {
                self.touch(entry_rip);
            }
            self.map.get(&entry_rip).copied()
        }

        fn insert(&mut self, entry_rip: u64, byte_len: u32) -> Vec<u64> {
            if let Some(prev) = self.map.insert(entry_rip, byte_len) {
                self.current_bytes = self.current_bytes.saturating_sub(prev as usize);
                self.remove_from_lru(entry_rip);
            }

            self.current_bytes = self.current_bytes.saturating_add(byte_len as usize);
            self.lru.push_front(entry_rip);
            self.evict_if_needed()
        }

        fn remove(&mut self, entry_rip: u64) -> Option<u32> {
            let removed = self.map.remove(&entry_rip)?;
            self.current_bytes = self.current_bytes.saturating_sub(removed as usize);
            self.remove_from_lru(entry_rip);
            Some(removed)
        }

        fn evict_if_needed(&mut self) -> Vec<u64> {
            let mut evicted = Vec::new();
            while self.map.len() > self.max_blocks
                || (self.max_bytes != 0 && self.current_bytes > self.max_bytes)
            {
                let Some(key) = self.lru.pop_back() else {
                    break;
                };
                if let Some(removed) = self.map.remove(&key) {
                    self.current_bytes = self.current_bytes.saturating_sub(removed as usize);
                    evicted.push(key);
                }
            }
            evicted
        }
    }

    #[derive(Clone, Copy)]
    struct XorShift64 {
        state: u64,
    }

    impl XorShift64 {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            x
        }
    }

    let mut rng = XorShift64::new(0x1234_5678_9abc_def0);
    let max_blocks = 5;
    let max_bytes = 50;
    let mut cache = CodeCache::new(max_blocks, max_bytes);
    let mut reference = RefCache::new(max_blocks, max_bytes);

    for step in 0..10_000u64 {
        let op = (rng.next_u64() % 3) as u8;
        let entry_rip = rng.next_u64() % 10;
        match op {
            0 => {
                // Insert
                // Mostly generate small blocks, with occasional oversize blocks to exercise the
                // "block too large for max_bytes" eviction path without constantly flushing the
                // cache (which would reduce LRU coverage).
                let r = rng.next_u64();
                let byte_len = if (r & 0x0f) == 0 {
                    // Guaranteed oversize (max_bytes is 50 in this test).
                    80
                } else {
                    (r % 20 + 1) as u32
                };
                let evicted = cache.insert(handle(entry_rip, byte_len));
                let expected = reference.insert(entry_rip, byte_len);
                assert_eq!(evicted, expected, "eviction mismatch at step {step}");
            }
            1 => {
                // Get
                let got = cache.get_cloned(entry_rip).map(|h| h.meta.byte_len);
                let expected = reference.get(entry_rip);
                assert_eq!(got, expected, "get mismatch at step {step}");
            }
            _ => {
                // Remove
                let got = cache.remove(entry_rip).map(|h| h.meta.byte_len);
                let expected = reference.remove(entry_rip);
                assert_eq!(got, expected, "remove mismatch at step {step}");
            }
        }

        assert_eq!(
            cache.len(),
            reference.map.len(),
            "len mismatch at step {step}"
        );
        assert_eq!(
            cache.current_bytes(),
            reference.current_bytes,
            "byte accounting mismatch at step {step}"
        );
        for rip in 0..10u64 {
            assert_eq!(
                cache.contains(rip),
                reference.map.contains_key(&rip),
                "contains mismatch for rip={rip} at step {step}"
            );
        }
    }
}
