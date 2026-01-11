use aero_cpu_core::interp::tier0::exec;
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;

use core::convert::TryInto;

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

    fn write_u8_raw(&mut self, paddr: u64, value: u8) {
        self.data[paddr as usize] = value;
    }

    fn write_u32_raw(&mut self, paddr: u64, value: u32) {
        let off = paddr as usize;
        self.data[off..off + 4].copy_from_slice(&value.to_le_bytes());
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

#[test]
fn paging_disabled_is_identity() {
    let mut phys = TestMemory::new(0x10000);
    phys.write_u8_raw(0x5678, 0xAA);

    let mut bus = PagingBus::new(phys);

    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE; // paging disabled
    state.update_mode();
    bus.sync(&state);

    assert_eq!(bus.read_u8(0x5678).unwrap(), 0xAA);
    // With paging disabled, linear addresses are 32-bit.
    assert_eq!(bus.read_u8(0x1_0000_0000u64 + 0x5678).unwrap(), 0xAA);
}

#[test]
fn legacy32_paging_page_fault_sets_error_code_and_cr2() {
    // Page tables:
    //  - PDE[0] -> PT
    //  - PTE[0] -> code page
    //  - PTE[1] not present (fault target)
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;

    let mut phys = TestMemory::new(0x10000);

    const PTE_P: u32 = 1 << 0;
    const PTE_RW: u32 = 1 << 1;
    const PTE_US: u32 = 1 << 2;
    let flags = PTE_P | PTE_RW | PTE_US;

    phys.write_u32_raw(pd_base + 0 * 4, (pt_base as u32) | flags);
    phys.write_u32_raw(pt_base + 0 * 4, (code_page as u32) | flags);

    // mov eax, dword ptr [0x00001000]
    let code = [0xA1, 0x00, 0x10, 0x00, 0x00];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);

    let mut state = CpuState::new(CpuMode::Protected);
    // Simulate user mode so the U/S bit is set in the #PF error code.
    state.segments.cs.selector = 3;
    state.control.cr3 = pd_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();

    state.set_rip(0);

    let err = exec::step(&mut state, &mut bus).unwrap_err();
    assert_eq!(
        err,
        Exception::PageFault {
            addr: 0x1000,
            // P=0 (not-present), W/R=0 (read), U/S=1 (user), RSVD=0, I/D=0.
            error_code: 1 << 2,
        }
    );
    assert_eq!(state.control.cr2, 0x1000);
}

#[test]
fn long_mode_non_canonical_is_gp0() {
    let mut phys = TestMemory::new(0x10000);
    phys.write_u8_raw(0, 0xCC);

    let mut bus = PagingBus::new(phys);

    let mut state = CpuState::new(CpuMode::Long);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME;
    state.update_mode();
    bus.sync(&state);

    let non_canonical = 0x0001_0000_0000_0000u64;
    assert_eq!(bus.read_u8(non_canonical), Err(Exception::gp0()));
}
