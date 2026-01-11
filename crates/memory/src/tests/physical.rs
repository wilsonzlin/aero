use crate::phys::{DenseMemory, GuestMemory, GuestMemoryError, SparseMemory};

#[test]
fn dense_read_write_aligned_and_unaligned() {
    let mut mem = DenseMemory::new(64).unwrap();

    mem.write_from(0, &[1, 2, 3, 4]).unwrap();
    let mut buf = [0u8; 4];
    mem.read_into(0, &mut buf).unwrap();
    assert_eq!(buf, [1, 2, 3, 4]);

    mem.write_from(1, &[0xAA, 0xBB, 0xCC]).unwrap();
    let mut buf = [0u8; 4];
    mem.read_into(0, &mut buf).unwrap();
    assert_eq!(buf, [1, 0xAA, 0xBB, 0xCC]);
}

#[test]
fn sparse_cross_chunk_accesses() {
    let mut mem = SparseMemory::with_chunk_size(64, 16).unwrap();

    // Write 4 bytes starting at offset 14 crosses chunk boundary (14..18).
    mem.write_from(14, &[0x10, 0x20, 0x30, 0x40]).unwrap();

    let mut buf = [0u8; 4];
    mem.read_into(14, &mut buf).unwrap();
    assert_eq!(buf, [0x10, 0x20, 0x30, 0x40]);
}

#[test]
fn out_of_range_accesses_return_error_without_panicking() {
    let mut dense = DenseMemory::new(8).unwrap();
    assert!(matches!(
        dense.write_from(100, &[1, 2, 3]),
        Err(GuestMemoryError::OutOfRange { .. })
    ));

    let sparse = SparseMemory::with_chunk_size(8, 4).unwrap();
    let mut buf = [0u8; 4];
    assert!(matches!(
        sparse.read_into(100, &mut buf),
        Err(GuestMemoryError::OutOfRange { .. })
    ));
}
