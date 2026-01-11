use aero_cpu_core::interrupts::{CpuCore, CpuExit};
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{gpr, CpuMode, CR0_PE, CR0_PG, CR4_PAE, EFER_LME, SEG_ACCESS_PRESENT};
use aero_cpu_core::{Exception, PagingBus};
use aero_mmu::MemoryBus;
use core::convert::TryInto;

const PTE_P: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_US: u64 = 1 << 2;

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

fn write_idt_gate64(
    mem: &mut impl MemoryBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u64,
    ist: u8,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 16;
    mem.write_u16(addr, (offset & 0xFFFF) as u16);
    mem.write_u16(addr + 2, selector);
    mem.write_u8(addr + 4, ist & 0x7);
    mem.write_u8(addr + 5, type_attr);
    mem.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16);
    mem.write_u32(addr + 8, ((offset >> 32) & 0xFFFF_FFFF) as u32);
    mem.write_u32(addr + 12, 0);
}

fn set_pte(mem: &mut impl MemoryBus, pt_base: u64, page_idx: u64, flags: u64) {
    mem.write_u64(pt_base + page_idx * 8, (page_idx * 0x1000) | flags);
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
    mem.write_u16(addr + 6, ((offset >> 16) & 0xFFFF) as u16);
}

fn set_pte32(mem: &mut impl MemoryBus, pt_base: u64, page_idx: u64, flags: u32) {
    mem.write_u32(pt_base + page_idx * 4, ((page_idx * 0x1000) as u32) | flags);
}

#[test]
fn long_mode_interrupt_delivery_can_access_supervisor_idt_tss_and_stack() -> Result<(), CpuExit> {
    // Physical memory layout (identity-mapped for low pages):
    // - Guest pages:
    //   - 0x0000: user code (unused)
    //   - 0x1000: IDT (supervisor)
    //   - 0x2000: handler (supervisor)
    //   - 0x4000: TSS (supervisor)
    //   - 0x7000: user stack
    //   - 0x9000: kernel stack
    // - Page tables live at high physical addresses and are accessed via CR3.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x10000u64;
    let pdpt_base = 0x11000u64;
    let pd_base = 0x12000u64;
    let pt_base = 0x13000u64;

    // Top-level tables (permit user access; leaf PTE controls U/S).
    phys.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    // Leaf mappings for the pages we touch.
    set_pte(&mut phys, pt_base, 0x0, PTE_P | PTE_RW | PTE_US); // user code
    set_pte(&mut phys, pt_base, 0x1, PTE_P | PTE_RW); // IDT supervisor-only
    set_pte(&mut phys, pt_base, 0x2, PTE_P | PTE_RW); // handler supervisor-only
    set_pte(&mut phys, pt_base, 0x4, PTE_P | PTE_RW); // TSS supervisor-only
    set_pte(&mut phys, pt_base, 0x7, PTE_P | PTE_RW | PTE_US); // user stack
    set_pte(&mut phys, pt_base, 0x9, PTE_P | PTE_RW); // kernel stack supervisor-only

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let handler = 0x2000u64;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u64;
    let kernel_stack_top = 0xA000u64;

    // IDT[0x80] -> handler, user-callable interrupt gate.
    write_idt_gate64(bus.inner_mut(), idt_base, 0x80, 0x08, handler, 0, 0xEE);
    // Provide a #GP gate so any unexpected faults during delivery don't instantly triple fault.
    write_idt_gate64(bus.inner_mut(), idt_base, 13, 0x08, handler, 0, 0x8E);

    // TSS.RSP0 at offset +4.
    bus.inner_mut().write_u64(tss_base + 4, kernel_stack_top);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pml4_base;
    cpu.state.control.cr4 = CR4_PAE;
    cpu.state.msr.efer = EFER_LME;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x33; // CPL3
    cpu.state.segments.ss.selector = 0x2B;
    cpu.state.write_gpr64(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_eq!(cpu.state.rip(), handler);
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), kernel_stack_top - 40);

    // Stack frame (top -> bottom): RIP, CS, RFLAGS, old RSP, old SS.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(bus.read_u64(frame_base).unwrap(), return_rip);
    assert_eq!(bus.read_u64(frame_base + 8).unwrap(), 0x33);
    assert_ne!(bus.read_u64(frame_base + 16).unwrap() & 0x200, 0);
    assert_eq!(bus.read_u64(frame_base + 24).unwrap(), user_stack_top);
    assert_eq!(bus.read_u64(frame_base + 32).unwrap(), 0x2B);

    Ok(())
}

