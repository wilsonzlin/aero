#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use aero_mmu::{AccessType, MemoryBus, Mmu};
#[cfg(not(target_arch = "wasm32"))]
use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
#[cfg(not(target_arch = "wasm32"))]
use std::convert::TryInto;

// Control register bits (x86).
#[cfg(not(target_arch = "wasm32"))]
const CR0_PG: u64 = 1 << 31;
#[cfg(not(target_arch = "wasm32"))]
const CR4_PAE: u64 = 1 << 5;
#[cfg(not(target_arch = "wasm32"))]
const EFER_LME: u64 = 1 << 8;

// Long-mode page-table entry bits (common subset).
#[cfg(not(target_arch = "wasm32"))]
const PTE_P: u64 = 1 << 0;
#[cfg(not(target_arch = "wasm32"))]
const PTE_RW: u64 = 1 << 1;
#[cfg(not(target_arch = "wasm32"))]
const PTE_US: u64 = 1 << 2;
#[cfg(not(target_arch = "wasm32"))]
const PTE_A: u64 = 1 << 5;

#[cfg(not(target_arch = "wasm32"))]
const MEM_SIZE: usize = 0x10000;

#[cfg(not(target_arch = "wasm32"))]
const PML4_BASE: u64 = 0x1000;
#[cfg(not(target_arch = "wasm32"))]
const PDPT_BASE: u64 = 0x2000;
#[cfg(not(target_arch = "wasm32"))]
const PD_BASE: u64 = 0x3000;
#[cfg(not(target_arch = "wasm32"))]
const PT_BASE: u64 = 0x4000;
#[cfg(not(target_arch = "wasm32"))]
const PAGE_BASE: u64 = 0x8000;

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
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

    fn write_u64_raw(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_long4_4k_mapping(&mut self, vaddr: u64, paddr_base: u64) {
        let pml4_index = (vaddr >> 39) & 0x1ff;
        let pdpt_index = (vaddr >> 30) & 0x1ff;
        let pd_index = (vaddr >> 21) & 0x1ff;
        let pt_index = (vaddr >> 12) & 0x1ff;

        // Pre-set Accessed so the benchmark measures the steady-state translation
        // path (no guest page-table writes during A/D updates).
        let flags = PTE_P | PTE_RW | PTE_US | PTE_A;

        // PML4E -> PDPT
        self.write_u64_raw(PML4_BASE + pml4_index * 8, PDPT_BASE | flags);
        // PDPTE -> PD
        self.write_u64_raw(PDPT_BASE + pdpt_index * 8, PD_BASE | flags);
        // PDE -> PT
        self.write_u64_raw(PD_BASE + pd_index * 8, PT_BASE | flags);
        // PTE -> page
        self.write_u64_raw(PT_BASE + pt_index * 8, paddr_base | flags);
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
fn new_long_mode_mmu() -> Mmu {
    let mut mmu = Mmu::new();
    mmu.set_cr3(PML4_BASE);
    mmu.set_cr4(CR4_PAE);
    mmu.set_efer(EFER_LME);
    mmu.set_cr0(CR0_PG);
    mmu
}

#[cfg(not(target_arch = "wasm32"))]
fn bench_translate(c: &mut Criterion) {
    // Arbitrary canonical linear address (< 2^47).
    let vaddr: u64 = 0x0000_1234_5678_9abc;

    let mut group = c.benchmark_group("mmu_translate_long4");

    group.bench_function("cold_tlb_miss", |b| {
        let mut mem = TestMemory::new(MEM_SIZE);
        mem.write_long4_4k_mapping(vaddr, PAGE_BASE);

        b.iter_batched(
            new_long_mode_mmu,
            |mut mmu| {
                let paddr = mmu
                    .translate(&mut mem, black_box(vaddr), AccessType::Read, 3)
                    .unwrap();
                black_box(paddr);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("hot_tlb_hit", |b| {
        let mut mem = TestMemory::new(MEM_SIZE);
        mem.write_long4_4k_mapping(vaddr, PAGE_BASE);
        let mut mmu = new_long_mode_mmu();

        // Warm the TLB.
        black_box(
            mmu.translate(&mut mem, black_box(vaddr), AccessType::Read, 3)
                .unwrap(),
        );

        b.iter(|| {
            let paddr = mmu
                .translate(&mut mem, black_box(vaddr), AccessType::Read, 3)
                .unwrap();
            black_box(paddr);
        });
    });

    group.finish();
}

#[cfg(not(target_arch = "wasm32"))]
criterion_group!(benches, bench_translate);
#[cfg(not(target_arch = "wasm32"))]
criterion_main!(benches);
