use std::collections::hash_map::Entry;
use std::collections::HashMap;

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
    /// Architectural guest instruction count for this block (i.e. number of retired guest
    /// instructions when the block commits).
    pub instruction_count: u32,
    /// Whether the last executed instruction creates an interrupt shadow that inhibits maskable
    /// interrupts for the following instruction.
    pub inhibit_interrupts_after_block: bool,
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
    /// Maps an entry RIP to its node index inside `nodes`.
    map: HashMap<u64, usize>,
    /// LRU list storage. Nodes are linked by indices so we can update in O(1) without `unsafe`.
    nodes: Vec<Option<LruNode>>,
    /// Recycled indices inside `nodes`.
    free_list: Vec<usize>,
    /// Most recently used node index.
    head: Option<usize>,
    /// Least recently used node index.
    tail: Option<usize>,
}

impl CodeCache {
    pub fn new(max_blocks: usize, max_bytes: usize) -> Self {
        Self {
            max_blocks,
            max_bytes,
            current_bytes: 0,
            map: HashMap::new(),
            nodes: Vec::new(),
            free_list: Vec::new(),
            head: None,
            tail: None,
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, entry_rip: u64) -> bool {
        self.map.contains_key(&entry_rip)
    }

    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    pub fn get_cloned(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        let idx = *self.map.get(&entry_rip)?;
        self.touch_idx(idx);
        let out = self.nodes[idx]
            .as_ref()
            .expect("LRU node must exist for map entry")
            .handle
            .clone();
        self.debug_assert_invariants();
        Some(out)
    }

    pub fn insert(&mut self, handle: CompiledBlockHandle) -> Vec<u64> {
        let entry_rip = handle.entry_rip;
        let byte_len = handle.meta.byte_len as usize;

        let idx = match self.map.entry(entry_rip) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                self.current_bytes = self.current_bytes.saturating_add(byte_len);
                let idx = Self::alloc_node_inner(&mut self.nodes, &mut self.free_list, LruNode {
                    entry_rip,
                    handle,
                    prev: None,
                    next: None,
                });
                let _ = entry.insert(idx);
                self.link_front(idx);
                return self.evict_if_needed();
            }
        };

