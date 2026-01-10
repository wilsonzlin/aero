use std::cell::Cell;

use aero_gpu::bindings::bind_group_cache::{
    BindGroupCache, BindGroupEntryKey, BindGroupKey, BindGroupResourceKey, BufferId,
};

#[test]
fn bind_group_cache_reports_hits_for_repeated_lookups() {
    let mut cache = BindGroupCache::<u64>::new(32);
    let created = Cell::new(0u64);

    let key = BindGroupKey::new(
        0xfeed_beef,
        &[BindGroupEntryKey {
            binding: 0,
            resource: BindGroupResourceKey::Buffer {
                id: BufferId(7),
                offset: 0,
                size: Some(64),
            },
        }],
    );

    for _ in 0..10 {
        let v = cache.get_or_create_with(key.clone(), || {
            let next = created.get() + 1;
            created.set(next);
            next
        });
        assert_eq!(v, 1);
    }

    let stats = cache.stats();
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.hits, 9);
    assert_eq!(stats.entries, 1);
}