#[test]
fn long_mode_interrupt_delivery_can_use_ist_stack_under_paging() -> Result<(), CpuExit> {
    // Same setup as `long_mode_interrupt_delivery_can_access_supervisor_idt_tss_and_stack`, but
    // use IST1 and ensure it overrides RSP0.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x10000u64;
    let pdpt_base = 0x11000u64;
    let pd_base = 0x12000u64;
    let pt_base = 0x13000u64;

    phys.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    set_pte(&mut phys, pt_base, 0x0, PTE_P | PTE_RW | PTE_US); // user code
    set_pte(&mut phys, pt_base, 0x1, PTE_P | PTE_RW); // IDT supervisor-only
    set_pte(&mut phys, pt_base, 0x2, PTE_P | PTE_RW); // handler supervisor-only
    set_pte(&mut phys, pt_base, 0x4, PTE_P | PTE_RW); // TSS supervisor-only
    set_pte(&mut phys, pt_base, 0x7, PTE_P | PTE_RW | PTE_US); // user stack
    set_pte(&mut phys, pt_base, 0x9, PTE_P | PTE_RW); // IST stack supervisor-only
    set_pte(&mut phys, pt_base, 0xB, PTE_P | PTE_RW); // RSP0 stack supervisor-only

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let handler = 0x2000u64;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u64;
    let ist_stack_top = 0xA000u64;
    let rsp0_stack_top = 0xC000u64;

    write_idt_gate64(bus.inner_mut(), idt_base, 0x80, 0x08, handler, 1, 0xEE);
    write_idt_gate64(bus.inner_mut(), idt_base, 13, 0x08, handler, 0, 0x8E);

    bus.inner_mut().write_u64(tss_base + 4, rsp0_stack_top);
    // 64-bit TSS: IST1 at +0x24.
    bus.inner_mut().write_u64(tss_base + 0x24, ist_stack_top);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pml4_base;
    cpu.state.control.cr4 = CR4_PAE;
    cpu.state.msr.efer = EFER_LME;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x33; // CPL3
    cpu.state.segments.ss.selector = 0x2B;
    cpu.state.write_gpr64(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0);
    assert_eq!(cpu.state.rip(), handler);
    assert_eq!(cpu.state.read_gpr64(gpr::RSP), ist_stack_top - 40);
    assert_ne!(cpu.state.read_gpr64(gpr::RSP), rsp0_stack_top - 40);

    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(bus.read_u64(frame_base).unwrap(), return_rip);
    assert_eq!(bus.read_u64(frame_base + 8).unwrap(), 0x33);
    assert_ne!(bus.read_u64(frame_base + 16).unwrap() & 0x200, 0);
    assert_eq!(bus.read_u64(frame_base + 24).unwrap(), user_stack_top);
    assert_eq!(bus.read_u64(frame_base + 32).unwrap(), 0x2B);

    Ok(())
}

