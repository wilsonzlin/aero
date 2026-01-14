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

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.counters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
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
            if !self.ensure_space_for_new_counter_entry() {
                // Profile is saturated with `requested` keys (in-flight compilation or already
                // compiled blocks). Avoid evicting them so compile requests remain de-duped; just
                // stop tracking new RIPs until space is freed.
                return false;
            }

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

        if counter >= self.threshold {
            // `HashSet::insert` returns true only when the RIP wasn't already present.
            // This avoids doing a separate `contains()` probe on the cold edge when a block
            // crosses the hot threshold for the first time.
            if self.requested.insert(entry_rip) {
                return true;
            }
        }

        false
    }

    fn bump_clock(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    /// Ensure there is room to insert a new *unrequested* counter entry.
    ///
    /// Returns `false` if the profile is saturated with `requested` keys (which we avoid evicting to
    /// preserve compilation request de-duping).
    fn ensure_space_for_new_counter_entry(&mut self) -> bool {
        if self.counters.len() < self.capacity {
            return true;
        }

        let Some(victim) = self.pick_victim(/*allow_requested=*/ false) else {
            return false;
        };
        self.evict(victim);
        true
    }

    /// Ensure there is room to insert a new *requested* entry.
    ///
    /// This prefers evicting non-requested entries first, but will fall back to evicting a
    /// requested entry if the profile is saturated.
    fn ensure_space_for_new_requested_entry(&mut self) {
        if self.counters.len() < self.capacity {
            return;
        }

        if let Some(victim) = self.pick_victim(/*allow_requested=*/ false) {
            self.evict(victim);
            return;
        }

        if let Some(victim) = self.pick_victim(/*allow_requested=*/ true) {
            self.evict(victim);
        }
    }

    fn evict(&mut self, entry_rip: u64) {
        self.counters.remove(&entry_rip);
        self.requested.remove(&entry_rip);
    }

    fn pick_victim(&self, allow_requested: bool) -> Option<u64> {
        let mut victim: Option<(u32, u64, u64)> = None;
        for (&rip, entry) in &self.counters {
            if !allow_requested && self.requested.contains(&rip) {
                continue;
            }
            let key = (entry.count, entry.last_hit, rip);
            if victim.is_none_or(|v| key < v) {
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

        self.ensure_space_for_new_requested_entry();
        self.counters.insert(
            entry_rip,
            CounterEntry {
                count: min_count,
                last_hit: now,
            },
        );
    }

    pub fn reset(&mut self) {
        self.clock = 0;
        self.counters.clear();
        self.requested.clear();
    }
}
