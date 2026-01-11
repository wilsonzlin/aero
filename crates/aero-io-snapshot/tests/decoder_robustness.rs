use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::SnapshotError;

#[test]
fn decoder_vec_bytes_does_not_preallocate_on_large_count() {
    // `Decoder::vec_bytes` reads a u32 element count, followed by `count` (len + bytes) entries.
    // Historically it used `Vec::with_capacity(count)`, which could attempt to allocate a
    // pathological amount of memory for corrupted/truncated snapshots. This test ensures we return
    // a normal decode error without trying to preallocate.
    let buf = u32::MAX.to_le_bytes();
    let mut d = Decoder::new(&buf);
    let err = d.vec_bytes().unwrap_err();
    assert_eq!(err, SnapshotError::UnexpectedEof);
}
