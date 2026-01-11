use aero_cpu_core::interrupts::{CpuCore, CpuExit};
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{gpr, CpuMode, CR0_PE, CR0_PG, CR4_PAE, EFER_LME, SEG_ACCESS_PRESENT};
use aero_cpu_core::PagingBus;
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

