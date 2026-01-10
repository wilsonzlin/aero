use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use memory::MemoryBus;

const PAGE_SIZE: usize = 4096;

struct BenchMem {
    buf: Vec<u8>,
}

impl BenchMem {
    fn new(size: usize) -> Self {
        Self { buf: vec![0u8; size] }
    }
}

impl MemoryBus for BenchMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start + buf.len();
        buf.copy_from_slice(&self.buf[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start + buf.len();
        self.buf[start..end].copy_from_slice(buf);
    }
}

fn bench_scatter_copy(c: &mut Criterion) {
    let mut group = c.benchmark_group("scatter_copy");
    for size_kib in [4usize, 64, 256, 1024] {
        let len = size_kib * 1024;
        let src = vec![0x55u8; len];

        // NVMe-ish: 4KiB pages via PRPs.
        group.bench_with_input(BenchmarkId::new("nvme_prp", size_kib), &len, |b, _| {
            let mut mem = BenchMem::new(8 * 1024 * 1024);
            let base = 0x10000u64;
            b.iter(|| {
                let mut offset = 0usize;
                let mut paddr = base;
                while offset < src.len() {
                    let chunk = (src.len() - offset).min(PAGE_SIZE);
                    mem.write_physical(paddr, &src[offset..offset + chunk]);
                    offset += chunk;
                    paddr += PAGE_SIZE as u64;
                }
            })
        });

        // AHCI-ish: pretend we have 1KiB PRD segments.
        group.bench_with_input(BenchmarkId::new("ahci_prdt", size_kib), &len, |b, _| {
            let mut mem = BenchMem::new(8 * 1024 * 1024);
            let base = 0x10000u64;
            b.iter(|| {
                let mut offset = 0usize;
                let mut paddr = base;
                while offset < src.len() {
                    let chunk = (src.len() - offset).min(1024);
                    mem.write_physical(paddr, &src[offset..offset + chunk]);
                    offset += chunk;
                    paddr += 1024;
                }
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_scatter_copy);
criterion_main!(benches);
