// Criterion benchmarks for `PagingBus` hot paths.
//
// These benches are intended to track the performance impact of optimizations in:
// - bulk_copy / bulk_set fast paths (e.g. used by REP string ops)
// - small instruction fetches (PagingBus::fetch / read_bytes)
//
// We intentionally use a simple in-crate Vec-backed `TestMemory` (no locks) so the
// results focus on paging + bulk operation overhead rather than embedding concerns.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::mem::CpuBus as _;
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
#[cfg(not(target_arch = "wasm32"))]
use aero_cpu_core::PagingBus;
#[cfg(not(target_arch = "wasm32"))]
use aero_mmu::MemoryBus;
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

#[cfg(not(target_arch = "wasm32"))]
const PTE_P: u64 = 1 << 0;
#[cfg(not(target_arch = "wasm32"))]
const PTE_RW: u64 = 1 << 1;
#[cfg(not(target_arch = "wasm32"))]
const PTE_US: u64 = 1 << 2;
#[cfg(not(target_arch = "wasm32"))]
const PTE_PS: u64 = 1 << 7; // Page Size (PDE maps 2MiB when set in long mode).

#[cfg(not(target_arch = "wasm32"))]
const CR4_PSE: u64 = 1 << 4;

#[cfg(not(target_arch = "wasm32"))]
const HUGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
const BULK_LEN: usize = 16 * 1024 * 1024; // 16 MiB.

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
struct TestMemory {
    data: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn load(&mut self, paddr: u64, bytes: &[u8]) {
        let off = paddr as usize;
        self.data[off..off + bytes.len()].copy_from_slice(bytes);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl MemoryBus for TestMemory {
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
fn criterion_config() -> Criterion {
    // Default to a CI-friendly profile so `cargo bench` completes under `scripts/safe-run.sh`'s
    // default timeout. Opt into longer runs explicitly with `AERO_BENCH_PROFILE=full`.
    match std::env::var("AERO_BENCH_PROFILE").as_deref() {
        Ok("full") => Criterion::default()
            .warm_up_time(Duration::from_secs(1))
            .measurement_time(Duration::from_secs(2))
            .sample_size(30)
            .noise_threshold(0.03),
        _ => Criterion::default()
            // Keep PR/CI runtime low.
            .warm_up_time(Duration::from_millis(200))
            .measurement_time(Duration::from_secs(1))
            .sample_size(10)
            .noise_threshold(0.05),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn long_state(pml4_base: u64) -> CpuState {
    let mut state = CpuState::new(CpuMode::Long);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    // `aero-mmu` models large-page enable (2MiB / 1GiB) via CR4.PSE even in long mode.
    state.control.cr4 = CR4_PAE | CR4_PSE;
    state.msr.efer = EFER_LME;
    state.update_mode();
    state
}

#[cfg(not(target_arch = "wasm32"))]
fn setup_long4_2m(
    mem: &mut impl MemoryBus,
    pml4_base: u64,
    pdpt_base: u64,
    pd_base: u64,
    phys_base: u64,
    map_size: usize,
) {
    assert_eq!(
        phys_base % (HUGE_PAGE_SIZE as u64),
        0,
        "phys_base must be 2MiB-aligned for 2MiB PDE mappings"
    );
    assert_eq!(
        map_size % HUGE_PAGE_SIZE,
        0,
        "map_size must be a multiple of 2MiB"
    );

    // PML4E[0] -> PDPT
    mem.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    // PDPTE[0] -> PD
    mem.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);

    let entries = map_size / HUGE_PAGE_SIZE;
    for i in 0..entries {
        let paddr = phys_base + (i as u64) * (HUGE_PAGE_SIZE as u64);
        let pde = paddr | PTE_P | PTE_RW | PTE_US | PTE_PS;
        mem.write_u64(pd_base + (i as u64) * 8, pde);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn make_paging_bus() -> PagingBus<TestMemory> {
    // Layout:
    // - Page tables at low physical memory (0x1000..)
    // - Mapped guest RAM region starts at a 2MiB boundary so we can use 2MiB PDEs.
    const PML4_BASE: u64 = 0x1000;
    const PDPT_BASE: u64 = 0x2000;
    const PD_BASE: u64 = 0x3000;
    const PHYS_BASE: u64 = 0x20_0000; // 2 MiB

    // Map enough linear memory for:
    // - source buffer [0, 16 MiB)
    // - destination buffer [16 MiB, 32 MiB)
    const MAP_SIZE: usize = 2 * BULK_LEN; // 32 MiB

    let phys_size = (PHYS_BASE as usize) + MAP_SIZE;
    let mut phys = TestMemory::new(phys_size);

    setup_long4_2m(
        &mut phys, PML4_BASE, PDPT_BASE, PD_BASE, PHYS_BASE, MAP_SIZE,
    );

    // Initialize the source buffer with deterministic data (outside the measured region).
    // This helps keep benchmarks stable and avoids "all-zero" patterns.
    let mut src_init = vec![0u8; BULK_LEN];
    for (i, b) in src_init.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    phys.load(PHYS_BASE, &src_init);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(PML4_BASE));
    bus
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_paging_bus_bulk(c: &mut Criterion) {
    let mut bus = make_paging_bus();

    let src = 0u64;
    let dst = BULK_LEN as u64; // [16 MiB, 32 MiB)

    let mut group = c.benchmark_group("paging_bus");

    group.throughput(Throughput::Bytes(BULK_LEN as u64));
    group.bench_function("bulk_copy_16mib", |b| {
        b.iter(|| {
            let bus = black_box(&mut bus);
            let ok = bus
                .bulk_copy(black_box(dst), black_box(src), black_box(BULK_LEN))
                .unwrap();
            black_box(ok);
        })
    });

    group.throughput(Throughput::Bytes(BULK_LEN as u64));
    group.bench_function("bulk_set_16mib", |b| {
        b.iter(|| {
            let bus = black_box(&mut bus);
            let ok = bus
                .bulk_set(black_box(dst), black_box(&[0xA5][..]), black_box(BULK_LEN))
                .unwrap();
            black_box(ok);
        })
    });

    // Approximate instruction fetch overhead by fetching 15 bytes in a tight loop.
    // Use a fixed address within the mapped region so the iTLB stays hot.
    const FETCH_ITERS: usize = 32 * 1024;
    group.throughput(Throughput::Bytes((15 * FETCH_ITERS) as u64));
    group.bench_function("fetch_15b_loop", |b| {
        b.iter(|| {
            let bus = black_box(&mut bus);
            let mut checksum = 0u64;
            let rip = black_box(0x1000u64);
            for _ in 0..FETCH_ITERS {
                let buf = bus.fetch(rip, 15).unwrap();
                checksum = checksum.wrapping_add(buf[0] as u64);
            }
            black_box(checksum);
        })
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group! {
    name = benches;
    config = criterion_config();
    targets = bench_paging_bus_bulk
}

#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
