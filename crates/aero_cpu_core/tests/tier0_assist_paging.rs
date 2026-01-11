use aero_cpu_core::assist::{handle_assist, AssistContext};
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG};
use aero_cpu_core::{AssistReason, Exception, PagingBus};
use aero_mmu::MemoryBus;
use aero_x86::Register;
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

const PTE_P: u32 = 1 << 0;
const PTE_RW: u32 = 1 << 1;

#[test]
fn assist_page_fault_updates_cr2() {
    // Page tables:
    //  - PDE[0] -> PT
    //  - PTE[0] -> code page
    //  - PTE[4] not present (fault target at 0x4000)
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;

    let mut phys = TestMemory::new(0x10000);
    let flags = PTE_P | PTE_RW;

    phys.write_u32_raw(pd_base + 0 * 4, (pt_base as u32) | flags);
    phys.write_u32_raw(pt_base + 0 * 4, (code_page as u32) | flags);

    // lgdt [0x00004000]
    let code = [0x0F, 0x01, 0x15, 0x00, 0x40, 0x00, 0x00];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr3 = pd_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();
    state.set_rip(0);

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1);
    match res.exit {
        BatchExit::Exception(Exception::PageFault { addr, .. }) => assert_eq!(addr, 0x4000),
        other => panic!("expected #PF from assist, got {other:?}"),
    }

    assert_eq!(state.control.cr2, 0x4000);
}

#[test]
fn handle_assist_syncs_bus_and_updates_cr2() {
    // Same as `assist_page_fault_updates_cr2`, but calls `assist::handle_assist`
    // directly (without an explicit `bus.sync(&state)` call by the caller).
    //
    // This ensures the public assist surface is self-contained for non-batch
    // users.
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;

    let mut phys = TestMemory::new(0x10000);
    let flags = PTE_P | PTE_RW;

    phys.write_u32_raw(pd_base + 0 * 4, (pt_base as u32) | flags);
    phys.write_u32_raw(pt_base + 0 * 4, (code_page as u32) | flags);

    // lgdt [0x00004000]
    let code = [0x0F, 0x01, 0x15, 0x00, 0x40, 0x00, 0x00];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr3 = pd_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();
    state.set_rip(0);

    let mut ctx = AssistContext::default();
    let err = handle_assist(&mut ctx, &mut state, &mut bus, AssistReason::Privileged).unwrap_err();
    match err {
        Exception::PageFault { addr, .. } => assert_eq!(addr, 0x4000),
        other => panic!("expected #PF from assist, got {other:?}"),
    }
    assert_eq!(state.control.cr2, 0x4000);
}

#[test]
fn handle_assist_syncs_bus_after_cr3_write() {
    // Ensure `handle_assist` syncs paging state back into the bus after a
    // paging-related state update (e.g. MOV CR3).
    let pd1_base = 0x1000u64;
    let pt1_base = 0x2000u64;
    let pd2_base = 0x4000u64;
    let pt2_base = 0x5000u64;
    let code_page = 0x6000u64;
    let page_a = 0x7000u64;
    let page_b = 0x8000u64;

    let mut phys = TestMemory::new(0x10000);
    let flags = PTE_P | PTE_RW;

    // PD1: map linear 0x0000_0000 -> code_page, 0x0000_1000 -> page_a.
    phys.write_u32_raw(pd1_base + 0 * 4, (pt1_base as u32) | flags);
    phys.write_u32_raw(pt1_base + 0 * 4, (code_page as u32) | flags);
    phys.write_u32_raw(pt1_base + 1 * 4, (page_a as u32) | flags);

    // PD2: map linear 0x0000_1000 -> page_b (code mapping not required after the CR3 write).
    phys.write_u32_raw(pd2_base + 0 * 4, (pt2_base as u32) | flags);
    phys.write_u32_raw(pt2_base + 1 * 4, (page_b as u32) | flags);

    phys.write_u32_raw(page_a, 0x1111_1111);
    phys.write_u32_raw(page_b, 0x2222_2222);

    // mov cr3, eax
    let code = [0x0F, 0x22, 0xD8];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr3 = pd1_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();
    state.set_rip(0);
    state.write_reg(Register::EAX, pd2_base);

    let mut ctx = AssistContext::default();
    handle_assist(&mut ctx, &mut state, &mut bus, AssistReason::Privileged).expect("assist");

    // If the bus wasn't synced after the MOV CR3, this read would still use PD1 and
    // return page A's value.
    assert_eq!(bus.read_u32(0x1000).unwrap(), 0x2222_2222);
}

