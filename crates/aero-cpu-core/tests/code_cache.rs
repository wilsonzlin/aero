use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};

fn handle(entry_rip: u64) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index: entry_rip as u32,
        meta: CompiledBlockMeta {
            code_paddr: entry_rip,
            byte_len: 1,
            page_versions: Vec::new(),
            instruction_count: 0,
            inhibit_interrupts_after_block: false,
        },
    }
}

#[test]
fn code_cache_get_cloned_updates_recency() {
    let mut cache = CodeCache::new(3, 0);
    assert!(cache.insert(handle(0)).is_empty());
    assert!(cache.insert(handle(1)).is_empty());
    assert!(cache.insert(handle(2)).is_empty());

    // Touch the LRU entry to make it MRU; the next insert should evict `1`, not `0`.
    assert!(cache.get_cloned(0).is_some());

    let evicted = cache.insert(handle(3));
    assert_eq!(evicted, vec![1]);
    assert!(cache.contains(0));
    assert!(!cache.contains(1));
    assert!(cache.contains(2));
    assert!(cache.contains(3));
}

