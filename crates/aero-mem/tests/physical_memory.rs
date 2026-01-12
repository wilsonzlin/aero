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

    // Avoid spawning excessive OS threads in constrained CI environments. We still exercise
    // concurrent writes, but cap the number of writer threads while covering all chunks.
    const THREADS: usize = 2;
    const CHUNKS: usize = 8;

    let mut threads = Vec::new();
    for tid in 0..THREADS {
        let mem = mem.clone();
        threads.push(std::thread::spawn(move || {
            let mut i = tid;
            while i < CHUNKS {
                let start = (i as u64) * 0x1000;
                mem.write_bytes(start, &vec![i as u8; 0x1000]);
                i += THREADS;
            }
        }));
    }

    for t in threads {
        t.join().expect("thread panicked");
    }

    // Each thread touched a distinct chunk.
    assert_eq!(mem.allocated_chunks(), CHUNKS);

    let mut buf = vec![0u8; 0x1000];
    for i in 0..CHUNKS {
        let start = (i as u64) * 0x1000;
        mem.read_bytes(start, &mut buf);
        assert!(buf.iter().all(|b| *b == i as u8));
    }
}

#[test]
fn typed_ops_across_chunk_boundary() {
    let mem =
        PhysicalMemory::with_options(0x3000, PhysicalMemoryOptions { chunk_size: 4096 }).unwrap();

    // Address 0x0FFF is the last byte of chunk 0.
    mem.write_u8(0x0FFF, 0x11);
    mem.write_u8(0x1000, 0x22);
    assert_eq!(mem.read_u16(0x0FFF), 0x2211);

    mem.write_u8(0x0FFE, 0xAA);
    mem.write_u8(0x0FFF, 0xBB);
    mem.write_u8(0x1000, 0xCC);
    mem.write_u8(0x1001, 0xDD);
    assert_eq!(mem.read_u32(0x0FFE), 0xDDCC_BBAA);

    mem.write_u64(0x0FFC, 0x1122_3344_5566_7788);
    assert_eq!(mem.read_u64(0x0FFC), 0x1122_3344_5566_7788);
}
