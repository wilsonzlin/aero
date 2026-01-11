use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, CR4_PAE, EFER_LME, SEG_ACCESS_PRESENT};
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

fn set_pte(mem: &mut impl MemoryBus, pt_base: u64, page_idx: u64, flags: u64) {
    mem.write_u64(pt_base + page_idx * 8, (page_idx * 0x1000) | flags);
}

#[test]
fn tss_helper_reads_ignore_user_supervisor_paging_bit() {
    // Like the GDT/IDT, the TSS is a "system structure": it must remain readable
    // even when interrupted code runs with CPL3 and the TSS page is marked
    // supervisor-only (U/S=0).
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x10000u64;
    let pdpt_base = 0x11000u64;
    let pd_base = 0x12000u64;
    let pt_base = 0x13000u64;

    // Identity-mapped long-mode paging.
    phys.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    set_pte(&mut phys, pt_base, 0x0, PTE_P | PTE_RW | PTE_US); // user page
    set_pte(&mut phys, pt_base, 0x4, PTE_P | PTE_RW); // TSS page, supervisor-only

    let mut bus = PagingBus::new(phys);

    let tss_base = 0x4000u64;
    let rsp0 = 0xA000u64;
    let ist1 = 0xB000u64;
    let iomap_base = 0x0068u16;

    bus.inner_mut().write_u64(tss_base + 4, rsp0);
    bus.inner_mut().write_u64(tss_base + 0x24, ist1);
    bus.inner_mut().write_u16(tss_base + 0x66, iomap_base);

    let mut state = CpuState::new(CpuMode::Long);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pml4_base;
    state.control.cr4 = CR4_PAE;
    state.msr.efer = EFER_LME;
    state.update_mode();

    state.segments.cs.selector = 0x33; // CPL3
    state.tables.tr.selector = 0x40;
    state.tables.tr.base = tss_base;
    state.tables.tr.limit = 0x67;
    state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    bus.sync(&state);

    // Sanity: a normal CPL3 data read of the supervisor-only TSS page faults.
    assert_eq!(
        bus.read_u64(tss_base + 4),
        Err(Exception::PageFault {
            addr: tss_base + 4,
            error_code: 0b00101, // P=1, W/R=0, U/S=1
        })
    );

    assert_eq!(state.tss64_rsp0(&mut bus).unwrap(), rsp0);
    assert_eq!(state.tss64_ist(&mut bus, 1).unwrap(), ist1);
    assert_eq!(state.tss64_iomap_base(&mut bus).unwrap(), iomap_base);

    assert_eq!(
        bus.read_u64(tss_base + 4),
        Err(Exception::PageFault {
            addr: tss_base + 4,
            error_code: 0b00101,
        })
    );
    assert_eq!(state.segments.cs.selector, 0x33);
}

