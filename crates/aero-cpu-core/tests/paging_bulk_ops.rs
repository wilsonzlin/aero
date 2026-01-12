use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;

const PAGE_SIZE: usize = 4096;

#[derive(Clone, Debug)]
struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    fn load(&mut self, paddr: u64, bytes: &[u8]) {
        let off = paddr as usize;
        self.data[off..off + bytes.len()].copy_from_slice(bytes);
    }

    fn slice(&self, paddr: u64, len: usize) -> &[u8] {
        let off = paddr as usize;
        &self.data[off..off + len]
    }
}

impl MemoryBus for TestMemory {
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.data[paddr as usize]
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        let off = paddr as usize;
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        let off = paddr as usize;
        u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        let off = paddr as usize;
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.data[paddr as usize] = value;
    }

    fn write_u16(&mut self, paddr: u64, value: u16) {
        let off = paddr as usize;
        self.data[off..off + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(&mut self, paddr: u64, value: u32) {
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, paddr: u64, value: u64) {
        let off = paddr as usize;
        self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
    }
}

fn setup_long4_4k(
    mem: &mut impl MemoryBus,
    pml4_base: u64,
    pdpt_base: u64,
    pd_base: u64,
    pt_base: u64,
    pte0: u64,
    pte1: u64,
) {
    // PML4E[0] -> PDPT
    mem.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    // PDPTE[0] -> PD
    mem.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    // PDE[0] -> PT
    mem.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    // PTE[0] and PTE[1]
    mem.write_u64(pt_base, pte0);
    mem.write_u64(pt_base + 8, pte1);
}

fn long_state(pml4_base: u64) -> CpuState {
    let mut state = CpuState::new(CpuMode::Long);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME;
    state.update_mode();
    state
}

#[test]
fn pagingbus_bulk_copy_success() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let src_data: Vec<u8> = (0u8..128).collect();
    phys.load(page0, &src_data);
    phys.load(page1, &[0xCC; 128]);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.supports_bulk_copy());
    assert!(bus.bulk_copy(0x1000, 0, src_data.len())?);

    assert_eq!(bus.inner_mut().slice(page1, src_data.len()), src_data.as_slice());
    Ok(())
}

#[test]
fn pagingbus_bulk_copy_overlap_memmove_backward_multi_page() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let mut initial = vec![0u8; PAGE_SIZE * 2];
    for (i, b) in initial.iter_mut().enumerate() {
        *b = i as u8;
    }
    phys.load(page0, &initial[..PAGE_SIZE]);
    phys.load(page1, &initial[PAGE_SIZE..]);

    // Choose an overlapping memmove where dst > src and len > SCRATCH_SIZE so the implementation
    // must copy backwards across multiple chunks.
    let src = 0x100usize;
    let dst = 0x180usize;
    let len = PAGE_SIZE + 128;

    let mut expected = initial.clone();
    expected.copy_within(src..src + len, dst);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.bulk_copy(dst as u64, src as u64, len)?);

    let phys = bus.inner_mut();
    let mut actual = Vec::with_capacity(PAGE_SIZE * 2);
    actual.extend_from_slice(phys.slice(page0, PAGE_SIZE));
    actual.extend_from_slice(phys.slice(page1, PAGE_SIZE));

    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn pagingbus_bulk_copy_overlap_memmove_forward_multi_page() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let mut initial = vec![0u8; PAGE_SIZE * 2];
    for (i, b) in initial.iter_mut().enumerate() {
        *b = i as u8;
    }
    phys.load(page0, &initial[..PAGE_SIZE]);
    phys.load(page1, &initial[PAGE_SIZE..]);

    // Choose an overlapping memmove where dst < src; forward copy is safe, but the operation
    // must still behave like memmove.
    let src = 0x180usize;
    let dst = 0x100usize;
    let len = PAGE_SIZE + 128;

    let mut expected = initial.clone();
    expected.copy_within(src..src + len, dst);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.bulk_copy(dst as u64, src as u64, len)?);

    let phys = bus.inner_mut();
    let mut actual = Vec::with_capacity(PAGE_SIZE * 2);
    actual.extend_from_slice(phys.slice(page0, PAGE_SIZE));
    actual.extend_from_slice(phys.slice(page1, PAGE_SIZE));

    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn pagingbus_bulk_set_success_two_pages() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.supports_bulk_set());
    assert!(bus.bulk_set(0, &[0xAB], PAGE_SIZE * 2)?);

    assert_eq!(bus.inner_mut().slice(page0, PAGE_SIZE), vec![0xAB; PAGE_SIZE]);
    assert_eq!(bus.inner_mut().slice(page1, PAGE_SIZE), vec![0xAB; PAGE_SIZE]);
    Ok(())
}

