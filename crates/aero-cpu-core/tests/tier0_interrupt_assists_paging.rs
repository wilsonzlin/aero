use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{CpuMode, CpuState, CR0_PE, CR0_PG, RFLAGS_IF, RFLAGS_RESERVED1};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;
use aero_x86::Register;

const PTE_P32: u32 = 1 << 0;
const PTE_RW32: u32 = 1 << 1;
const PTE_US32: u32 = 1 << 2;

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

fn set_pte32(mem: &mut impl MemoryBus, pt_base: u64, page_idx: u64, flags: u32) {
    mem.write_u32(
        pt_base + page_idx * 4,
        ((page_idx * 0x1000) as u32) | flags,
    );
}

fn seg_desc(base: u32, limit: u32, typ: u8, dpl: u8) -> u64 {
    // 8-byte segment descriptor (legacy 32-bit format).
    let limit_low = (limit & 0xFFFF) as u64;
    let base_low = (base & 0xFFFF) as u64;
    let base_mid = ((base >> 16) & 0xFF) as u64;
    let base_high = ((base >> 24) & 0xFF) as u64;
    let limit_high = ((limit >> 16) & 0xF) as u64;

    let s = 1u64; // code/data
    let present = 1u64;
    let db = 1u64; // 32-bit
    let g = 1u64; // 4K granularity

    let access = (typ as u64 & 0xF) | (s << 4) | ((dpl as u64 & 0x3) << 5) | (present << 7);
    let flags = (db << 2) | (g << 3);

    limit_low
        | (base_low << 16)
        | (base_mid << 32)
        | (access << 40)
        | (limit_high << 48)
        | (flags << 52)
        | (base_high << 56)
}

fn write_idt_gate32(
    mem: &mut impl MemoryBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 8;
    mem.write_u16(addr, (offset & 0xFFFF) as u16);
    mem.write_u16(addr + 2, selector);
    mem.write_u8(addr + 4, 0);
    mem.write_u8(addr + 5, type_attr);
    mem.write_u16(addr + 6, (offset >> 16) as u16);
}

#[test]
fn tier0_assist_protected_int_stack_switch_ignores_user_supervisor_paging_bit() {
    // Regression test for the assist-path INT implementation used by
    // `run_batch_with_assists`: IDT/GDT/TSS reads and ring-0 stack writes must be
    // treated as supervisor/system accesses even when the interrupted CPL is 3.
    //
    // With `PagingBus`, a naive implementation will #PF when the IDT/TSS/GDT or
    // kernel stack pages are marked supervisor-only.
    let mut phys = TestMemory::new(0x20000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;

    // Top-level PDE permits user access; leaf PTE controls U/S.
    phys.write_u32(
        pd_base + 0 * 4,
        (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32),
    );

    set_pte32(&mut phys, pt_base, 0x0, PTE_P32 | PTE_RW32 | PTE_US32); // user code
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // IDT supervisor-only
    set_pte32(&mut phys, pt_base, 0x2, PTE_P32 | PTE_RW32); // handler supervisor-only
    set_pte32(&mut phys, pt_base, 0x3, PTE_P32 | PTE_RW32); // GDT supervisor-only
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // TSS supervisor-only
    set_pte32(&mut phys, pt_base, 0x7, PTE_P32 | PTE_RW32 | PTE_US32); // user stack
    set_pte32(&mut phys, pt_base, 0x9, PTE_P32 | PTE_RW32); // kernel stack supervisor-only

    // Place the INT instruction at linear/physical 0x0000.
    phys.write_u8(0x0000, 0xCD); // int imm8
    phys.write_u8(0x0001, 0x80); // vector 0x80

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let gdt_base = 0x3000u64;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u32;
    let kernel_stack_top = 0xA000u32;

    // Minimal GDT: null + ring0 code/data + ring3 code/data.
    phys.write_u64(gdt_base + 0x00, 0);
    phys.write_u64(gdt_base + 0x08, seg_desc(0, 0xFFFFF, 0xA, 0)); // ring0 code
    phys.write_u64(gdt_base + 0x10, seg_desc(0, 0xFFFFF, 0x2, 0)); // ring0 data
    phys.write_u64(gdt_base + 0x18, seg_desc(0, 0xFFFFF, 0xA, 3)); // ring3 code
    phys.write_u64(gdt_base + 0x20, seg_desc(0, 0xFFFFF, 0x2, 3)); // ring3 data

    // IDT[0x80] -> handler, user-callable interrupt gate (DPL3).
    write_idt_gate32(&mut phys, idt_base, 0x80, 0x08, handler, 0xEE);

    // TSS32 ring0 stack (SS0:ESP0).
    phys.write_u32(tss_base + 4, kernel_stack_top);
    phys.write_u16(tss_base + 8, 0x10);

    let mut bus = PagingBus::new(phys);
    let mut state = CpuState::new(CpuMode::Protected);
    state.control.cr0 = CR0_PE | CR0_PG;
    state.control.cr3 = pd_base;
    state.update_mode();

    state.set_rip(0);
    state.set_rflags(RFLAGS_RESERVED1 | RFLAGS_IF);
    state.segments.cs.selector = 0x1B; // ring3 code selector (0x18 | RPL3)
    state.segments.ss.selector = 0x23; // ring3 data selector (0x20 | RPL3)
    state.write_reg(Register::ESP, user_stack_top as u64);
    state.tables.gdtr.base = gdt_base;
    state.tables.gdtr.limit = 0x28 - 1;
    state.tables.idtr.base = idt_base;
    state.tables.idtr.limit = (0x80 * 8 + 7) as u16;

    state.tables.tr.selector = 0x28;
    state.tables.tr.base = tss_base;
    state.tables.tr.limit = 0x67;
    state.tables.tr.access = aero_cpu_core::state::SEG_ACCESS_PRESENT | 0x9;

    bus.sync(&state);

    // Sanity: direct CPL3 reads of the supervisor-only IDT page should fault.
    assert_eq!(
        bus.read_u8(idt_base),
        Err(Exception::PageFault {
            addr: idt_base,
            error_code: 0b00101, // P=1, W/R=0, U/S=1
        })
    );

    let mut ctx = AssistContext::default();
    let res = run_batch_with_assists(&mut ctx, &mut state, &mut bus, 1);
    assert_eq!(res.executed, 1);
    assert_eq!(res.exit, BatchExit::Branch);

    assert_eq!(state.segments.cs.selector, 0x08);
    assert_eq!(state.segments.ss.selector, 0x10);
    assert_eq!(state.rip(), handler as u64);
    assert_eq!(state.read_reg(Register::ESP) as u32, kernel_stack_top - 20);

    // Stack frame (top -> bottom): EIP, CS, EFLAGS, old ESP, old SS.
    bus.sync(&state);
    let frame_base = (kernel_stack_top - 20) as u64;
    assert_eq!(bus.read_u32(frame_base).unwrap(), 2); // return EIP
    assert_eq!(bus.read_u32(frame_base + 4).unwrap() as u16, 0x1B); // old CS
    assert_ne!(bus.read_u32(frame_base + 8).unwrap() & 0x200, 0); // old EFLAGS has IF
    assert_eq!(bus.read_u32(frame_base + 12).unwrap(), user_stack_top); // old ESP
    assert_eq!(bus.read_u32(frame_base + 16).unwrap() as u16, 0x23); // old SS
}