#[test]
fn long_mode_stack_page_fault_during_interrupt_delivery_delivers_pf_using_ist(
) -> Result<(), CpuExit> {
    // Trigger a page fault during the first stack push while delivering an interrupt.
    // The page fault handler uses IST1 so it can be delivered even when the current stack
    // page is not present.
    let mut phys = TestMemory::new(0x20000);

    let pml4_base = 0x10000u64;
    let pdpt_base = 0x11000u64;
    let pd_base = 0x12000u64;
    let pt_base = 0x13000u64;

    phys.write_u64(pml4_base, pdpt_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pdpt_base, pd_base | PTE_P | PTE_RW | PTE_US);
    phys.write_u64(pd_base, pt_base | PTE_P | PTE_RW | PTE_US);

    // Leaf mappings for the pages we touch.
    set_pte(&mut phys, pt_base, 0x1, PTE_P | PTE_RW); // IDT supervisor-only
    set_pte(&mut phys, pt_base, 0x2, PTE_P | PTE_RW); // interrupt handler supervisor-only
    set_pte(&mut phys, pt_base, 0x3, PTE_P | PTE_RW); // #PF handler supervisor-only
    set_pte(&mut phys, pt_base, 0x4, PTE_P | PTE_RW); // TSS supervisor-only
                                                      // Page 0x8 (0x8000..0x8FFF) intentionally left not-present: stack push faults there.
    set_pte(&mut phys, pt_base, 0x9, PTE_P | PTE_RW); // IST stack supervisor-only

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let int_handler = 0x2000u64;
    let pf_handler = 0x3000u64;
    let tss_base = 0x4000u64;

    let stack_top = 0x9000u64;
    let ist1_top = 0xA000u64;

    // Interrupt gate at 0x80 (CPL0 is allowed to invoke it) and page fault handler at vector 14.
    write_idt_gate64(bus.inner_mut(), idt_base, 0x80, 0x08, int_handler, 0, 0x8E);
    write_idt_gate64(bus.inner_mut(), idt_base, 14, 0x08, pf_handler, 1, 0x8E);
    // Provide a #GP gate so unexpected faults don't instantly triple fault.
    write_idt_gate64(bus.inner_mut(), idt_base, 13, 0x08, pf_handler, 1, 0x8E);

    // TSS.IST1 at +0x24.
    bus.inner_mut().write_u64(tss_base + 0x24, ist1_top);

    let mut cpu = CpuCore::new(CpuMode::Long);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pml4_base;
    cpu.state.control.cr4 = CR4_PAE;
    cpu.state.msr.efer = EFER_LME;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x0FFF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x08; // CPL0
    cpu.state.segments.ss.selector = 0;
    cpu.state.write_gpr64(gpr::RSP, stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    // Stack push into the not-present page should raise #PF and transfer to its handler.
    assert_eq!(cpu.state.rip(), pf_handler);
    assert_eq!(cpu.state.control.cr2, stack_top - 8);

    // #PF uses IST1: error_code, RIP, CS, RFLAGS, old RSP, old SS.
    let frame_base = cpu.state.read_gpr64(gpr::RSP);
    assert_eq!(frame_base, ist1_top - 48);
    assert_eq!(bus.read_u64(frame_base).unwrap(), 0x2); // not-present + write, supervisor
    assert_eq!(bus.read_u64(frame_base + 8).unwrap(), return_rip);
    assert_eq!(bus.read_u64(frame_base + 16).unwrap(), 0x08);
    assert_ne!(bus.read_u64(frame_base + 24).unwrap() & 0x200, 0);

    Ok(())
}

#[test]
fn protected_mode_interrupt_delivery_can_access_supervisor_idt_tss_and_stack() -> Result<(), CpuExit>
{
    // Physical memory layout (identity-mapped for low pages):
    // - Guest pages:
    //   - 0x0000: user code (unused)
    //   - 0x1000: IDT (supervisor)
    //   - 0x2000: handler (supervisor)
    //   - 0x4000: TSS (supervisor)
    //   - 0x7000: user stack
    //   - 0x9000: kernel stack
    // - Page tables live at high physical addresses and are accessed via CR3.
    let mut phys = TestMemory::new(0x20000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;

    // Top-level PDE permits user access; leaf PTE controls U/S.
    phys.write_u32(pd_base, (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32));

    set_pte32(&mut phys, pt_base, 0x0, PTE_P32 | PTE_RW32 | PTE_US32); // user code
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // IDT supervisor-only
    set_pte32(&mut phys, pt_base, 0x2, PTE_P32 | PTE_RW32); // handler supervisor-only
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // TSS supervisor-only
    set_pte32(&mut phys, pt_base, 0x7, PTE_P32 | PTE_RW32 | PTE_US32); // user stack
    set_pte32(&mut phys, pt_base, 0x9, PTE_P32 | PTE_RW32); // kernel stack supervisor-only

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u32;
    let kernel_stack_top = 0xA000u32;

    // IDT[0x80] -> handler, user-callable interrupt gate.
    write_idt_gate32(bus.inner_mut(), idt_base, 0x80, 0x08, handler, 0xEE);
    // Provide a #GP gate so any unexpected faults during delivery don't instantly triple fault.
    write_idt_gate32(bus.inner_mut(), idt_base, 13, 0x08, handler, 0x8E);

    // TSS.ESP0 and SS0.
    bus.inner_mut().write_u32(tss_base + 4, kernel_stack_top);
    bus.inner_mut().write_u16(tss_base + 8, 0x10);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pd_base;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x07FF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.write_gpr32(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0x10);
    assert_eq!(cpu.state.rip(), handler as u64);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), kernel_stack_top - 20);

    // Stack frame (top -> bottom): EIP, CS, EFLAGS, old ESP, old SS.
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    assert_eq!(bus.read_u32(frame_base).unwrap(), return_rip as u32);
    assert_eq!(bus.read_u32(frame_base + 4).unwrap(), 0x1B);
    assert_ne!(bus.read_u32(frame_base + 8).unwrap() & 0x200, 0);
    assert_eq!(bus.read_u32(frame_base + 12).unwrap(), user_stack_top);
    assert_eq!(bus.read_u32(frame_base + 16).unwrap(), 0x23);

    Ok(())
}

