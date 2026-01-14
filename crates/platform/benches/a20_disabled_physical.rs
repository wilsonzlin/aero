use aero_platform::address_filter::AddressFilter;
use aero_platform::memory::MemoryBus;
use aero_platform::ChipsetState;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use memory::{DenseMemory, PhysicalMemoryBus};

const A20_BIT: u64 = 1 << 20;

fn bench_a20_disabled_large_read(c: &mut Criterion) {
    // Read a 1MiB span starting near the top of the first MiB so that (with A20 disabled) the
    // access crosses the 1MiB boundary and wraps back to address 0.
    let start = 0x000F_0000u64;
    let len = 1024 * 1024usize;

    // Optimized path: `aero_platform::memory::MemoryBus` with A20 disabled uses bulk reads with
    // 1MiB-chunking (splitting at the A20 boundary).
    let chipset = ChipsetState::new(false);
    let filter = AddressFilter::new(chipset.a20());
    let mut bus = MemoryBus::new(filter, 2 * 1024 * 1024);
    bus.ram_mut()
        .write_from(0, &vec![0xAAu8; 2 * 1024 * 1024])
        .unwrap();

    let mut buf = vec![0u8; len];
    c.bench_function("a20_disabled_read_chunked_1mib_cross_boundary", |b| {
        b.iter(|| {
            bus.read_physical(black_box(start), black_box(&mut buf));
            black_box(&buf);
        })
    });

    // Baseline reference: a naive per-byte implementation that applies the A20 mask to each
    // address and performs a 1-byte physical bus read.
    let mut slow_bus = PhysicalMemoryBus::new(Box::new(DenseMemory::new(2 * 1024 * 1024).unwrap()));
    slow_bus
        .ram
        .write_from(0, &vec![0xAAu8; 2 * 1024 * 1024])
        .unwrap();

    c.bench_function("a20_disabled_read_bytewise_1mib_cross_boundary", |b| {
        b.iter(|| {
            for (i, slot) in buf.iter_mut().enumerate() {
                let addr = start.wrapping_add(i as u64);
                let filtered = addr & !A20_BIT;
                *slot = slow_bus.read_physical_u8(filtered);
            }
            black_box(&buf);
        })
    });
}

criterion_group!(benches, bench_a20_disabled_large_read);
criterion_main!(benches);
