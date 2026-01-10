use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct HotnessProfile {
    threshold: u32,
    counters: HashMap<u64, u32>,
    requested: HashSet<u64>,
}

impl HotnessProfile {
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold,
            counters: HashMap::new(),
            requested: HashSet::new(),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    pub fn counter(&self, entry_rip: u64) -> u32 {
        self.counters.get(&entry_rip).copied().unwrap_or(0)
    }

    pub fn clear_requested(&mut self, entry_rip: u64) {
        self.requested.remove(&entry_rip);
    }

    pub fn mark_requested(&mut self, entry_rip: u64) {
        self.requested.insert(entry_rip);
    }

    pub fn record_hit(&mut self, entry_rip: u64, has_compiled_block: bool) -> bool {
        let counter = self.counters.entry(entry_rip).or_insert(0);
        *counter = counter.saturating_add(1);

        if has_compiled_block {
            return false;
        }

        if *counter >= self.threshold && !self.requested.contains(&entry_rip) {
            self.requested.insert(entry_rip);
            return true;
        }

        false
    }
}