        let prev_len = self.nodes[idx]
            .as_ref()
            .expect("LRU node must exist for map entry")
            .handle
            .meta
            .byte_len as usize;
        self.current_bytes = self.current_bytes.saturating_sub(prev_len);
        self.current_bytes = self.current_bytes.saturating_add(byte_len);
        self.nodes[idx]
            .as_mut()
            .expect("LRU node must exist for map entry")
            .handle = handle;
        self.touch_idx(idx);
        let evicted = self.evict_if_needed();
        self.debug_assert_invariants();
        evicted
    }

    pub fn remove(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        let idx = self.map.remove(&entry_rip)?;
        let node = self.remove_idx(idx);
        debug_assert_eq!(node.entry_rip, entry_rip);
        self.debug_assert_invariants();
        Some(node.handle)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.free_list.clear();
        self.head = None;
        self.tail = None;
        self.current_bytes = 0;
        self.debug_assert_invariants();
    }

    fn evict_if_needed(&mut self) -> Vec<u64> {
        let mut evicted = Vec::new();
        while self.map.len() > self.max_blocks
            || (self.max_bytes != 0 && self.current_bytes > self.max_bytes)
        {
            let Some(idx) = self.tail else {
                break;
            };
            let key = self.nodes[idx]
                .as_ref()
                .expect("LRU tail must exist")
                .entry_rip;
            // `remove()` is O(1) and updates both list + accounting.
            let _ = self.remove(key);
            evicted.push(key);
        }
        evicted
    }

    fn touch_idx(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return;
        }
        self.unlink(idx);
        self.link_front(idx);
    }

    fn alloc_node_inner(
        nodes: &mut Vec<Option<LruNode>>,
        free_list: &mut Vec<usize>,
        node: LruNode,
    ) -> usize {
        if let Some(idx) = free_list.pop() {
            nodes[idx] = Some(node);
            idx
        } else {
            let idx = nodes.len();
            nodes.push(Some(node));
            idx
        }
    }

    fn remove_idx(&mut self, idx: usize) -> LruNode {
        self.unlink(idx);
        let node = self.nodes[idx]
            .take()
            .expect("LRU node must exist when removing");
        self.free_list.push(idx);
        self.current_bytes = self
            .current_bytes
            .saturating_sub(node.handle.meta.byte_len as usize);
        node
    }

    fn unlink(&mut self, idx: usize) {
        let (prev, next) = {
            let node = self.nodes[idx].as_ref().expect("LRU node must exist");
            (node.prev, node.next)
        };

        match prev {
            Some(prev_idx) => {
                self.nodes[prev_idx]
                    .as_mut()
                    .expect("prev node must exist")
                    .next = next;
            }
            None => {
                self.head = next;
            }
        }

        match next {
            Some(next_idx) => {
                self.nodes[next_idx]
                    .as_mut()
                    .expect("next node must exist")
                    .prev = prev;
            }
            None => {
                self.tail = prev;
            }
        }

        let node = self.nodes[idx].as_mut().expect("LRU node must exist");
        node.prev = None;
        node.next = None;
    }

    fn link_front(&mut self, idx: usize) {
        let old_head = self.head;

        {
            let node = self.nodes[idx].as_mut().expect("LRU node must exist");
            node.prev = None;
            node.next = old_head;
        }

        match old_head {
            Some(old_idx) => {
                self.nodes[old_idx]
                    .as_mut()
                    .expect("old head must exist")
                    .prev = Some(idx);
            }
            None => {
                // List was empty; this is also the tail.
                self.tail = Some(idx);
            }
        }

        self.head = Some(idx);
    }

    #[cfg(debug_assertions)]
    #[inline]
    fn debug_assert_invariants(&self) {
        // Avoid turning debug builds into O(n) per access at production cache sizes. The full
        // invariant scan is still exercised by unit tests (which typically use small caches), and
        // can be enabled by temporarily lowering this threshold while debugging.
        const MAX_VALIDATE_ENTRIES: usize = 256;

        // Empty cache must have no LRU pointers and no accounting, and all nodes should be free.
        if self.map.is_empty() {
            debug_assert_eq!(self.head, None);
            debug_assert_eq!(self.tail, None);
            debug_assert_eq!(self.current_bytes, 0);
            debug_assert_eq!(
                self.free_list.len(),
                self.nodes.len(),
                "all nodes should be free when the cache is empty"
            );
            for node in &self.nodes {
                debug_assert!(node.is_none(), "node slot should be free when cache is empty");
            }
            return;
        }

        if self.map.len() > MAX_VALIDATE_ENTRIES {
            return;
        }

        let head = self.head.expect("non-empty cache must have head");
        let tail = self.tail.expect("non-empty cache must have tail");

        // Traverse the LRU list and validate prev/next links.
        let mut in_list = vec![false; self.nodes.len()];
        let mut cur = Some(head);
        let mut prev = None;
        let mut list_len = 0usize;
        let mut bytes = 0usize;
        while let Some(idx) = cur {
            debug_assert!(idx < self.nodes.len(), "LRU index out of bounds: {idx}");
            debug_assert!(!in_list[idx], "LRU list cycle detected at idx={idx}");
            in_list[idx] = true;
            list_len += 1;
            debug_assert!(
                list_len <= self.map.len(),
                "LRU list longer than map: {list_len} > {}",
                self.map.len()
            );

            let node = self.nodes[idx].as_ref().expect("LRU node must exist");
            debug_assert_eq!(node.prev, prev, "broken LRU prev link at idx={idx}");
            debug_assert_eq!(
                self.map.get(&node.entry_rip),
                Some(&idx),
                "LRU node entry_rip {} missing/mismatched in map",
                node.entry_rip
            );
            bytes = bytes.saturating_add(node.handle.meta.byte_len as usize);
            prev = Some(idx);
            cur = node.next;
        }

        debug_assert_eq!(
            list_len,
            self.map.len(),
            "LRU list length mismatch with map"
        );
        debug_assert_eq!(
            prev,
            Some(tail),
            "LRU traversal did not end at tail"
        );
        debug_assert_eq!(
            bytes, self.current_bytes,
            "byte accounting mismatch: list sums to {bytes}, current_bytes is {}",
            self.current_bytes
        );

        // Validate free_list: no duplicates, all indices are in-bounds and refer to `None` slots.
        let mut in_free = vec![false; self.nodes.len()];
        for &idx in &self.free_list {
            debug_assert!(idx < self.nodes.len(), "free_list index out of bounds: {idx}");
            debug_assert!(!in_free[idx], "duplicate index in free_list: {idx}");
            in_free[idx] = true;
            debug_assert!(
                self.nodes[idx].is_none(),
                "free_list contains idx {idx} whose node slot is not free"
            );
            debug_assert!(
                !in_list[idx],
                "free_list contains idx {idx} that is still linked in LRU list"
            );
        }

        // Every `None` slot must be present in `free_list`.
        for (idx, node) in self.nodes.iter().enumerate() {
            if node.is_none() {
                debug_assert!(
                    in_free[idx],
                    "node slot {idx} is free but missing from free_list"
                );
            }
        }
    }

    #[cfg(not(debug_assertions))]
    #[inline]
    fn debug_assert_invariants(&self) {}
}

#[derive(Debug)]
struct LruNode {
    entry_rip: u64,
    handle: CompiledBlockHandle,
    prev: Option<usize>,
    next: Option<usize>,
}