#[test]
fn pagingbus_bulk_copy_preflight_failure_is_atomic() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;

    // Map only the first 4KiB page; leave the second unmapped.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Capture page-table state so we can assert the preflight made no guest-visible paging
    // changes (accessed/dirty bit updates, etc).
    let pt_state_before = (
        phys.read_u64(pml4_base),
        phys.read_u64(pdpt_base),
        phys.read_u64(pd_base),
        phys.read_u64(pt_base),
        phys.read_u64(pt_base + 8),
    );

    // Destination range in the mapped page.
    let len = 0x200usize;
    phys.load(page0, &vec![0xCC; len]);

    // Source starts near the end of the mapped page and spans into the unmapped second page.
    let src = 0xF00u64;
    for (i, b) in (0u8..=0xFF).enumerate() {
        phys.write_u8(page0 + src + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(!bus.bulk_copy(0, src, len)?);
    assert_eq!(bus.mmu().cr2(), 0);

    // Must not have written any destination bytes.
    assert_eq!(bus.inner_mut().slice(page0, len), vec![0xCC; len]);

    let pt_state_after = (
        bus.inner_mut().read_u64(pml4_base),
        bus.inner_mut().read_u64(pdpt_base),
        bus.inner_mut().read_u64(pd_base),
        bus.inner_mut().read_u64(pt_base),
        bus.inner_mut().read_u64(pt_base + 8),
    );
    assert_eq!(pt_state_after, pt_state_before);
    Ok(())
}

#[test]
fn pagingbus_bulk_set_preflight_failure_is_atomic() -> Result<(), Exception> {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;

    // Map only the first 4KiB page; leave the second unmapped.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    let pt_state_before = (
        phys.read_u64(pml4_base),
        phys.read_u64(pdpt_base),
        phys.read_u64(pd_base),
        phys.read_u64(pt_base),
        phys.read_u64(pt_base + 8),
    );

    // Destination starts near the end of the mapped page and spans into the unmapped second page.
    let dst = 0xF00u64;
    let repeat = 0x200usize;
    let mapped_len = PAGE_SIZE - dst as usize;
    phys.load(page0 + dst, &vec![0xCD; mapped_len]);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(!bus.bulk_set(dst, &[0xAB], repeat)?);
    assert_eq!(bus.mmu().cr2(), 0);

    // Must not have written any bytes in the mapped portion.
    assert_eq!(
        bus.inner_mut().slice(page0 + dst, mapped_len),
        vec![0xCD; mapped_len]
    );

    let pt_state_after = (
        bus.inner_mut().read_u64(pml4_base),
        bus.inner_mut().read_u64(pdpt_base),
        bus.inner_mut().read_u64(pd_base),
        bus.inner_mut().read_u64(pt_base),
        bus.inner_mut().read_u64(pt_base + 8),
    );
    assert_eq!(pt_state_after, pt_state_before);
    Ok(())
}

#[test]
fn pagingbus_bulk_copy_overlap_memmove_backward_tiny_overlap_32b() -> Result<(), Exception> {
    // Exercise PagingBus's bulk_copy memmove semantics for overlapping ranges where dst > src.
    // This must copy backward so it behaves like memmove rather than memcpy.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let mut init = [0u8; 32];
    for (i, b) in init.iter_mut().enumerate() {
        *b = i as u8;
    }
    phys.load(page0, &init);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.bulk_copy(4, 0, 16)?);

    let mut expected = init;
    expected[4..20].copy_from_slice(&init[0..16]);

    assert_eq!(bus.inner_mut().slice(page0, expected.len()), expected.as_slice());
    Ok(())
}

#[test]
fn pagingbus_bulk_copy_overlap_memmove_forward_tiny_overlap_32b() -> Result<(), Exception> {
    // Exercise PagingBus's bulk_copy memmove semantics for overlapping ranges where dst < src.
    // This must copy forward.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let page0 = 0x5000u64;
    let page1 = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    let mut init = [0u8; 32];
    for (i, b) in init.iter_mut().enumerate() {
        *b = i as u8;
    }
    phys.load(page0, &init);

    let mut bus = PagingBus::new(phys);
    bus.sync(&long_state(pml4_base));

    assert!(bus.bulk_copy(0, 4, 16)?);

    let mut expected = init;
    expected[0..16].copy_from_slice(&init[4..20]);

    assert_eq!(bus.inner_mut().slice(page0, expected.len()), expected.as_slice());
    Ok(())
}
