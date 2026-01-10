use aero_mem::{PhysicalMemory, PhysicalMemoryOptions};

#[test]
fn sparse_allocation_only_on_write() {
    let mem = PhysicalMemory::with_options(0x10_0000, PhysicalMemoryOptions { chunk_size: 4096 })
        .unwrap();

    assert_eq!(mem.allocated_chunks(), 0);

    let mut buf = [0u8; 16];
    mem.read_bytes(0x2000, &mut buf);
    assert_eq!(buf, [0u8; 16]);
    assert_eq!(mem.allocated_chunks(), 0, "reads must not allocate");

    mem.write_u8(0x2000, 0xAA);
    assert_eq!(mem.allocated_chunks(), 1);

    mem.write_u8(0x2001, 0xBB);
    assert_eq!(
        mem.allocated_chunks(),
        1,
        "same chunk should not reallocate"
    );

    mem.write_u8(0x3000, 0xCC);
    assert_eq!(mem.allocated_chunks(), 2, "different chunk should allocate");
}
