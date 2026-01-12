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
const PTE_A: u64 = 1 << 5;
const PTE_D: u64 = 1 << 6;
const PTE_NX: u64 = 1 << 63;

const PAGE_SIZE: usize = 4096;

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
    mem.write_u64(pt_base, pte0);
    mem.write_u64(pt_base + 8, pte1);
}

fn setup_pae_4k(
    mem: &mut impl MemoryBus,
    pdpt_base: u64,
    pd_base: u64,
    pt_base: u64,
    pte0: u64,
    pte511: u64,
) {
    // IA-32e PDPT (PAE) entries do not have RW/US bits; only bit 0 (P) and a
    // handful of cache/AVL bits are allowed. Keep it minimal to avoid reserved
    // bit faults during the walk.
    mem.write_u64(pdpt_base, pd_base | PTE_P);
    mem.write_u64(pdpt_base + 3 * 8, pd_base | PTE_P);

    // Point both PD[0] and PD[511] at the same PT so we can map 0x0000_0000 and
    // 0xFFFF_F000 with a single 4KiB page table.
    let pde_flags = PTE_P | PTE_RW | PTE_US;
    mem.write_u64(pd_base, pt_base | pde_flags);
    mem.write_u64(pd_base + 511 * 8, pt_base | pde_flags);

    // PTE[0] and PTE[511]
    mem.write_u64(pt_base, pte0);
    mem.write_u64(pt_base + 511 * 8, pte511);
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

fn pae_state(pdpt_base: u64, efer_extra: u64, cpl: u8) -> CpuState {
    let mut state = CpuState::new(CpuMode::Protected);
    state.segments.cs.selector = (state.segments.cs.selector & !0b11) | (cpl as u16 & 0b11);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pdpt_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = efer_extra;
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
fn pagingbus_scalar_accesses_use_wide_physical_reads_and_writes_when_possible() {
    #[derive(Clone, Debug)]
    struct WideIoMemory {
        data: Vec<u8>,
        read_u8_calls: usize,
        read_u32_calls: usize,
        write_u8_calls: usize,
        write_u32_calls: usize,
    }

    impl WideIoMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
                read_u8_calls: 0,
                read_u32_calls: 0,
                write_u8_calls: 0,
                write_u32_calls: 0,
            }
        }

        fn load(&mut self, paddr: u64, bytes: &[u8]) {
            let off = paddr as usize;
            self.data[off..off + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl MemoryBus for WideIoMemory {
        fn read_u8(&mut self, _paddr: u64) -> u8 {
            self.read_u8_calls += 1;
            0
        }

        fn read_u16(&mut self, paddr: u64) -> u16 {
            let off = paddr as usize;
            u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
        }

        fn read_u32(&mut self, paddr: u64) -> u32 {
            self.read_u32_calls += 1;
            let off = paddr as usize;
            u32::from_le_bytes(self.data[off..off + 4].try_into().unwrap())
        }

        fn read_u64(&mut self, paddr: u64) -> u64 {
            let off = paddr as usize;
            u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
        }

        fn write_u8(&mut self, _paddr: u64, _value: u8) {
            self.write_u8_calls += 1;
        }

        fn write_u16(&mut self, paddr: u64, value: u16) {
            let off = paddr as usize;
            self.data[off..off + 2].copy_from_slice(&value.to_le_bytes());
        }

        fn write_u32(&mut self, paddr: u64, value: u32) {
            self.write_u32_calls += 1;
            let off = paddr as usize;
            self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn write_u64(&mut self, paddr: u64, value: u64) {
            let off = paddr as usize;
            self.data[off..off + 8].copy_from_slice(&value.to_le_bytes());
        }
    }

    let mut phys = WideIoMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page = 0x5000u64;

    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page | PTE_P | PTE_RW | PTE_US,
        0,
    );

    phys.load(data_page, &[0x11, 0x22, 0x33, 0x44]);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 0);
    bus.sync(&state);

    bus.inner_mut().read_u8_calls = 0;
    bus.inner_mut().read_u32_calls = 0;

    assert_eq!(bus.read_u32(0).unwrap(), 0x4433_2211);
    assert_eq!(bus.inner_mut().read_u8_calls, 0);
    assert_eq!(bus.inner_mut().read_u32_calls, 1);

    bus.inner_mut().write_u8_calls = 0;
    bus.inner_mut().write_u32_calls = 0;

    bus.write_u32(0, 0xAABB_CCDD).unwrap();
    assert_eq!(bus.inner_mut().write_u8_calls, 0);
    assert_eq!(bus.inner_mut().write_u32_calls, 1);
    assert_eq!(bus.inner_mut().read_u32(data_page), 0xAABB_CCDD);
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
    setup_long4_4k(&mut phys, pml4_base, pdpt_base, pd_base, pt_base, 0, 0);

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
fn tier0_fetch_wrapping_32bit_linear_address_respects_nx() {
    let mut phys = TestMemory::new(0x20000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;
    let pt_base = 0x3000u64;
    let low_page = 0x4000u64;
    let high_page = 0x5000u64;

    setup_pae_4k(
        &mut phys,
        pdpt_base,
        pd_base,
        pt_base,
        low_page | PTE_P | PTE_RW | PTE_US | PTE_NX,
        high_page | PTE_P | PTE_RW | PTE_US,
    );

    // Place `mov al, imm8` such that the opcode byte is at 0xFFFF_FFFF and the
    // immediate byte wraps to 0x0000_0000. The low page is marked NX, so the
    // instruction fetch must #PF there.
    phys.write_u8_raw(high_page + 0xfff, 0xB0);
    phys.write_u8_raw(low_page, 0x5A);

    let mut bus = PagingBus::new(phys);
    let mut state = pae_state(pdpt_base, EFER_NXE, 0);
    state.set_rip(0xFFFF_FFFF);

    let err = step(&mut state, &mut bus).unwrap_err();
    assert_eq!(
        err,
        Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 4), // P=1, I/D=1
        }
    );
    assert_eq!(state.control.cr2, 0);
}

#[test]
fn wrapped_multi_byte_write_is_atomic_wrt_page_faults() {
    // The paging bus guarantees multi-byte writes don't partially commit on #PF.
    // When the architectural linear address space wraps (32-bit truncation),
    // Tier-0 uses `linear_mem::*_wrapped` helpers which must preserve that
    // property even though the access becomes non-contiguous in masked space.
    let mut phys = TestMemory::new(0x20000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;
    let pt_base = 0x3000u64;
    let high_page = 0x4000u64;

    // Map only the high page (0xFFFF_F000). Leave the low page unmapped so the
    // wrapped bytes at 0x0000_0000.. fault.
    setup_pae_4k(
        &mut phys,
        pdpt_base,
        pd_base,
        pt_base,
        0,
        high_page | PTE_P | PTE_RW | PTE_US,
    );

    // Sentinel value at the last byte in the high page (linear 0xFFFF_FFFF).
    phys.write_u8_raw(high_page + 0xfff, 0xaa);

    let mut bus = PagingBus::new(phys);
    let state = pae_state(pdpt_base, 0, 0);
    bus.sync(&state);

    let err =
        aero_cpu_core::linear_mem::write_u32_wrapped(&state, &mut bus, 0xFFFF_FFFF, 0x1234_5678)
            .unwrap_err();
    assert_eq!(
        err,
        Exception::PageFault {
            addr: 0,
            error_code: 1 << 1, // W=1, P=0, U=0
        }
    );

    // The store must not partially commit.
    assert_eq!(bus.inner_mut().read_u8(high_page + 0xfff), 0xaa);
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
fn pagingbus_multi_byte_writes_are_atomic_wrt_page_faults() {
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let data_page0 = 0x5000u64;

    // Only the first page is present; the next page is not present.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        data_page0 | PTE_P | PTE_RW | PTE_US,
        0,
    );

    // Sentinel values at the end of the first page.
    phys.write_u8_raw(data_page0 + 0xffe, 0xaa);
    phys.write_u8_raw(data_page0 + 0xfff, 0xbb);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 0);
    bus.sync(&state);

    // Writing a 16-bit value at 0xFFF crosses into the unmapped page at 0x1000.
    // The write must not partially commit to the mapped page.
    assert_eq!(
        bus.write_u16(0xfff, 0x1234),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );

    assert_eq!(bus.inner_mut().read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.inner_mut().read_u8(data_page0 + 0xfff), 0xbb);

    // Same property for `write_bytes`.
    assert_eq!(
        bus.write_bytes(0xffe, &[1, 2, 3]),
        Err(Exception::PageFault {
            addr: 0x1000,
            error_code: 1 << 1,
        })
    );
    assert_eq!(bus.inner_mut().read_u8(data_page0 + 0xffe), 0xaa);
    assert_eq!(bus.inner_mut().read_u8(data_page0 + 0xfff), 0xbb);
}

#[test]
fn pagingbus_does_not_panic_on_wrapping_linear_addresses() {
    // Map the final 4KiB page in the canonical address space (0xffff...f000),
    // but leave the low page unmapped. Reading 2 bytes at `u64::MAX` should:
    //  - read the first byte from the high page,
    //  - wrap to address 0 for the second byte and fault there,
    //  - never panic from debug overflow checks.
    let mut phys = TestMemory::new(0x10000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;
    let high_page = 0x5000u64;

    // PML4E[511] -> PDPT[511] -> PD[511] -> PT[511] -> high_page
    let idx = 0x1ffu64;
    let flags = PTE_P | PTE_RW | PTE_US;
    phys.write_u64(pml4_base + idx * 8, pdpt_base | flags);
    phys.write_u64(pdpt_base + idx * 8, pd_base | flags);
    phys.write_u64(pd_base + idx * 8, pt_base | flags);
    phys.write_u64(pt_base + idx * 8, high_page | flags);

    // Place a distinguishable byte at the final address.
    phys.write_u8_raw(high_page + 0xfff, 0x90);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 0);
    bus.sync(&state);

    // These accesses previously used `vaddr + i` and could panic on overflow in debug builds.
    assert_eq!(
        bus.fetch(u64::MAX, 2),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 4, // I/D=1, P=0, W=0, U=0
        })
    );
    assert_eq!(
        bus.read_u16(u64::MAX),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 0,
        })
    );
    assert_eq!(
        bus.atomic_rmw::<u16, _>(u64::MAX, |old| (old, old)),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );
    assert_eq!(
        bus.write_u16(u64::MAX, 0x1234),
        Err(Exception::PageFault {
            addr: 0,
            error_code: 1 << 1, // W=1, P=0, U=0
        })
    );
    assert_eq!(bus.inner_mut().read_u8(high_page + 0xfff), 0x90);
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
fn pagingbus_bulk_copy_decline_does_not_update_accessed_or_dirty_bits() -> Result<(), Exception> {
    // Set up long-mode page tables where the destination range spans an unmapped page. The paging
    // bus bulk-copy fast path should return `Ok(false)` and must not update guest page table A/D
    // bits as a side effect of preflight translation.
    let mut phys = TestMemory::new(0x30000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;

    let src_page0 = 0x5000u64;
    let src_page1 = 0x6000u64;
    let dst_page4 = 0x7000u64;

    // Map virtual pages 0 and 1 for the source range.
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        src_page0 | PTE_P | PTE_RW | PTE_US,
        src_page1 | PTE_P | PTE_RW | PTE_US,
    );

    // Map virtual page 4 (0x4000) for the destination range, but leave page 5 unmapped.
    phys.write_u64(pt_base + 4 * 8, dst_page4 | PTE_P | PTE_RW | PTE_US);

    // Fill destination with a sentinel pattern so we can confirm no writes occur on `Ok(false)`.
    phys.load(dst_page4, &[0xCCu8; 32]);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 3);
    bus.sync(&state);

    // Snapshot relevant page table entries.
    let before_pml4e = bus.inner_mut().read_u64(pml4_base);
    let before_pdpte = bus.inner_mut().read_u64(pdpt_base);
    let before_pde = bus.inner_mut().read_u64(pd_base);
    let before_pte0 = bus.inner_mut().read_u64(pt_base);
    let before_pte1 = bus.inner_mut().read_u64(pt_base + 8);
    let before_pte4 = bus.inner_mut().read_u64(pt_base + 4 * 8);

    let dst_before = bus.inner_mut().data[dst_page4 as usize..dst_page4 as usize + 32].to_vec();

    // Copy 8KiB from 0x0000 to 0x4000. The second destination page (0x5000) is unmapped, so the
    // bulk op should decline without side effects.
    assert_eq!(bus.bulk_copy(0x4000, 0, 0x2000)?, false);

    // Page table entries must be unchanged (no accessed/dirty bits set).
    let after_pml4e = bus.inner_mut().read_u64(pml4_base);
    let after_pdpte = bus.inner_mut().read_u64(pdpt_base);
    let after_pde = bus.inner_mut().read_u64(pd_base);
    let after_pte0 = bus.inner_mut().read_u64(pt_base);
    let after_pte1 = bus.inner_mut().read_u64(pt_base + 8);
    let after_pte4 = bus.inner_mut().read_u64(pt_base + 4 * 8);

    assert_eq!(after_pml4e, before_pml4e);
    assert_eq!(after_pdpte, before_pdpte);
    assert_eq!(after_pde, before_pde);
    assert_eq!(after_pte0, before_pte0);
    assert_eq!(after_pte1, before_pte1);
    assert_eq!(after_pte4, before_pte4);

    for entry in [after_pml4e, after_pdpte, after_pde, after_pte0, after_pte1, after_pte4] {
        assert_eq!(entry & (PTE_A | PTE_D), 0);
    }

    // Destination memory must remain untouched.
    assert_eq!(
        &bus.inner_mut().data[dst_page4 as usize..dst_page4 as usize + 32],
        dst_before.as_slice()
    );
    Ok(())
}

