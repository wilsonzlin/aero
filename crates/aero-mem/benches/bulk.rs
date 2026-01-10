use aero_mem::{MemoryBus, PhysicalMemory, PhysicalMemoryOptions};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;

const MIB: u64 = 1024 * 1024;

fn bench_bulk(c: &mut Criterion) {
    let ram_size = 64 * MIB;
    let opts = PhysicalMemoryOptions {
        chunk_size: 2 * 1024 * 1024,
    };
    let ram = Arc::new(PhysicalMemory::with_options(ram_size, opts).unwrap());

    // Pre-allocate all chunks so the benchmark measures copy throughput rather
    // than one-time lazy allocation cost.
    let chunk_size = ram.chunk_size() as u64;
    for addr in (0..ram_size).step_by(chunk_size as usize) {
        ram.write_u8(addr, 0);
    }

    let bus = MemoryBus::new(ram.clone());

    let block = vec![0xA5u8; 4096];
    let mut scratch = vec![0u8; block.len()];

    let region = 16 * MIB; // benchmark window per iteration

    let mut group = c.benchmark_group("bulk_4k");
    group.throughput(Throughput::Bytes(region));

    group.bench_function(BenchmarkId::new("physical_write", region), |b| {
        b.iter(|| {
            for addr in (0..region).step_by(block.len()) {
                ram.write_bytes(addr, &block);
            }
        })
    });

    group.bench_function(BenchmarkId::new("physical_read", region), |b| {
        b.iter(|| {
            for addr in (0..region).step_by(block.len()) {
                ram.read_bytes(addr, &mut scratch);
                black_box(&scratch);
            }
        })
    });

    group.bench_function(BenchmarkId::new("bus_write", region), |b| {
        b.iter(|| {
            for addr in (0..region).step_by(block.len()) {
                bus.write_bytes(addr, &block);
            }
        })
    });

    group.bench_function(BenchmarkId::new("bus_read", region), |b| {
        b.iter(|| {
            for addr in (0..region).step_by(block.len()) {
                bus.read_bytes(addr, &mut scratch);
                black_box(&scratch);
            }
        })
    });

    group.finish();
}

criterion_group!(benches, bench_bulk);
criterion_main!(benches);
