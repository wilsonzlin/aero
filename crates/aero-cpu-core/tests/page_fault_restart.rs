use aero_cpu_core::exec::{Interpreter, Tier0Interpreter, Vcpu};
use aero_cpu_core::mem::CpuBus as _;
use aero_cpu_core::state::{gpr, CpuMode, CR0_PE, CR0_PG};
use aero_cpu_core::PagingBus;
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

fn run_to_halt<B: aero_cpu_core::mem::CpuBus>(
    cpu: &mut Vcpu<B>,
    interp: &mut Tier0Interpreter,
    max_iters: u64,
) {
    for _ in 0..max_iters {
        if cpu.exit.is_some() {
            panic!("unexpected CPU exit: {:?}", cpu.exit);
        }
        if cpu.cpu.state.halted {
            return;
        }
        interp.exec_block(cpu);
    }
    panic!("program did not halt");
}

#[test]
fn protected_mode_page_fault_handler_can_map_page_and_restart_faulting_instruction() {
    // Physical memory layout (identity-mapped unless noted):
    // - 0x1000: IDT
    // - 0x2000: code + #PF handler
    // - 0x3000: page directory
    // - 0x4000: page table
    // - 0x5000: backing physical page containing test byte 0xAA
    // - 0x7000: scratch (handler stores saved EIP here)
    // - 0x8000: stack page (ESP starts at 0x9000 and faults push into this page)
    const IDT_BASE: u64 = 0x1000;
    const CODE_BASE: u64 = 0x2000;
    const HANDLER_BASE: u64 = 0x2100;
    const PD_BASE: u64 = 0x3000;
    const PT_BASE: u64 = 0x4000;
    const BACKING_PAGE: u64 = 0x5000;
    const SCRATCH_EIP: u32 = 0x7000;
    const STACK_TOP: u32 = 0x9000;
    const FAULT_ADDR: u32 = 0x6000;

    const PTE_P: u32 = 1 << 0;
    const PTE_RW: u32 = 1 << 1;
    let flags = PTE_P | PTE_RW;

    let mut phys = TestMemory::new(0x20000);

    // Page directory: PDE[0] -> PT.
    phys.write_u32(PD_BASE + 0 * 4, (PT_BASE as u32) | flags);

    // Identity-map the low pages we need (except the faulting page).
    for page_idx in 0u32..=9 {
        if page_idx == (FAULT_ADDR >> 12) {
            continue;
        }
        let pte_addr = PT_BASE + (page_idx as u64) * 4;
        phys.write_u32(pte_addr, page_idx * 0x1000 | flags);
    }

    // Place the backing byte at physical 0x5000; handler will map FAULT_ADDR -> BACKING_PAGE.
    phys.write_u8(BACKING_PAGE, 0xAA);

    // Main program:
    //   mov al, byte ptr [FAULT_ADDR]
    //   hlt
    let mut main_code = vec![0xA0];
    main_code.extend_from_slice(&FAULT_ADDR.to_le_bytes());
    main_code.push(0xF4);
    for (i, b) in main_code.iter().copied().enumerate() {
        phys.write_u8(CODE_BASE + i as u64, b);
    }

    let pte_slot = PT_BASE + ((FAULT_ADDR as u64 >> 12) * 4);
    let pte_value = (BACKING_PAGE as u32) | flags;

    // #PF handler (interrupt gate):
    //   mov eax, [esp+4]            ; saved EIP (faulting RIP)
    //   mov [SCRATCH_EIP], eax      ; store for assertions
    //   mov dword ptr [pte_slot], pte_value
    //   invlpg [FAULT_ADDR]
    //   add esp, 4                  ; discard #PF error code
    //   iretd
    let mut handler: Vec<u8> = vec![
        0x8B, 0x44, 0x24, 0x04, // mov eax, [esp+4]
        0xA3, // mov [moffs32], eax
    ];
    handler.extend_from_slice(&SCRATCH_EIP.to_le_bytes());
    handler.extend_from_slice(&[
        0xC7, 0x05, // mov dword ptr [disp32], imm32
    ]);
    handler.extend_from_slice(&(pte_slot as u32).to_le_bytes());
    handler.extend_from_slice(&pte_value.to_le_bytes());
    handler.extend_from_slice(&[0x0F, 0x01, 0x3D]); // invlpg [disp32]
    handler.extend_from_slice(&FAULT_ADDR.to_le_bytes());
    handler.extend_from_slice(&[
        0x83, 0xC4, 0x04, // add esp, 4
        0xCF, // iretd
    ]);
    for (i, b) in handler.iter().copied().enumerate() {
        phys.write_u8(HANDLER_BASE + i as u64, b);
    }

    // IDT[14] -> page fault handler (32-bit interrupt gate).
    write_idt_gate32(&mut phys, IDT_BASE, 14, 0x08, HANDLER_BASE as u32, 0x8E);

    let bus = PagingBus::new(phys);
    let mut cpu = Vcpu::new_with_mode(CpuMode::Protected, bus);
    cpu.cpu.state.control.cr0 = CR0_PE | CR0_PG;
    cpu.cpu.state.control.cr3 = PD_BASE;
    cpu.cpu.state.update_mode();
    cpu.cpu.state.tables.idtr.base = IDT_BASE;
    cpu.cpu.state.tables.idtr.limit = 0x7FF;
    cpu.cpu.state.segments.cs.selector = 0x08;
    cpu.cpu.state.segments.ss.selector = 0x10;
    cpu.cpu.state.segments.ds.selector = 0x10;
    cpu.cpu.state.write_gpr32(gpr::RSP, STACK_TOP);
    cpu.cpu.state.set_rflags(0x0002);
    cpu.cpu.state.set_rip(CODE_BASE);

    let mut interp = Tier0Interpreter::new(1024);
    run_to_halt(&mut cpu, &mut interp, 128);

    assert!(cpu.cpu.state.halted);
    assert_eq!(cpu.cpu.state.read_reg(Register::AL) as u8, 0xAA);
    assert_eq!(cpu.cpu.state.control.cr2, FAULT_ADDR as u64);
    assert_eq!(
        cpu.bus.read_u32(SCRATCH_EIP as u64).unwrap(),
        CODE_BASE as u32
    );
    assert_eq!(interp.assist.invlpg_log, vec![FAULT_ADDR as u64]);
}
