use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageVersionSnapshot {
    pub page: u64,
    pub version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBlockMeta {
    pub code_paddr: u64,
    pub byte_len: u32,
    pub page_versions: Vec<PageVersionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledBlockHandle {
    pub entry_rip: u64,
    pub table_index: u32,
    pub meta: CompiledBlockMeta,
}

#[derive(Debug)]
pub struct CodeCache {
    max_blocks: usize,
    max_bytes: usize,
    current_bytes: usize,
    map: HashMap<u64, CompiledBlockHandle>,
    lru: VecDeque<u64>,
}

impl CodeCache {
    pub fn new(max_blocks: usize, max_bytes: usize) -> Self {
        Self {
            max_blocks,
            max_bytes,
            current_bytes: 0,
            map: HashMap::new(),
            lru: VecDeque::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn contains(&self, entry_rip: u64) -> bool {
        self.map.contains_key(&entry_rip)
    }

    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub fn get_cloned(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        if self.map.contains_key(&entry_rip) {
            self.touch(entry_rip);
        }
        self.map.get(&entry_rip).cloned()
    }

    pub fn insert(&mut self, handle: CompiledBlockHandle) -> Vec<u64> {
        let entry_rip = handle.entry_rip;
        let byte_len = handle.meta.byte_len as usize;

        if let Some(prev) = self.map.insert(entry_rip, handle) {
            let prev_len = prev.meta.byte_len as usize;
            self.current_bytes = self.current_bytes.saturating_sub(prev_len);
            self.remove_from_lru(entry_rip);
        }

        self.current_bytes = self.current_bytes.saturating_add(byte_len);
        self.lru.push_front(entry_rip);

        self.evict_if_needed()
    }

    pub fn remove(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        let removed = self.map.remove(&entry_rip)?;
        self.current_bytes = self
            .current_bytes
            .saturating_sub(removed.meta.byte_len as usize);
        self.remove_from_lru(entry_rip);
        Some(removed)
    }

    fn touch(&mut self, entry_rip: u64) {
        self.remove_from_lru(entry_rip);
        self.lru.push_front(entry_rip);
    }

    fn remove_from_lru(&mut self, entry_rip: u64) {
        if let Some(pos) = self.lru.iter().position(|&k| k == entry_rip) {
            self.lru.remove(pos);
        }
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
                self.current_bytes = self
                    .current_bytes
                    .saturating_sub(removed.meta.byte_len as usize);
                evicted.push(key);
            }
        }
        evicted
    }
}
