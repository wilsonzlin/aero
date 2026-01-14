#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_mmu::{AccessType, MemoryBus, Mmu};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
#[cfg(not(target_arch = "wasm32"))]
use std::convert::TryInto;

#[cfg(not(target_arch = "wasm32"))]
fn criterion_config() -> Criterion {
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("ci") => Criterion::default()
            // Keep PR runtime low.
            .warm_up_time(Duration::from_millis(200))
            .measurement_time(Duration::from_secs(1))
            .sample_size(10)
            .noise_threshold(0.05),
        _ => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(30)
            .noise_threshold(0.03),
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct BenchMemory {
    data: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BenchMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn write_u64_raw(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl MemoryBus for BenchMemory {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.data[paddr as usize]
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        let off = paddr as usize;
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        let off = paddr as usize;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        let off = paddr as usize;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.data[paddr as usize] = value;
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        let off = paddr as usize;
        self.data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_tlb_lookup_hit_long4_4k(c: &mut Criterion) {
    // x86-64 paging-structure bits (subset; repeated here so benches don't rely on crate privates).
    const PTE_P64: u64 = 1 << 0;
    const PTE_RW64: u64 = 1 << 1;
    const PTE_US64: u64 = 1 << 2;

    const CR0_PG: u64 = 1 << 31;
    const CR4_PSE: u64 = 1 << 4;
    const CR4_PAE: u64 = 1 << 5;
    const EFER_LME: u64 = 1 << 8;

    let mut mmu = Mmu::new();
    let mut mem = BenchMemory::new(0x20_000);

    // Simple long-mode 4KB mapping:
    //   PML4[0] -> PDPT[0] -> PD[0] -> PT[0] -> page.
    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page_base = 0x8000u64;

    mem.write_u64_raw(pml4_base, pdpt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pdpt_base, pd_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pd_base, pt_base | PTE_P64 | PTE_RW64 | PTE_US64);
    mem.write_u64_raw(pt_base, page_base | PTE_P64 | PTE_RW64 | PTE_US64);

    mmu.set_cr3(pml4_base);
    // Many OSes set CR4.PSE in long mode even if they don't actively map large
    // pages. This keeps the benchmark representative while still using a 4KiB
    // mapping.
    mmu.set_cr4(CR4_PAE | CR4_PSE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);

    let vaddr = 0x234u64;

    // Populate the TLB once via a page walk.
    let warm_read = mmu.translate(&mut mem, vaddr, AccessType::Read, 0).unwrap();
    black_box(warm_read);
    // Warm the ITLB too so execute hits don't page-walk inside the benchmark.
    let warm_exec = mmu
        .translate(&mut mem, vaddr, AccessType::Execute, 0)
        .unwrap();
    black_box(warm_exec);

    let mut group = c.benchmark_group("tlb_lookup");
    group.throughput(Throughput::Elements(1));
    group.bench_function("hit_long4_4k_read", |b| {
        b.iter(|| {
            let paddr = mmu
                .translate(&mut mem, black_box(vaddr), AccessType::Read, 0)
                .unwrap();
            black_box(paddr)
        })
    });
    group.bench_function("hit_long4_4k_exec", |b| {
        b.iter(|| {
            let paddr = mmu
                .translate(&mut mem, black_box(vaddr), AccessType::Execute, 0)
                .unwrap();
            black_box(paddr)
        })
    });
    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_tlb_lookup_hit_long4_4k
}
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
