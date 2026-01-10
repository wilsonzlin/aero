use aero_mem::{PhysicalMemory, PhysicalMemoryOptions};
use std::sync::Arc;

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

#[test]
fn concurrent_writes_to_disjoint_ranges() {
    let mem = Arc::new(
        PhysicalMemory::with_options(0x8000, PhysicalMemoryOptions { chunk_size: 4096 }).unwrap(),
    );

    let mut threads = Vec::new();
    for i in 0u64..8 {
        let mem = mem.clone();
        threads.push(std::thread::spawn(move || {
            let start = i * 0x1000;
            mem.write_bytes(start, &vec![i as u8; 0x1000]);
        }));
    }

    for t in threads {
        t.join().expect("thread panicked");
    }

    // Each thread touched a distinct chunk.
    assert_eq!(mem.allocated_chunks(), 8);

    let mut buf = vec![0u8; 0x1000];
    for i in 0u64..8 {
        let start = i * 0x1000;
        mem.read_bytes(start, &mut buf);
        assert!(buf.iter().all(|b| *b == i as u8));
    }
}
