use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
struct CounterEntry {
    count: u32,
    last_hit: u64,
}

#[derive(Debug)]
pub struct HotnessProfile {
    threshold: u32,
    capacity: usize,
    clock: u64,
    counters: HashMap<u64, CounterEntry>,
    requested: HashSet<u64>,
}

impl HotnessProfile {
    /// Default profile capacity used by [`Self::new`].
    ///
    /// Most callers should construct this profile via [`JitRuntime`](crate::jit::runtime::JitRuntime),
    /// which derives a capacity from the JIT cache size.
    const DEFAULT_CAPACITY: usize = 4096;
    const MIN_CAPACITY: usize = 256;
    const MAX_CAPACITY: usize = 262_144;
    const CACHE_BLOCKS_MULTIPLIER: usize = 4;

    pub fn new(threshold: u32) -> Self {
        Self::new_with_capacity(threshold, Self::DEFAULT_CAPACITY)
    }

    pub fn new_with_capacity(threshold: u32, capacity: usize) -> Self {
        // Always keep at least one slot so the profile can still trigger compilation when enabled.
        let capacity = capacity.max(1);
        Self {
            threshold,
            capacity,
            clock: 0,
            counters: HashMap::with_capacity(capacity),
            requested: HashSet::with_capacity(capacity),
        }
    }

    /// Derive a bounded hotness table capacity from the JIT cache size.
    ///
    /// This keeps hotness tracking memory-bounded without adding a new config field.
    pub fn recommended_capacity(cache_max_blocks: usize) -> usize {
        let scaled = cache_max_blocks.saturating_mul(Self::CACHE_BLOCKS_MULTIPLIER);
        scaled.clamp(Self::MIN_CAPACITY, Self::MAX_CAPACITY)
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn counter(&self, entry_rip: u64) -> u32 {
        self.counters.get(&entry_rip).map(|e| e.count).unwrap_or(0)
    }

    pub fn clear_requested(&mut self, entry_rip: u64) {
        self.requested.remove(&entry_rip);
    }

    pub fn mark_requested(&mut self, entry_rip: u64) {
        // Ensure the requested set is bounded by the same table capacity.
        self.ensure_entry(entry_rip, self.threshold.max(1));
        self.requested.insert(entry_rip);
    }

    pub fn record_hit(&mut self, entry_rip: u64, has_compiled_block: bool) -> bool {
        let now = self.bump_clock();

        let counter = if let Some(entry) = self.counters.get_mut(&entry_rip) {
            entry.count = entry.count.saturating_add(1);
            entry.last_hit = now;
            entry.count
        } else {
            self.ensure_space_for_new_entry();
            self.counters.insert(
                entry_rip,
                CounterEntry {
                    count: 1,
                    last_hit: now,
                },
            );
            1
        };

        if has_compiled_block {
            return false;
        }

        if counter >= self.threshold && !self.requested.contains(&entry_rip) {
            self.requested.insert(entry_rip);
            return true;
        }

        false
    }

    fn bump_clock(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn ensure_space_for_new_entry(&mut self) {
        if self.counters.len() < self.capacity {
            return;
        }

        let Some(victim) = self.pick_victim() else {
            return;
        };
        self.counters.remove(&victim);
        self.requested.remove(&victim);
    }

    fn pick_victim(&self) -> Option<u64> {
        let mut victim: Option<(u32, u64, u64)> = None;
        for (&rip, entry) in &self.counters {
            let key = (entry.count, entry.last_hit, rip);
            if victim.map_or(true, |v| key < v) {
                victim = Some(key);
            }
        }
        victim.map(|(_, _, rip)| rip)
    }

    fn ensure_entry(&mut self, entry_rip: u64, min_count: u32) {
        let now = self.bump_clock();
        if let Some(entry) = self.counters.get_mut(&entry_rip) {
            // `mark_requested` is an access; bias eviction away from in-flight compilation keys.
            entry.last_hit = now;
            return;
        }

        self.ensure_space_for_new_entry();
        self.counters.insert(
            entry_rip,
            CounterEntry {
                count: min_count,
                last_hit: now,
            },
        );
    }
}
