use aero_cpu_core::interp::tier0::exec::step;
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;

const EFER_NXE: u64 = 1 << 11;
const CR0_WP: u64 = 1 << 16;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;
const PTE_NX: u64 = 1 << 63;

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

    fn load(&mut self, paddr: u64, bytes: &[u8]) {
        let off = paddr as usize;
        self.data[off..off + bytes.len()].copy_from_slice(bytes);
    }

    fn write_u8_raw(&mut self, paddr: u64, value: u8) {
        self.data[paddr as usize] = value;
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
    mem.write_u64(pt_base + 0 * 8, pte0);
    mem.write_u64(pt_base + 1 * 8, pte1);
}

fn long_state(pml4_base: u64, efer_extra: u64, cpl: u8) -> CpuState {
    let mut state = CpuState::new(CpuMode::Long);
    state.segments.cs.selector = (state.segments.cs.selector & !0b11) | (cpl as u16 & 0b11);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME | efer_extra;
    state.update_mode();
    state
}

#[test]
fn paging_disabled_is_identity_with_32bit_truncation() {
    let mut phys = TestMemory::new(0x2000);
    phys.write_u8_raw(0, 0xAA);
    phys.write_u8_raw(0x1234, 0xBB);

    let mut bus = PagingBus::new(phys);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE;
    state.update_mode();
    bus.sync(&state);

    assert_eq!(bus.read_u8(0).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x1234).unwrap(), 0xBB);

    // When paging is disabled, linear addresses are 32-bit.
    assert_eq!(bus.read_u8(0x1_0000_0000).unwrap(), 0xAA);
    assert_eq!(bus.read_u8(0x1_0000_0000 + 0x1234).unwrap(), 0xBB);
}

#[test]
fn long_mode_4k_translation_read_write_exec() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;
    let code_page = 0x6000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_RW | PTE_US,
        code_page | PTE_P | PTE_RW | PTE_US,
    );

    phys.load(data_page, &[0x11, 0x22, 0x33, 0x44]);
    phys.load(code_page, &[0x90, 0x90, 0xC3]);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 0);
    bus.sync(&state);

    assert_eq!(bus.read_u32(0).unwrap(), 0x4433_2211);

    bus.write_u32(0, 0xAABB_CCDD).unwrap();
    assert_eq!(bus.inner_mut().read_u32(data_page), 0xAABB_CCDD);

    let fetched = bus.fetch(0x1000, 3).unwrap();
    assert_eq!(&fetched[..3], &[0x90, 0x90, 0xC3]);
}

#[test]
fn page_fault_error_codes_not_present_vs_protection() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    // PTE not present.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        0,
        0,
    );

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 3);
    bus.sync(&state);

    assert_eq!(
        bus.read_u8(0),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 2, // U=1, P=0, W=0
        })
    );

    // Now make the page present but read-only; user write should fault with P=1,W=1,U=1.
    setup_long4_4k(
        bus.inner_mut(),
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_US,
        0,
    );

    assert_eq!(
        bus.write_u8(0, 1),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1) | (1 << 2),
        })
    );
}

#[test]
fn nx_fault_when_nxe_enabled() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let code_page = 0x5000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        code_page | PTE_P | PTE_RW | PTE_US | PTE_NX,
        0,
    );

    phys.load(code_page, &[0x90]);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, EFER_NXE, 0);
    bus.sync(&state);

    assert_eq!(
        bus.fetch(0, 1),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 4), // P=1, I/D=1
        })
    );
}

#[test]
fn non_canonical_address_in_long_mode_is_gp0() {
    let phys = TestMemory::new(0x2000);
    let mut bus = PagingBus::new(phys);

    let state = long_state(0x1000, 0, 0);
    bus.sync(&state);

    let non_canonical = 0x0000_8000_0000_0000u64;
    assert_eq!(bus.read_u8(non_canonical), Err(Exception::gp0()));
}

#[test]
fn fetch_crossing_page_boundary_faults_on_first_missing_byte_and_sets_cr2() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let code_page0 = 0x5000u64;

    // Only the first page is present; the next page is not present.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        code_page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Populate the last 14 bytes of the page so the fetch can read them before faulting.
    let start = 0xff2u64;
    for i in 0..14u64 {
        phys.write_u8_raw(code_page0 + start + i, (i as u8).wrapping_add(1));
    }

    let mut bus = PagingBus::new(phys);

    let mut state = long_state(pml4_base, 0, 0);
    state.set_rip(start);

    let err = step(&mut state, &mut bus).unwrap_err();
    assert_eq!(
        err,
        Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 4, // I/D=1, P=0, W=0, U=0
        }
    );
    assert_eq!(state.control.cr2, 0x1000);
}

#[test]
fn supervisor_write_respects_wp() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    // Supervisor-only, read-only page.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P,
        0,
    );

    let mut bus = PagingBus::new(phys);
    let mut state = long_state(pml4_base, 0, 0);
    bus.sync(&state);

    // With WP=0, supervisor writes to read-only pages are allowed.
    assert_eq!(bus.write_u8(0, 0x5a), Ok(()));

    // With WP=1, supervisor writes fault with P=1,W=1,U=0.
    state.control.cr0 = CR0_PE | CR0_PG | CR0_WP;
    bus.sync(&state);
    assert_eq!(
        bus.write_u8(0, 0),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1),
        })
    );
}

#[test]
fn atomic_rmw_faults_on_user_read_only_pages() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    // User-accessible but read-only page.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_US,
        0,
    );

    phys.write_u8_raw(data_page, 0x7B);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 3);
    bus.sync(&state);

    // Atomic RMWs are write-intent operations, even if the update is a no-op.
    assert_eq!(
        bus.atomic_rmw::<u8, _>(0, |old| (old, old)),
        Err(Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1) | (1 << 2),
        })
    );
}