#[test]
fn protected_mode_iret_syncs_bus_before_popping_supervisor_stack() -> Result<(), CpuExit> {
    // Regression: `interrupts::iret` should `bus.sync(state)` before it starts popping
    // the return frame, otherwise `PagingBus` could still be caching CPL=3 and fault
    // while reading from a supervisor-only kernel stack page.
    let mut phys = TestMemory::new(0x20000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;

    phys.write_u32(
        pd_base + 0 * 4,
        (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32),
    );

    set_pte32(&mut phys, pt_base, 0x0, PTE_P32 | PTE_RW32 | PTE_US32); // user code
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // IDT supervisor-only
    set_pte32(&mut phys, pt_base, 0x2, PTE_P32 | PTE_RW32); // handler supervisor-only
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // TSS supervisor-only
    set_pte32(&mut phys, pt_base, 0x7, PTE_P32 | PTE_RW32 | PTE_US32); // user stack
    set_pte32(&mut phys, pt_base, 0x9, PTE_P32 | PTE_RW32); // kernel stack supervisor-only

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let handler = 0x2000u32;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u32;
    let kernel_stack_top = 0xA000u32;

    write_idt_gate32(bus.inner_mut(), idt_base, 0x80, 0x08, handler, 0xEE);
    write_idt_gate32(bus.inner_mut(), idt_base, 13, 0x08, handler, 0x8E);

    bus.inner_mut().write_u32(tss_base + 4, kernel_stack_top);
    bus.inner_mut().write_u16(tss_base + 8, 0x10);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pd_base;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x07FF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.write_gpr32(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0x10);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), kernel_stack_top - 20);

    // Force the bus to cache CPL=3, then return state to CPL0 without syncing the bus.
    cpu.state.segments.cs.selector = 0x1B;
    bus.sync(&cpu.state);
    cpu.state.segments.cs.selector = 0x08;

    cpu.iret(&mut bus)?;

    assert_eq!(cpu.state.segments.cs.selector, 0x1B);
    assert_eq!(cpu.state.segments.ss.selector, 0x23);
    assert_eq!(cpu.state.rip(), return_rip);
    assert_eq!(cpu.state.read_gpr32(gpr::RSP), user_stack_top);

    // The bus should now also be synced to CPL3 so supervisor-only pages still fault.
    assert_eq!(
        bus.read_u8(idt_base),
        Err(Exception::PageFault {
            addr: idt_base,
            error_code: 0b00101, // P=1, W/R=0, U/S=1
        })
    );

    Ok(())
}

