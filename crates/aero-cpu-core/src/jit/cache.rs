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
        Some(
            self.nodes[idx]
                .as_ref()
                .expect("LRU node must exist for map entry")
                .handle
                .clone(),
        )
    }

    pub fn insert(&mut self, handle: CompiledBlockHandle) -> Vec<u64> {
        let entry_rip = handle.entry_rip;
        let byte_len = handle.meta.byte_len as usize;

        if self.remove(entry_rip).is_some() {
            // `remove()` already adjusted bytes and unlinked the old node.
        }

        self.current_bytes = self.current_bytes.saturating_add(byte_len);
        let idx = self.alloc_node(LruNode {
            entry_rip,
            handle,
            prev: None,
            next: None,
        });
        self.map.insert(entry_rip, idx);
        self.link_front(idx);

        self.evict_if_needed()
    }

    pub fn remove(&mut self, entry_rip: u64) -> Option<CompiledBlockHandle> {
        let idx = self.map.remove(&entry_rip)?;
        let node = self.remove_idx(idx);
        debug_assert_eq!(node.entry_rip, entry_rip);
        Some(node.handle)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.nodes.clear();
        self.free_list.clear();
        self.head = None;
        self.tail = None;
        self.current_bytes = 0;
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

    fn alloc_node(&mut self, node: LruNode) -> usize {
        if let Some(idx) = self.free_list.pop() {
            self.nodes[idx] = Some(node);
            idx
        } else {
            let idx = self.nodes.len();
            self.nodes.push(Some(node));
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
}

#[derive(Debug)]
struct LruNode {
    entry_rip: u64,
    handle: CompiledBlockHandle,
    prev: Option<usize>,
    next: Option<usize>,
}