#[test]
fn pagingbus_bulk_set_decline_does_not_update_accessed_or_dirty_bits() -> Result<(), Exception> {
    // Set up long-mode page tables where the destination range spans an unmapped page. The paging
    // bus bulk-set fast path should return `Ok(false)` and must not update guest page table A/D
    // bits as a side effect of preflight translation.
    let mut phys = TestMemory::new(0x30000);

    let pml4_base = 0x1000u64;
    let pdpt_base = 0x2000u64;
    let pd_base = 0x3000u64;
    let pt_base = 0x4000u64;

    let page0 = 0x5000u64;
    let page1 = 0x6000u64;
    let dst_page4 = 0x7000u64;

    // Map virtual pages 0 and 1 (not directly used by the test, but provides a minimal valid page
    // table root that we can extend with PTE[4]).
    setup_long4_4k(
        &mut phys,
        pml4_base,
        pdpt_base,
        pd_base,
        pt_base,
        page0 | PTE_P | PTE_RW | PTE_US,
        page1 | PTE_P | PTE_RW | PTE_US,
    );

    // Map virtual page 4 (0x4000) for the destination range, but leave page 5 unmapped.
    phys.write_u64(pt_base + 4 * 8, dst_page4 | PTE_P | PTE_RW | PTE_US);

    // Fill the mapped portion of the destination with a sentinel so we can confirm no writes occur
    // on `Ok(false)`.
    let dst = 0x4F00u64;
    let mapped_len = PAGE_SIZE - (dst as usize & (PAGE_SIZE - 1));
    phys.load(dst_page4 + (dst & 0xFFF), &[0xCDu8; 0x100]);

    let mut bus = PagingBus::new(phys);
    let state = long_state(pml4_base, 0, 3);
    bus.sync(&state);

    // Snapshot relevant page table entries.
    let before_pml4e = bus.inner_mut().read_u64(pml4_base);
    let before_pdpte = bus.inner_mut().read_u64(pdpt_base);
    let before_pde = bus.inner_mut().read_u64(pd_base);
    let before_pte4 = bus.inner_mut().read_u64(pt_base + 4 * 8);
    let before_pte5 = bus.inner_mut().read_u64(pt_base + 5 * 8);

    let dst_phys = dst_page4 + (dst & 0xFFF);
    let dst_before =
        bus.inner_mut().data[dst_phys as usize..dst_phys as usize + mapped_len].to_vec();

    // Write 512 bytes starting near the end of page 4. The write crosses into page 5 which is
    // unmapped, so the bulk op should decline without side effects.
    assert_eq!(bus.bulk_set(dst, &[0xAB], 0x200)?, false);

    // Page table entries must be unchanged (no accessed/dirty bits set).
    let after_pml4e = bus.inner_mut().read_u64(pml4_base);
    let after_pdpte = bus.inner_mut().read_u64(pdpt_base);
    let after_pde = bus.inner_mut().read_u64(pd_base);
    let after_pte4 = bus.inner_mut().read_u64(pt_base + 4 * 8);
    let after_pte5 = bus.inner_mut().read_u64(pt_base + 5 * 8);

    assert_eq!(after_pml4e, before_pml4e);
    assert_eq!(after_pdpte, before_pdpte);
    assert_eq!(after_pde, before_pde);
    assert_eq!(after_pte4, before_pte4);
    assert_eq!(after_pte5, before_pte5);

    for entry in [after_pml4e, after_pdpte, after_pde, after_pte4, after_pte5] {
        assert_eq!(entry & (PTE_A | PTE_D), 0);
    }

    // Destination memory must remain untouched.
    assert_eq!(
        &bus.inner_mut().data[dst_phys as usize..dst_phys as usize + mapped_len],
        dst_before.as_slice()
    );

    Ok(())
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

#[test]
fn tier0_locked_rmw_wrap_faults_with_write_intent_even_if_no_store() {
    // Regression test: Tier-0 must preserve `CpuBus::atomic_rmw` write-intent
    // semantics even when a locked RMW crosses the 32-bit linear-address wrap.
    //
    // Without this, a LOCKed no-op update (e.g. ADD [mem], 0) could incorrectly
    // succeed on read-only pages when the access wraps across 4GiB.
    let mut phys = TestMemory::new(0x20000);

    let pdpt_base = 0x1000u64;
    let pd_base = 0x2000u64;
    let pt_base = 0x3000u64;
    let low_page = 0x4000u64;
    let high_page = 0x5000u64;

    // Map 0x0000_0000 as present/user but read-only, and 0xFFFF_F000 as present/user/writable.
    setup_pae_4k(
        &mut phys,
        pdpt_base,
        pd_base,
        pt_base,
        low_page | PTE_P | PTE_US,
        high_page | PTE_P | PTE_RW | PTE_US,
    );

    // `lock add dword ptr [0xFFFF_FFFF], 0` -- the dword operand wraps to 0x0000_0000.
    // Encoding: F0 81 05 <disp32> <imm32>
    phys.load(
        high_page,
        &[
            0xF0, 0x81, 0x05, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00,
        ],
    );

    let mut bus = PagingBus::new(phys);
    let mut state = pae_state(pdpt_base, 0, 3);
    state.set_rip(0xFFFF_F000);

    let err = step(&mut state, &mut bus).unwrap_err();
    assert_eq!(
        err,
        Exception::PageFault {
            addr: 0,
            error_code: (1 << 0) | (1 << 1) | (1 << 2), // P=1, W=1, U=1
        }
    );
    assert_eq!(state.control.cr2, 0);
}