#[test]
fn invlpg_flushes_pagingbus_translation() {
    // Page tables:
    //  - PDE[0] -> PT
    //  - PTE[0] -> code page
    //  - PTE[1] -> data page (patched after first load)
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;
    let page_a = 0x4000u64;
    let page_b = 0x5000u64;

    let mut phys = TestMemory::new(0x10000);
    let flags = PTE_P | PTE_RW;

    phys.write_u32_raw(pd_base + 0 * 4, (pt_base as u32) | flags);
    phys.write_u32_raw(pt_base + 0 * 4, (code_page as u32) | flags);
    phys.write_u32_raw(pt_base + 1 * 4, (page_a as u32) | flags);

    // Place distinguishable values in the backing physical pages.
    phys.write_u32_raw(page_a, 0x1111_1111);
    phys.write_u32_raw(page_b, 0x2222_2222);

    // mov eax, dword ptr [0x00001000]
    // invlpg [0x00001000]
    // mov eax, dword ptr [0x00001000]
    let code: Vec<u8> = vec![
        0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
        0x0F, 0x01, 0x3D, 0x00, 0x10, 0x00, 0x00, // invlpg [0x1000]
        0xA1, 0x00, 0x10, 0x00, 0x00, // mov eax, [0x1000]
    ];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr3 = pd_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();
    state.set_rip(0);

    let mut ctx = AssistContext::default();

    // First load: should observe page A and populate the TLB.
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(state.read_reg(Register::EAX) as u32, 0x1111_1111);

    // Patch the PTE to point to page B without changing CR3.
    bus.inner_mut()
        .write_u32_raw(pt_base + 1 * 4, (page_b as u32) | flags);

    // INVLPG should flush the cached translation so the second load sees page B.
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 2);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(ctx.invlpg_log, vec![0x1000]);
    assert_eq!(state.read_reg(Register::EAX) as u32, 0x2222_2222);
}

#[test]
fn invlpg_flushes_translation_for_wrapped_linear_address() {
    // Same setup as `invlpg_flushes_pagingbus_translation`, but exercises the
    // non-long mode linear-address wraparound: (segment_base + offset) is
    // truncated to 32 bits.
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;
    let page_a = 0x4000u64;
    let page_b = 0x5000u64;

    let mut phys = TestMemory::new(0x10000);
    let flags = PTE_P | PTE_RW;

    phys.write_u32_raw(pd_base + 0 * 4, (pt_base as u32) | flags);
    phys.write_u32_raw(pt_base + 0 * 4, (code_page as u32) | flags);
    phys.write_u32_raw(pt_base + 1 * 4, (page_a as u32) | flags);

    phys.write_u32_raw(page_a, 0x1111_1111);
    phys.write_u32_raw(page_b, 0x2222_2222);

    // We will access linear address 0x1000, but do so via DS.base + disp32 where
    // the sum overflows 32 bits:
    //   DS.base = 0xFFFF_F000
    //   disp32  = 0x0000_2000
    //   linear  = 0x1_0000_1000 -> 0x0000_1000 (32-bit wrap)
    //
    // mov eax, dword ptr [0x00002000]
    // invlpg [0x00002000]
    // mov eax, dword ptr [0x00002000]
    let code: Vec<u8> = vec![
        0xA1, 0x00, 0x20, 0x00, 0x00, // mov eax, [0x2000]
        0x0F, 0x01, 0x3D, 0x00, 0x20, 0x00, 0x00, // invlpg [0x2000]
        0xA1, 0x00, 0x20, 0x00, 0x00, // mov eax, [0x2000]
    ];
    for (i, b) in code.iter().copied().enumerate() {
        phys.write_u8_raw(code_page + i as u64, b);
    }

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr3 = pd_base;
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr4 = 0;
    state.update_mode();
    state.set_rip(0);
    state.segments.ds.base = 0xFFFF_F000;

    let mut ctx = AssistContext::default();

    // First load: should observe page A and populate the TLB for linear 0x1000.
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(state.read_reg(Register::EAX) as u32, 0x1111_1111);

    // Patch the PTE for linear 0x1000 to point to page B without changing CR3.
    bus.inner_mut()
        .write_u32_raw(pt_base + 1 * 4, (page_b as u32) | flags);

    // INVLPG must invalidate based on the architecturally correct linear address
    // (32-bit wrap), so the second load sees page B.
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 2);
    assert_eq!(res.exit, BatchExit::Completed);
    assert_eq!(ctx.invlpg_log, vec![0x1000]);
    assert_eq!(state.read_reg(Register::EAX) as u32, 0x2222_2222);
}
