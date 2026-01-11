use aero_http_range::ResolvedByteRange;

#[test]
fn resolved_range_len_saturates_on_overflow() {
    let r = ResolvedByteRange {
        start: 0,
        end: u64::MAX,
    };
    assert_eq!(r.len(), u64::MAX);
    assert!(!r.is_empty());
}

#[test]
fn resolved_range_len_is_empty_when_end_before_start() {
    let r = ResolvedByteRange { start: 10, end: 5 };
    assert_eq!(r.len(), 0);
    assert!(r.is_empty());
}
