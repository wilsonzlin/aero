use aero_cpu_core::jit::runtime::{PageVersionTracker, PAGE_SIZE};

#[test]
fn page_version_table_pointer_is_stable_and_mutable() {
    let mut tracker = PageVersionTracker::new(64);
    let (ptr0, len0) = tracker.table_ptr_len();
    assert_eq!(len0, 64);

    // Simulate JIT-side inlined stores by writing directly through the raw table pointer.
    // Safety: the table is a contiguous `u32` array of length `len0`.
    unsafe {
        ptr0.add(3).write(123);
    }
    assert_eq!(tracker.version(3), 123);

    // Mutate via the Rust API and ensure the table pointer does not change.
    for i in 0..10_000u32 {
        let page = (i % 32) as u64;
        tracker.set_version(page, i);
        tracker.bump_write(u64::from(i % 32) * PAGE_SIZE, 1);
    }

    let (ptr1, len1) = tracker.table_ptr_len();
    assert_eq!(len1, len0);
    assert_eq!(ptr1, ptr0);
}

#[test]
fn oom_hardening_large_addresses_and_lengths_do_not_panic() {
    let tracker = PageVersionTracker::new(8);

    // Huge/out-of-range addresses must be ignored, not cause reallocations/panics.
    tracker.bump_write(u64::MAX - PAGE_SIZE, PAGE_SIZE as usize);
    let snapshot = tracker.snapshot(u64::MAX - PAGE_SIZE, u32::MAX);
    assert_eq!(
        snapshot.len(),
        2,
        "expected to span at most two pages near u64::MAX"
    );

    // Snapshot generation must be bounded even for absurd byte lengths.
    let bounded = tracker.snapshot(0, u32::MAX);
    assert_eq!(bounded.len(), PageVersionTracker::MAX_SNAPSHOT_PAGES);
    assert_eq!(bounded[0].page, 0);
    assert_eq!(
        bounded[PageVersionTracker::MAX_SNAPSHOT_PAGES - 1].page,
        (PageVersionTracker::MAX_SNAPSHOT_PAGES - 1) as u64
    );

    // Pathological large lengths must not panic and must only bump the tracked range.
    tracker.bump_write(0, usize::MAX);
    for page in 0..8u64 {
        assert_eq!(tracker.version(page), 1);
    }
}

#[test]
fn bump_write_and_snapshot_are_correct_for_in_range_pages() {
    let tracker = PageVersionTracker::new(4);

    assert_eq!(tracker.version(0), 0);
    tracker.bump_write(0x100, 1);
    assert_eq!(tracker.version(0), 1);
    assert_eq!(tracker.version(1), 0);

    // Cross a page boundary: touches pages 0 and 1.
    tracker.bump_write(PAGE_SIZE - 1, 2);
    assert_eq!(tracker.version(0), 2);
    assert_eq!(tracker.version(1), 1);

    // Explicit set should override, then bump should increment.
    tracker.set_version(2, 99);
    tracker.bump_write(2 * PAGE_SIZE, 1);
    assert_eq!(tracker.version(2), 100);

    // Out-of-range pages are always version 0 and do not panic.
    tracker.set_version(100, 42);
    assert_eq!(tracker.version(100), 0);

    let snap = tracker.snapshot(PAGE_SIZE - 1, 2);
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].page, 0);
    assert_eq!(snap[0].version, 2);
    assert_eq!(snap[1].page, 1);
    assert_eq!(snap[1].version, 1);
}
