use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::interrupts::CpuExit;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, CR0_PE, CR0_PG};
use aero_cpu_core::PagingBus;
use aero_mmu::MemoryBus;

use core::convert::TryInto;

fn write_idt_gate32(
    mem: &mut impl CpuBus,
    base: u64,
    vector: u8,
    selector: u16,
    offset: u32,
    type_attr: u8,
) {
    let addr = base + (vector as u64) * 8;
    mem.write_u16(addr, (offset & 0xFFFF) as u16).unwrap();
    mem.write_u16(addr + 2, selector).unwrap();
    mem.write_u8(addr + 4, 0).unwrap();
    mem.write_u8(addr + 5, type_attr).unwrap();
    mem.write_u16(addr + 6, (offset >> 16) as u16).unwrap();
}

#[test]
fn ud2_is_delivered_through_idt() {
    let mut bus = FlatTestBus::new(0x10000);

    let idt_base = 0x1000u64;
    let code_base = 0x2000u64;
    let handler = 0x3000u32;

    // UD2; HLT (must not execute)
    bus.load(code_base, &[0x0F, 0x0B, 0xF4]);

    // #UD handler: HLT
    bus.load(handler as u64, &[0xF4]);
    write_idt_gate32(&mut bus, idt_base, 6, 0x08, handler, 0x8E);
    // Provide a #GP gate so unexpected delivery issues don't immediately triple fault.
    write_idt_gate32(&mut bus, idt_base, 13, 0x08, handler, 0x8E);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.control.cr0 |= CR0_PE;
    cpu.cpu.state.update_mode();

    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x8000);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.set_rip(code_base);

    let mut interp = Tier0Interpreter::new(1024);

    // First block must deliver #UD and transfer control to the handler.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.exit, None);
    assert_eq!(cpu.cpu.state.rip(), handler as u64);

    // Second block executes the handler's HLT.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.exit, None);
    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.rip(), handler as u64 + 1);
}

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

    fn write_bytes(&mut self, paddr: u64, bytes: &[u8]) {
        let start = paddr as usize;
        let end = start + bytes.len();
        self.data[start..end].copy_from_slice(bytes);
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

fn write_idt_gate32_phys(
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

#[test]
fn page_fault_is_delivered_through_idt_and_cr2_is_set() {
    // 32-bit paging setup:
    // - PDE[0] -> PT
    // - PTE[0] -> code/IDT/handler page
    // - PTE[1] not present (fault target page at 0x1000)
    // - PTE[2] -> stack page
    let pd_base = 0x1000u64;
    let pt_base = 0x2000u64;
    let code_page = 0x3000u64;
    let stack_page = 0x4000u64;

    let mut phys = TestMemory::new(0x8000);

    const PTE_P: u32 = 1 << 0;
    const PTE_RW: u32 = 1 << 1;
    const PTE_US: u32 = 1 << 2;
    let flags = PTE_P | PTE_RW | PTE_US;

    // PDE[0] -> PT.
    phys.write_u32(pd_base, (pt_base as u32) | flags);
    // PTE[0] -> code page (linear 0x0000).
    phys.write_u32(pt_base, (code_page as u32) | flags);
    // PTE[2] -> stack page (linear 0x2000).
    phys.write_u32(pt_base + 2 * 4, (stack_page as u32) | flags);

    // Code at linear 0: mov eax, dword ptr [0x00001000]; hlt
    phys.write_bytes(code_page, &[0xA1, 0x00, 0x10, 0x00, 0x00, 0xF4]);

    let handler = 0x0400u32;
    phys.write_bytes(code_page + handler as u64, &[0xF4]); // handler: HLT

    let idt_base = 0x0800u64;
    let idt_phys = code_page + idt_base;
    write_idt_gate32_phys(&mut phys, idt_phys, 14, 0x08, handler, 0x8E);
    // Provide a #GP gate so unexpected delivery issues don't immediately triple fault.
    write_idt_gate32_phys(&mut phys, idt_phys, 13, 0x08, handler, 0x8E);

    let bus = PagingBus::new(phys);

    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.cpu.state.control.cr3 = pd_base;
    cpu.cpu.state.update_mode();

    cpu.cpu.state.tables.idtr.base = idt_base;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x3000);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.set_rip(0);

    let mut interp = Tier0Interpreter::new(1024);

    // Deliver #PF and jump into the handler.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.exit, None);
    assert_eq!(cpu.cpu.state.rip(), handler as u64);
    assert_eq!(cpu.cpu.state.control.cr2, 0x1000);

    // Handler halts.
    interp.exec_block(&mut cpu);
    assert_eq!(cpu.exit, None);
    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.rip(), handler as u64 + 1);
}

#[test]
fn fetch_memory_fault_becomes_cpu_exit() {
    let bus = FlatTestBus::new(0x1000);
    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.control.cr0 |= CR0_PE;
    cpu.cpu.state.update_mode();
    cpu.cpu.state.segments.cs.base = 0;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.base = 0;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.write_gpr32(gpr::RSP, 0x800);

    // RIP points outside FlatTestBus memory so instruction fetch fails.
    cpu.cpu.state.set_rip(0x2000);

    let mut interp = Tier0Interpreter::new(1024);
    interp.exec_block(&mut cpu);

    assert_eq!(cpu.exit, Some(CpuExit::MemoryFault));
}