#[test]
fn push_during_interrupt_delivery_page_faults() -> Result<(), CpuExit> {
    // Arrange paging so the user stack page is not-present. Deliver a user-mode software INT
    // to a ring-3 handler (no stack switch); the first stack push should raise #PF.
    //
    // The resulting #PF should then be delivered via a ring-0 handler, switching to the kernel
    // stack via the TSS.
    let mut phys = TestMemory::new(0x30000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;

    phys.write_u32(
        pd_base + 0 * 4,
        (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32),
    );

    // Identity-map the pages we need. Leave 0x7000 (user stack) not-present.
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // IDT (supervisor)
    set_pte32(&mut phys, pt_base, 0x2, PTE_P32 | PTE_RW32 | PTE_US32); // INT handler (user)
    set_pte32(&mut phys, pt_base, 0x3, PTE_P32 | PTE_RW32); // #PF handler (supervisor)
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // TSS (supervisor)
                                                            // 0x7: user stack page intentionally not present.
    set_pte32(&mut phys, pt_base, 0x9, PTE_P32 | PTE_RW32); // kernel stack (supervisor)
    set_pte32(&mut phys, pt_base, 0x5, PTE_P32 | PTE_RW32); // #SS handler
    set_pte32(&mut phys, pt_base, 0x6, PTE_P32 | PTE_RW32); // #GP handler

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let int_handler = 0x2000u32;
    let pf_handler = 0x3000u32;
    let ss_handler = 0x5000u32;
    let gp_handler = 0x6000u32;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u32;
    let kernel_stack_top = 0xA000u32;

    // INT 0x80 gate (ring-3 handler, DPL3 so CPL3 can invoke it).
    write_idt_gate32(bus.inner_mut(), idt_base, 0x80, 0x1B, int_handler, 0xEE);
    // Provide handlers for the relevant exceptions.
    write_idt_gate32(bus.inner_mut(), idt_base, 14, 0x08, pf_handler, 0x8E);
    write_idt_gate32(bus.inner_mut(), idt_base, 12, 0x08, ss_handler, 0x8E);
    write_idt_gate32(bus.inner_mut(), idt_base, 13, 0x08, gp_handler, 0x8E);

    // 32-bit TSS: ESP0 and SS0.
    bus.inner_mut().write_u32(tss_base + 4, kernel_stack_top);
    bus.inner_mut().write_u16(tss_base + 8, 0x10);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pd_base;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.write_gpr32(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let return_rip = 0x5555u64;
    cpu.pending.raise_software_interrupt(0x80, return_rip);
    cpu.deliver_pending_event(&mut bus)?;

    // The interrupt delivery should have faulted and delivered #PF instead.
    assert_eq!(cpu.state.rip(), pf_handler as u64);
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0x10);

    // The faulting address should be the first attempted push (EFLAGS) to the user stack.
    let expected_cr2 = (user_stack_top - 4) as u64;
    assert_eq!(cpu.state.control.cr2, expected_cr2);

    // Ensure the delivered #PF error code reflects a user-mode write to a not-present page.
    bus.sync(&cpu.state);
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    let error_code = bus.read_u32(frame_base).unwrap();
    assert_eq!(error_code & 0x1, 0); // P=0 (not present)
    assert_ne!(error_code & 0x2, 0); // W/R=1 (write)
    assert_ne!(error_code & 0x4, 0); // U/S=1 (user)

    Ok(())
}

#[test]
fn idt_read_page_faults() -> Result<(), CpuExit> {
    // Arrange an IDT that spans two pages and unmap the page that contains the target vector's
    // entry. Reading the gate should raise #PF (not #GP), and CR2 should point at the IDT entry.
    let mut phys = TestMemory::new(0x30000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;
    phys.write_u32(
        pd_base + 0 * 4,
        (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32),
    );

    // IDT base chosen so vector 0xFF lives in the next page.
    let idt_base = 0x1900u64;
    let faulting_entry_addr = idt_base + (0xFFu64 * 8);

    // Map page containing low vectors (including #PF/#GP handlers).
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // 0x1000
                                                            // Leave 0x2000 (page 0x2) not present so IDT[0xFF] gate read faults.

    // Map handlers + stack.
    set_pte32(&mut phys, pt_base, 0x3, PTE_P32 | PTE_RW32); // vector 0xFF handler (unused)
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // #GP handler
    set_pte32(&mut phys, pt_base, 0x5, PTE_P32 | PTE_RW32); // #PF handler
    set_pte32(&mut phys, pt_base, 0x7, PTE_P32 | PTE_RW32); // stack

    let mut bus = PagingBus::new(phys);

    let test_handler = 0x3000u32;
    let gp_handler = 0x4000u32;
    let pf_handler = 0x5000u32;

    write_idt_gate32(bus.inner_mut(), idt_base, 0xFF, 0x08, test_handler, 0x8E);
    write_idt_gate32(bus.inner_mut(), idt_base, 13, 0x08, gp_handler, 0x8E);
    write_idt_gate32(bus.inner_mut(), idt_base, 14, 0x08, pf_handler, 0x8E);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pd_base;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.segments.cs.selector = 0x08; // CPL0
    cpu.state.segments.ss.selector = 0x10;
    cpu.state.write_gpr32(gpr::RSP, 0x8000);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    cpu.pending.raise_software_interrupt(0xFF, 0x1234);
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.rip(), pf_handler as u64);
    assert_eq!(cpu.state.control.cr2, faulting_entry_addr);

    // Not-present supervisor read => error_code == 0.
    bus.sync(&cpu.state);
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    assert_eq!(bus.read_u32(frame_base).unwrap(), 0);

    Ok(())
}

#[test]
fn nested_pf_during_pf_delivery_escalates_to_df() -> Result<(), CpuExit> {
    // Trigger a #PF delivery from CPL3 with a ring-3 page fault handler so the delivery uses the
    // current (user) stack. Leave the user stack not-present so pushing the #PF frame triggers a
    // nested #PF. The nested #PF should escalate to #DF, and CR2 must be updated to the nested
    // faulting address even though #DF is delivered.
    let mut phys = TestMemory::new(0x30000);

    let pd_base = 0x10000u64;
    let pt_base = 0x11000u64;
    phys.write_u32(
        pd_base + 0 * 4,
        (pt_base as u32) | (PTE_P32 | PTE_RW32 | PTE_US32),
    );

    // Identity-map required pages. Leave 0x7000 (user stack) not-present.
    set_pte32(&mut phys, pt_base, 0x1, PTE_P32 | PTE_RW32); // IDT
    set_pte32(&mut phys, pt_base, 0x2, PTE_P32 | PTE_RW32 | PTE_US32); // #PF handler
    set_pte32(&mut phys, pt_base, 0x4, PTE_P32 | PTE_RW32); // TSS
    set_pte32(&mut phys, pt_base, 0x5, PTE_P32 | PTE_RW32); // #DF handler
    set_pte32(&mut phys, pt_base, 0x9, PTE_P32 | PTE_RW32); // kernel stack

    let mut bus = PagingBus::new(phys);

    let idt_base = 0x1000u64;
    let pf_handler = 0x2000u32;
    let df_handler = 0x5000u32;
    let tss_base = 0x4000u64;
    let user_stack_top = 0x8000u32;
    let kernel_stack_top = 0xA000u32;

    // #PF handler deliberately uses a ring-3 code selector so delivery does not stack-switch.
    write_idt_gate32(bus.inner_mut(), idt_base, 14, 0x1B, pf_handler, 0x8E);
    // #DF handler is ring-0 so it can be delivered via a TSS stack switch.
    write_idt_gate32(bus.inner_mut(), idt_base, 8, 0x08, df_handler, 0x8E);

    // 32-bit TSS: ESP0 and SS0.
    bus.inner_mut().write_u32(tss_base + 4, kernel_stack_top);
    bus.inner_mut().write_u16(tss_base + 8, 0x10);

    let mut cpu = CpuCore::new(CpuMode::Protected);
    cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.state.control.cr3 = pd_base;
    cpu.state.update_mode();

    cpu.state.tables.idtr.base = idt_base;
    cpu.state.tables.idtr.limit = 0x7FF;
    cpu.state.tables.tr.selector = 0x40;
    cpu.state.tables.tr.base = tss_base;
    cpu.state.tables.tr.limit = 0x67;
    cpu.state.tables.tr.access = SEG_ACCESS_PRESENT | 0x9;

    cpu.state.segments.cs.selector = 0x1B; // CPL3
    cpu.state.segments.ss.selector = 0x23;
    cpu.state.write_gpr32(gpr::RSP, user_stack_top);
    cpu.state.set_rflags(0x202);

    bus.sync(&cpu.state);

    let initial_cr2 = 0xCAFE_BABEu64;
    let fault_rip = 0x1234u64;
    cpu.pending.raise_exception_fault(
        &mut cpu.state,
        aero_cpu_core::exceptions::Exception::PageFault,
        fault_rip,
        Some(0),
        Some(initial_cr2),
    );
    cpu.deliver_pending_event(&mut bus)?;

    assert_eq!(cpu.state.rip(), df_handler as u64);
    assert_eq!(cpu.state.segments.cs.selector, 0x08);
    assert_eq!(cpu.state.segments.ss.selector, 0x10);

    // CR2 should reflect the nested #PF from attempting to push to the user stack.
    let expected_cr2 = (user_stack_top - 4) as u64;
    assert_eq!(cpu.state.control.cr2, expected_cr2);

    // Validate the #DF stack frame (top -> bottom): error_code, EIP, CS, EFLAGS, old ESP, old SS.
    bus.sync(&cpu.state);
    let frame_base = cpu.state.read_gpr32(gpr::RSP) as u64;
    assert_eq!(bus.read_u32(frame_base).unwrap(), 0);
    assert_eq!(bus.read_u32(frame_base + 4).unwrap(), fault_rip as u32);

    Ok(())
}
