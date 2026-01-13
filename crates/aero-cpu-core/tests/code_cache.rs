use aero_cpu_core::jit::cache::{CodeCache, CompiledBlockHandle, CompiledBlockMeta};

fn handle(entry_rip: u64, byte_len: u32) -> CompiledBlockHandle {
    CompiledBlockHandle {
        entry_rip,
        table_index: entry_rip as u32,
        meta: CompiledBlockMeta {
            code_paddr: entry_rip,
            byte_len,
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
