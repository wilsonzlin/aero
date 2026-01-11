use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::segmentation::{LoadReason, Seg};
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;

#[derive(Clone, Debug)]
struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
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

#[allow(clippy::too_many_arguments)]
fn make_descriptor(
    base: u32,
    limit_raw: u32,
    typ: u8,
    s: bool,
    dpl: u8,
    present: bool,
    avl: bool,
    l: bool,
    db: bool,
    g: bool,
) -> u64 {
    let mut raw = 0u64;
    raw |= (limit_raw & 0xFFFF) as u64;
    raw |= ((base & 0xFFFF) as u64) << 16;
    raw |= (((base >> 16) & 0xFF) as u64) << 32;
    let access =
        (typ as u64) | ((s as u64) << 4) | (((dpl as u64) & 0x3) << 5) | ((present as u64) << 7);
    raw |= access << 40;
    raw |= (((limit_raw >> 16) & 0xF) as u64) << 48;
    let flags = (avl as u64) | ((l as u64) << 1) | ((db as u64) << 2) | ((g as u64) << 3);
    raw |= flags << 52;
    raw |= (((base >> 24) & 0xFF) as u64) << 56;
    raw
}

#[test]
fn gdt_descriptor_reads_ignore_user_supervisor_paging_bit() {
    // This test targets the subtle paging rule that the CPU must be able to read
    // system structures (like the GDT) even when running at CPL3 and the backing
    // page is marked supervisor-only (U/S=0).
    //
    // Our paging adapter (`PagingBus`) caches CPL and enforces U/S based on it.
    // Without special-casing, `load_seg` would #PF when attempting to read a
    // supervisor-only GDT page while executing user code.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x10000u64;
    let pdpt_base = 0x11000u64;
    let pd_base = 0x12000u64;
    let pt_base = 0x13000u64;

    // Set up a simple identity-mapped long-mode page table. All upper levels are
    // user-accessible; the leaf PTE controls U/S per-page.
    phys.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    // Map page 0 as user-accessible (not strictly needed but mirrors typical setups).
    phys.write_u64(pt_base, PTE_P | PTE_RW | PTE_US);
    // Map the GDT page (0x1000) as supervisor-only (U/S=0).
    phys.write_u64(pt_base + 8, 0x1000 | PTE_P | PTE_RW);

    let mut bus = PagingBus::new(phys);

    // Place a tiny GDT at linear/physical 0x1000.
    let gdt_base = 0x1000u64;
    let null = 0u64;
    let user_data = make_descriptor(0, 0xFFFFF, 0x2, true, 3, true, false, false, true, true);
    bus.inner_mut().write_u64(gdt_base, null);
    bus.inner_mut().write_u64(gdt_base + 8, user_data);

    // Selector for the DPL3 data segment.
    let selector = (1u16 << 3) | 0b11;

    let mut state = CpuState::new(CpuMode::Long);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME;
    state.update_mode();

    // Run as CPL3.
    state.segments.cs.selector = 0x33;
    state.tables.gdtr.base = gdt_base;
    state.tables.gdtr.limit = (2 * 8 - 1) as u16;

    bus.sync(&state);

    // Loading DS from CPL3 should be allowed; the descriptor read is a system access
    // and must not be blocked by the supervisor-only U/S page bit.
    state
        .load_seg(&mut bus, Seg::DS, selector, LoadReason::Data)
        .expect("load_seg should not page fault on supervisor-only GDT pages");

    assert_eq!(state.segments.ds.selector, selector);
    assert!(
        !state.segments.ds.is_unusable(),
        "DS cache should be marked usable after successful load"
    );

    // Sanity: user-mode reads of the GDT page should still fault via the normal
    // data-access path.
    assert_eq!(
        bus.read_u8(gdt_base),
        Err(Exception::PageFault {
            addr: gdt_base,
            error_code: 0b00101, // P=1, W/R=0, U/S=1
        })
    );
}
