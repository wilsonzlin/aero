use aero_cpu_core::interp::tier0::exec::{step as tier0_step, StepExit};
use aero_cpu_core::interrupts::{self, PendingEventState};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{
    mask_bits, CpuMode, CpuState, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_SF, FLAG_ZF, RFLAGS_IF,
};
use aero_cpu_core::{AssistReason, Exception};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};
use std::collections::HashMap;

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        self.data[addr as usize..addr as usize + bytes.len()].copy_from_slice(bytes);
    }

    fn write_u8(&mut self, addr: u64, value: u8) -> Result<(), Exception> {
        let Some(slot) = self.data.get_mut(addr as usize) else {
            return Err(Exception::MemoryFault);
        };
        *slot = value;
        Ok(())
    }

    fn write_u16(&mut self, addr: u64, value: u16) -> Result<(), Exception> {
        let bytes = value.to_le_bytes();
        self.write_u8(addr, bytes[0])?;
        self.write_u8(addr + 1, bytes[1])?;
        Ok(())
    }

    fn write_u32(&mut self, addr: u64, value: u32) -> Result<(), Exception> {
        let bytes = value.to_le_bytes();
        for (i, b) in bytes.iter().enumerate() {
            self.write_u8(addr + i as u64, *b)?;
        }
        Ok(())
    }
}

#[derive(Default)]
struct TestPorts {
    debugcon: Vec<u8>,
    in_u8: HashMap<u16, u8>,
    out_u8: Vec<(u16, u8)>,
}

impl TestPorts {
    fn in_u8(&mut self, port: u16) -> u8 {
        *self.in_u8.get(&port).unwrap_or(&0)
    }

    fn out_u8(&mut self, port: u16, value: u8) {
        self.out_u8.push((port, value));
        if port == 0xE9 {
            self.debugcon.push(value);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    Continue,
    Halted,
}

struct Machine {
    cpu: CpuState,
    pending: PendingEventState,
    mem: TestMem,
    ports: TestPorts,
    msr: HashMap<u32, u64>,
}

impl Machine {
    fn new(cpu: CpuState, mem: TestMem, ports: TestPorts) -> Self {
        Self {
            cpu,
            pending: PendingEventState::default(),
            mem,
            ports,
            msr: HashMap::new(),
        }
    }

    fn run(&mut self, max_insts: u64) -> Result<(), Exception> {
        for _ in 0..max_insts {
            match self.step()? {
                StepOutcome::Continue => {}
                StepOutcome::Halted => return Ok(()),
            }
        }
        Err(Exception::Unimplemented("execution limit reached"))
    }

    fn step(&mut self) -> Result<StepOutcome, Exception> {
        if self.cpu.halted {
            return Ok(StepOutcome::Halted);
        }
        let mut bus = Bus {
            mem: &mut self.mem,
            ports: &mut self.ports,
        };
        match tier0_step(&mut self.cpu, &mut bus)? {
            StepExit::Continue | StepExit::ContinueInhibitInterrupts | StepExit::Branch => {
                self.pending.retire_instruction();
                Ok(StepOutcome::Continue)
            }
            StepExit::Halted => Ok(StepOutcome::Halted),
            StepExit::BiosInterrupt(vector) => {
                self.handle_bios_interrupt(vector)?;
                self.pending.retire_instruction();
                Ok(StepOutcome::Continue)
            }
            StepExit::Assist {
                reason,
                decoded,
                addr_size_override,
            } => {
                self.handle_assist(reason, &decoded, addr_size_override)?;
                self.pending.retire_instruction();
                Ok(StepOutcome::Continue)
            }
        }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) -> Result<(), Exception> {
        // Tier-0 can surface certain real-mode `INT n` calls as a BIOS hypercall:
        // after the guest transfers control to a ROM stub, the stub executes `HLT`
        // and Tier-0 exits with `StepExit::BiosInterrupt(n)`.
        //
        // For the `aero-tests` harness we model a tiny subset of BIOS services
        // needed by tests. In particular, we treat `INT 10h / AH=0Eh` as a
        // "debugcon" write to port 0xE9.
        match vector {
            0x10 => {
                let ah = self.cpu.read_reg(Register::AH) as u8;
                if ah != 0x0E {
                    return Err(Exception::Unimplemented("BIOS INT10h function"));
                }
                let al = self.cpu.read_reg(Register::AL) as u8;
                self.ports.out_u8(0xE9, al);
            }
            _ => return Err(Exception::Unimplemented("BIOS interrupt vector")),
        }

        // Return to the interrupted context (IRET). The Tier-0 BIOS hypercall mechanism
        // surfaces the interrupt vector, but the original interrupt frame remains on the
        // guest stack. Use the canonical IRET implementation so the pending-frame
        // bookkeeping stays consistent with INT delivery.
        let mut bus = Bus {
            mem: &mut self.mem,
            ports: &mut self.ports,
        };
        interrupts::iret(&mut self.cpu, &mut bus, &mut self.pending)
            .unwrap_or_else(|exit| panic!("BIOS hypercall IRET failed: {exit:?}"));
        self.cpu.clear_pending_bios_int();

        Ok(())
    }

    fn handle_assist(
        &mut self,
        _reason: AssistReason,
        decoded: &DecodedInst,
        _addr_size_override: bool,
    ) -> Result<(), Exception> {
        let mut bus = Bus {
            mem: &mut self.mem,
            ports: &mut self.ports,
        };

        let ip = self.cpu.rip();
        let next_ip = ip.wrapping_add(decoded.len as u64) & mask_bits(self.cpu.bitness());
        let instr = &decoded.instr;

        match instr.mnemonic() {
            Mnemonic::In => {
                let dst = instr.op0_register();
                let bits = reg_bits(dst)?;
                let port = match instr.op_kind(1) {
                    OpKind::Immediate8 => instr.immediate8() as u16,
                    OpKind::Register => self.cpu.read_reg(instr.op1_register()) as u16,
                    _ => return Err(Exception::InvalidOpcode),
                };
                let v = bus.io_read(port, bits)?;
                self.cpu.write_reg(dst, v);
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Out => {
                let src = instr.op1_register();
                let bits = reg_bits(src)?;
                let port = match instr.op_kind(0) {
                    OpKind::Immediate8 => instr.immediate8() as u16,
                    OpKind::Register => self.cpu.read_reg(instr.op0_register()) as u16,
                    _ => return Err(Exception::InvalidOpcode),
                };
                let v = self.cpu.read_reg(src);
                bus.io_write(port, bits, v)?;
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Cli => {
                self.cpu.set_flag(RFLAGS_IF, false);
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Sti => {
                self.cpu.set_flag(RFLAGS_IF, true);
                self.pending.inhibit_interrupts_for_one_instruction();
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Cpuid => {
                let leaf = self.cpu.read_reg(Register::EAX) as u32;
                let sub = self.cpu.read_reg(Register::ECX) as u32;
                let (eax, ebx, ecx, edx) = cpuid_leaf(leaf, sub);
                self.cpu.write_reg(Register::EAX, eax as u64);
                self.cpu.write_reg(Register::EBX, ebx as u64);
                self.cpu.write_reg(Register::ECX, ecx as u64);
                self.cpu.write_reg(Register::EDX, edx as u64);
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Rdtsc => {
                let tsc = self.cpu.msr.tsc;
                self.cpu.write_reg(Register::EAX, (tsc as u32) as u64);
                self.cpu
                    .write_reg(Register::EDX, ((tsc >> 32) as u32) as u64);
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Rdmsr => {
                let idx = self.cpu.read_reg(Register::ECX) as u32;
                let val = match idx {
                    0x10 => self.cpu.msr.tsc,
                    _ => *self.msr.get(&idx).unwrap_or(&0),
                };
                self.cpu.write_reg(Register::EAX, (val as u32) as u64);
                self.cpu
                    .write_reg(Register::EDX, ((val >> 32) as u32) as u64);
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Wrmsr => {
                let idx = self.cpu.read_reg(Register::ECX) as u32;
                let edx = self.cpu.read_reg(Register::EDX) & 0xFFFF_FFFF;
                let eax = self.cpu.read_reg(Register::EAX) & 0xFFFF_FFFF;
                let val = (edx << 32) | eax;
                if idx == 0x10 {
                    self.cpu.msr.tsc = val;
                } else {
                    self.msr.insert(idx, val);
                }
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Lgdt | Mnemonic::Lidt => {
                if instr.op_kind(0) != OpKind::Memory {
                    return Err(Exception::InvalidOpcode);
                }
                let addr = calc_ea(&self.cpu, instr, next_ip, true)?;
                let limit = bus.read_u16(addr)?;
                let base = bus.read_u32(addr + 2)? as u64;
                match instr.mnemonic() {
                    Mnemonic::Lgdt => {
                        self.cpu.tables.gdtr.limit = limit;
                        self.cpu.tables.gdtr.base = base;
                    }
                    Mnemonic::Lidt => {
                        self.cpu.tables.idtr.limit = limit;
                        self.cpu.tables.idtr.base = base;
                    }
                    _ => {}
                }
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Ltr => {
                let sel = match instr.op_kind(0) {
                    OpKind::Register => self.cpu.read_reg(instr.op0_register()) as u16,
                    OpKind::Memory => {
                        let addr = calc_ea(&self.cpu, instr, next_ip, true)?;
                        bus.read_u16(addr)?
                    }
                    _ => return Err(Exception::InvalidOpcode),
                };
                self.cpu.tables.tr.selector = sel;
                self.cpu.set_rip(next_ip);
                Ok(())
            }
            Mnemonic::Mov => {
                if instr.op_kind(0) == OpKind::Register
                    && instr.op_kind(1) == OpKind::Register
                    && (is_ctrl_or_debug_reg(instr.op0_register())
                        || is_ctrl_or_debug_reg(instr.op1_register()))
                {
                    let dst = instr.op0_register();
                    let src = instr.op1_register();
                    if is_ctrl_or_debug_reg(dst) {
                        let v = self.cpu.read_reg(src);
                        write_special_reg(&mut self.cpu, dst, v);
                    } else {
                        let v = read_special_reg(&self.cpu, src)?;
                        self.cpu.write_reg(dst, v);
                    }
                    self.cpu.update_mode();
                    self.cpu.set_rip(next_ip);
                    Ok(())
                } else {
                    Err(Exception::Unimplemented("assisted MOV form"))
                }
            }
            Mnemonic::Call
                if matches!(instr.op_kind(0), OpKind::FarBranch16 | OpKind::FarBranch32) =>
            {
                let selector = instr.far_branch_selector();
                let target = match instr.op_kind(0) {
                    OpKind::FarBranch16 => instr.far_branch16() as u64,
                    OpKind::FarBranch32 => instr.far_branch32() as u64,
                    _ => unreachable!(),
                };
                // push CS then return IP (top of stack).
                let cs = self.cpu.read_reg(Register::CS);
                push(&mut self.cpu, &mut bus, cs, 2)?;
                push(&mut self.cpu, &mut bus, next_ip, 2)?;
                self.cpu.write_reg(Register::CS, selector as u64);
                self.cpu.set_rip(target & mask_bits(self.cpu.bitness()));
                Ok(())
            }
            Mnemonic::Retf => {
                let pop_imm = if instr.op_count() == 1 && instr.op_kind(0) == OpKind::Immediate16 {
                    instr.immediate16() as u32
                } else {
                    0
                };
                let ip = pop(&mut self.cpu, &mut bus, 2)? & 0xFFFF;
                let cs = pop(&mut self.cpu, &mut bus, 2)? as u16;
                let sp = self.cpu.stack_ptr().wrapping_add(pop_imm as u64)
                    & mask_bits(self.cpu.stack_ptr_bits());
                self.cpu.set_stack_ptr(sp);
                self.cpu.write_reg(Register::CS, cs as u64);
                self.cpu.set_rip(ip);
                Ok(())
            }
            Mnemonic::Int | Mnemonic::Int3 => {
                let vector = if instr.mnemonic() == Mnemonic::Int3 {
                    3u8
                } else {
                    instr.immediate8()
                };
                if self.cpu.bitness() != 16 {
                    return Err(Exception::Unimplemented("INT outside real mode"));
                }

                self.pending.raise_software_interrupt(vector, next_ip);
                interrupts::deliver_pending_event(&mut self.cpu, &mut bus, &mut self.pending)
                    .unwrap_or_else(|exit| panic!("interrupt delivery failed: {exit:?}"));
                Ok(())
            }
            Mnemonic::Iret => {
                if self.cpu.bitness() != 16 {
                    return Err(Exception::Unimplemented("IRET outside real mode"));
                }
                interrupts::iret(&mut self.cpu, &mut bus, &mut self.pending)
                    .unwrap_or_else(|exit| panic!("IRET failed: {exit:?}"));
                // Any real-mode IRET cancels the pending BIOS interrupt marker (the hypercall
                // path consumes the marker when the ROM stub executes `HLT`).
                self.cpu.clear_pending_bios_int();
                Ok(())
            }
            _ => Err(Exception::Unimplemented("assist handler missing mnemonic")),
        }
    }
}

struct Bus<'a> {
    mem: &'a mut TestMem,
    ports: &'a mut TestPorts,
}

impl<'a> Bus<'a> {
    fn io_read(&mut self, port: u16, bits: u32) -> Result<u64, Exception> {
        match bits {
            8 => Ok(self.ports.in_u8(port) as u64),
            16 => Ok(self.ports.in_u8(port) as u64),
            32 => Ok(self.ports.in_u8(port) as u64),
            _ => Err(Exception::InvalidOpcode),
        }
    }

    fn io_write(&mut self, port: u16, bits: u32, val: u64) -> Result<(), Exception> {
        match bits {
            8 => {
                self.ports.out_u8(port, val as u8);
                Ok(())
            }
            16 => {
                self.ports.out_u8(port, val as u8);
                Ok(())
            }
            32 => {
                self.ports.out_u8(port, val as u8);
                Ok(())
            }
            _ => Err(Exception::InvalidOpcode),
        }
    }
}

impl CpuBus for Bus<'_> {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem
            .data
            .get(vaddr as usize)
            .copied()
            .ok_or(Exception::MemoryFault)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let lo = self.read_u8(vaddr)? as u16;
        let hi = self.read_u8(vaddr + 1)? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut v = 0u32;
        for i in 0..4 {
            v |= (self.read_u8(vaddr + i)? as u32) << (i * 8);
        }
        Ok(v)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut v = 0u64;
        for i in 0..8 {
            v |= (self.read_u8(vaddr + i)? as u64) << (i * 8);
        }
        Ok(v)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut v = 0u128;
        for i in 0..16 {
            v |= (self.read_u8(vaddr + i)? as u128) << (i * 8);
        }
        Ok(v)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.mem.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.mem.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.mem.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        for i in 0..8 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        for i in 0..16 {
            self.write_u8(vaddr + i, (val >> (i * 8)) as u8)?;
        }
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for (i, slot) in buf.iter_mut().enumerate().take(len) {
            *slot = self.read_u8(vaddr.wrapping_add(i as u64))?;
        }
        Ok(buf)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        match size {
            1 => Ok(self.ports.in_u8(port) as u64),
            _ => Ok(0),
        }
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        match size {
            1 => {
                self.ports.out_u8(port, val as u8);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn reg_bits(reg: Register) -> Result<u32, Exception> {
    use Register::*;
    let bits = match reg {
        AL | CL | DL | BL | AH | CH | DH | BH | SPL | BPL | SIL | DIL | R8L | R9L | R10L | R11L
        | R12L | R13L | R14L | R15L => 8,
        AX | CX | DX | BX | SP | BP | SI | DI | R8W | R9W | R10W | R11W | R12W | R13W | R14W
        | R15W => 16,
        EAX | ECX | EDX | EBX | ESP | EBP | ESI | EDI | R8D | R9D | R10D | R11D | R12D | R13D
        | R14D | R15D => 32,
        RAX | RCX | RDX | RBX | RSP | RBP | RSI | RDI | R8 | R9 | R10 | R11 | R12 | R13 | R14
        | R15 => 64,
        ES | CS | SS | DS | FS | GS => 16,
        _ => return Err(Exception::InvalidOpcode),
    };
    Ok(bits)
}

fn calc_ea(
    cpu: &CpuState,
    instr: &Instruction,
    next_ip: u64,
    include_seg: bool,
) -> Result<u64, Exception> {
    let base = instr.memory_base();
    let index = instr.memory_index();
    let scale = instr.memory_index_scale() as u64;
    let mut disp = instr.memory_displacement64() as i128;
    if base == Register::RIP {
        disp -= next_ip as i128;
    }

    let addr_bits = if base == Register::RIP {
        64
    } else if base != Register::None {
        reg_bits(base)?
    } else if index != Register::None {
        reg_bits(index)?
    } else {
        match instr.memory_displ_size() {
            2 => 16,
            4 => 32,
            8 => 64,
            _ => cpu.bitness(),
        }
    };

    let mut offset: i128 = disp;
    if base != Register::None {
        let base_val = if base == Register::RIP {
            next_ip
        } else {
            cpu.read_reg(base)
        };
        offset += (base_val & mask_bits(addr_bits)) as i128;
    }
    if index != Register::None {
        let idx_val = cpu.read_reg(index) & mask_bits(addr_bits);
        offset += (idx_val as i128) * (scale as i128);
    }

    let addr = (offset as u64) & mask_bits(addr_bits);
    if include_seg {
        Ok(cpu.apply_a20(cpu.seg_base_reg(instr.memory_segment()).wrapping_add(addr)))
    } else {
        Ok(addr)
    }
}

fn is_ctrl_or_debug_reg(reg: Register) -> bool {
    matches!(
        reg,
        Register::CR0
            | Register::CR2
            | Register::CR3
            | Register::CR4
            | Register::CR8
            | Register::DR0
            | Register::DR1
            | Register::DR2
            | Register::DR3
            | Register::DR6
            | Register::DR7
    )
}

fn write_special_reg(cpu: &mut CpuState, reg: Register, value: u64) {
    match reg {
        Register::CR0 => cpu.control.cr0 = value,
        Register::CR2 => cpu.control.cr2 = value,
        Register::CR3 => cpu.control.cr3 = value,
        Register::CR4 => cpu.control.cr4 = value,
        Register::CR8 => cpu.control.cr8 = value,
        Register::DR0 => cpu.debug.dr[0] = value,
        Register::DR1 => cpu.debug.dr[1] = value,
        Register::DR2 => cpu.debug.dr[2] = value,
        Register::DR3 => cpu.debug.dr[3] = value,
        Register::DR6 => cpu.debug.dr6 = value,
        Register::DR7 => cpu.debug.dr7 = value,
        _ => {}
    }
}

fn read_special_reg(cpu: &CpuState, reg: Register) -> Result<u64, Exception> {
    Ok(match reg {
        Register::CR0 => cpu.control.cr0,
        Register::CR2 => cpu.control.cr2,
        Register::CR3 => cpu.control.cr3,
        Register::CR4 => cpu.control.cr4,
        Register::CR8 => cpu.control.cr8,
        Register::DR0 => cpu.debug.dr[0],
        Register::DR1 => cpu.debug.dr[1],
        Register::DR2 => cpu.debug.dr[2],
        Register::DR3 => cpu.debug.dr[3],
        Register::DR6 => cpu.debug.dr6,
        Register::DR7 => cpu.debug.dr7,
        _ => return Err(Exception::InvalidOpcode),
    })
}

fn push<B: CpuBus>(cpu: &mut CpuState, bus: &mut B, val: u64, size: u32) -> Result<(), Exception> {
    let sp_bits = cpu.stack_ptr_bits();
    let mut sp = cpu.stack_ptr();
    sp = sp.wrapping_sub(size as u64) & mask_bits(sp_bits);
    cpu.set_stack_ptr(sp);
    let addr = cpu.apply_a20(cpu.seg_base_reg(Register::SS).wrapping_add(sp));
    match size {
        2 => bus.write_u16(addr, val as u16),
        4 => bus.write_u32(addr, val as u32),
        8 => bus.write_u64(addr, val),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn pop<B: CpuBus>(cpu: &mut CpuState, bus: &mut B, size: u32) -> Result<u64, Exception> {
    let sp_bits = cpu.stack_ptr_bits();
    let sp = cpu.stack_ptr();
    let addr = cpu.apply_a20(cpu.seg_base_reg(Register::SS).wrapping_add(sp));
    let v = match size {
        2 => bus.read_u16(addr)? as u64,
        4 => bus.read_u32(addr)? as u64,
        8 => bus.read_u64(addr)?,
        _ => return Err(Exception::InvalidOpcode),
    };
    let new_sp = sp.wrapping_add(size as u64) & mask_bits(sp_bits);
    cpu.set_stack_ptr(new_sp);
    Ok(v)
}

fn cpuid_leaf(leaf: u32, _subleaf: u32) -> (u32, u32, u32, u32) {
    match leaf {
        0 => {
            let vendor = *b"GenuineIntel";
            let ebx = u32::from_le_bytes([vendor[0], vendor[1], vendor[2], vendor[3]]);
            let edx = u32::from_le_bytes([vendor[4], vendor[5], vendor[6], vendor[7]]);
            let ecx = u32::from_le_bytes([vendor[8], vendor[9], vendor[10], vendor[11]]);
            (1, ebx, ecx, edx)
        }
        _ => (0, 0, 0, 0),
    }
}

fn run_test(
    mode: CpuMode,
    code: &[u8],
    init: impl FnOnce(&mut CpuState, &mut TestMem, &mut TestPorts),
) -> Machine {
    let mut mem = TestMem::new(1024 * 1024);
    mem.load(0, code);
    let mut cpu = CpuState::new(mode);
    cpu.write_reg(Register::CS, 0);
    cpu.write_reg(Register::DS, 0);
    cpu.write_reg(Register::ES, 0);
    cpu.write_reg(Register::SS, 0);
    cpu.set_rip(0);
    let mut ports = TestPorts::default();
    init(&mut cpu, &mut mem, &mut ports);

    let mut machine = Machine::new(cpu, mem, ports);
    machine.run(10_000).unwrap();
    machine
}

fn make_machine(
    mode: CpuMode,
    code: &[u8],
    init: impl FnOnce(&mut CpuState, &mut TestMem, &mut TestPorts),
) -> Machine {
    let mut mem = TestMem::new(1024 * 1024);
    mem.load(0, code);
    let mut cpu = CpuState::new(mode);
    cpu.write_reg(Register::CS, 0);
    cpu.write_reg(Register::DS, 0);
    cpu.write_reg(Register::ES, 0);
    cpu.write_reg(Register::SS, 0);
    cpu.set_rip(0);
    let mut ports = TestPorts::default();
    init(&mut cpu, &mut mem, &mut ports);
    Machine::new(cpu, mem, ports)
}

#[test]
fn boot_sector_hello_via_int10() {
    // A tiny boot sector that prints "Hello" using INT 10h.
    // We install an IVT handler for INT 10h that outputs AL to port 0xE9 and IRET's.
    let mut mem = TestMem::new(1024 * 1024);

    let boot_addr = 0x7C00u64;
    let msg_off = 0x7C00u16 + 0x1B; // after code below
    let boot = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0x8E,
        0xC0, // mov es,ax
        0x8E,
        0xD0, // mov ss,ax
        0xBC,
        0x00,
        0x7C, // mov sp,0x7c00
        0xBE,
        (msg_off & 0xFF) as u8,
        (msg_off >> 8) as u8, // mov si,msg
        0xFC,                 // cld
        0xAC,                 // lodsb
        0x0A,
        0xC0, // or al,al
        0x74,
        0x06, // jz done
        0xB4,
        0x0E, // mov ah,0x0e
        0xCD,
        0x10, // int 0x10
        0xEB,
        0xF5, // jmp loop (back 11 bytes)
        0xF4, // done: hlt
        b'H',
        b'e',
        b'l',
        b'l',
        b'o',
        0,
    ];
    mem.load(boot_addr, &boot);

    // BIOS handler at F000:0100 => physical 0xF0100.
    let bios_seg = 0xF000u16;
    let bios_off = 0x0100u16;
    let bios_addr = ((bios_seg as u64) << 4) + bios_off as u64;
    let bios = [
        0x50, // push ax
        0xE6, 0xE9, // out 0xE9, al
        0x58, // pop ax
        0xCF, // iret
    ];
    mem.load(bios_addr, &bios);

    // IVT[0x10] = bios handler.
    let ivt = 0x10u64 * 4;
    mem.write_u16(ivt, bios_off).unwrap();
    mem.write_u16(ivt + 2, bios_seg).unwrap();

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.write_reg(Register::CS, 0);
    cpu.set_rip(boot_addr);

    let ports = TestPorts::default();
    let mut machine = Machine::new(cpu, mem, ports);
    machine.run(10_000).unwrap();

    assert_eq!(machine.step().unwrap(), StepOutcome::Halted);
    assert_eq!(
        std::str::from_utf8(&machine.ports.debugcon).unwrap(),
        "Hello"
    );
}

#[test]
fn boot_sector_hello_via_bios_hypercall_int10() {
    // Same "Hello" boot sector as `boot_sector_hello_via_int10`, but routes INT 10h
    // through the Tier-0 BIOS hypercall mechanism:
    // - IVT[0x10] points at a ROM stub that executes `HLT`
    // - Tier-0 exits with `StepExit::BiosInterrupt(0x10)`
    // - The harness emulates the BIOS service (AH=0Eh teletype) and IRET's
    let mut mem = TestMem::new(1024 * 1024);

    let boot_addr = 0x7C00u64;
    let msg_off = 0x7C00u16 + 0x1B; // after code below
    let boot = [
        0x31,
        0xC0, // xor ax,ax
        0x8E,
        0xD8, // mov ds,ax
        0x8E,
        0xC0, // mov es,ax
        0x8E,
        0xD0, // mov ss,ax
        0xBC,
        0x00,
        0x7C, // mov sp,0x7c00
        0xBE,
        (msg_off & 0xFF) as u8,
        (msg_off >> 8) as u8, // mov si,msg
        0xFC,                 // cld
        0xAC,                 // lodsb
        0x0A,
        0xC0, // or al,al
        0x74,
        0x06, // jz done
        0xB4,
        0x0E, // mov ah,0x0e
        0xCD,
        0x10, // int 0x10
        0xEB,
        0xF5, // jmp loop (back 11 bytes)
        0xF4, // done: hlt
        b'H',
        b'e',
        b'l',
        b'l',
        b'o',
        0,
    ];
    mem.load(boot_addr, &boot);

    // BIOS ROM stub at F000:0100 (`HLT; IRET`).
    let bios_seg = 0xF000u16;
    let bios_off = 0x0100u16;
    let bios_addr = ((bios_seg as u64) << 4) + bios_off as u64;
    mem.load(bios_addr, &[0xF4, 0xCF]); // hlt; iret

    // IVT[0x10] = bios stub.
    let ivt = 0x10u64 * 4;
    mem.write_u16(ivt, bios_off).unwrap();
    mem.write_u16(ivt + 2, bios_seg).unwrap();

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.write_reg(Register::CS, 0);
    cpu.set_rip(boot_addr);

    let ports = TestPorts::default();
    let mut machine = Machine::new(cpu, mem, ports);
    machine.run(20_000).unwrap();

    assert_eq!(machine.step().unwrap(), StepOutcome::Halted);
    assert_eq!(
        std::str::from_utf8(&machine.ports.debugcon).unwrap(),
        "Hello"
    );
}

#[test]
fn mov_lea_xchg() {
    // mov ax,0x5678; mov bx,0x1234; lea di,[bx+si+0x10]; xchg ax,bx
    let code = [
        0xB8, 0x78, 0x56, // mov ax,0x5678
        0xBB, 0x34, 0x12, // mov bx,0x1234
        0xBE, 0x04, 0x00, // mov si,0x0004
        0x8D, 0x78, 0x10, // lea di,[bx+si+0x10]
        0x93, // xchg ax,bx
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });

    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0x1234);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::BX), 0x5678);
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::DI),
        0x1234 + 0x0004 + 0x10
    );
}

#[test]
fn movzx_movsx() {
    // mov al,0x80; movsx ecx,al; movzx edx,al
    let code = [
        0xB0, 0x80, // mov al,0x80
        0x0F, 0xBE, 0xC8, // movsx ecx,al
        0x0F, 0xB6, 0xD0, // movzx edx,al
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x8000);
    });

    assert_eq!(machine.cpu.read_reg(iced_x86::Register::ECX), 0xFFFF_FF80);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX), 0x80);
}

#[test]
fn push_pop_and_pusha_popa() {
    let code = [
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0x50, // push ax
        0xB8, 0x00, 0x00, // mov ax,0
        0x5B, // pop bx
        0x60, // pusha
        0xB8, 0x00, 0x00, // mov ax,0
        0x61, // popa
        0xF4, // hlt
    ];

    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x0100);
        cpu.write_reg(iced_x86::Register::CX, 0x2222);
        cpu.write_reg(iced_x86::Register::DX, 0x3333);
        cpu.write_reg(iced_x86::Register::BX, 0x4444);
        cpu.write_reg(iced_x86::Register::BP, 0x5555);
        cpu.write_reg(iced_x86::Register::SI, 0x6666);
        cpu.write_reg(iced_x86::Register::DI, 0x7777);
    });

    assert_eq!(machine.cpu.read_reg(iced_x86::Register::BX), 0x1234);
    // AX was 0 when PUSHA executed, so POPA restores it back to 0.
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0x0000);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::SP), 0x0100);
}

#[test]
fn add_flags() {
    let code = [0x00, 0xD8, 0xF4]; // add al,bl; hlt
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0xFF);
        cpu.write_reg(iced_x86::Register::BL, 0x01);
    });

    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0x00);
    assert!(machine.cpu.get_flag(FLAG_CF));
    assert!(machine.cpu.get_flag(FLAG_ZF));
    assert!(machine.cpu.get_flag(FLAG_AF));
    assert!(!machine.cpu.get_flag(FLAG_OF));
    assert!(!machine.cpu.get_flag(FLAG_SF));
}

#[test]
fn adc_sbb_and_cmp_flags() {
    // adc al,bl; cmp al,bl; sbb al,bl
    let adc = [0x10, 0xD8, 0xF4]; // adc al,bl
    let machine = run_test(CpuMode::Real, &adc, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 1);
        cpu.write_reg(iced_x86::Register::BL, 1);
        cpu.set_flag(FLAG_CF, true);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 3);
    assert!(!machine.cpu.get_flag(FLAG_CF));

    let cmp = [0x38, 0xD8, 0xF4]; // cmp al,bl
    let machine = run_test(CpuMode::Real, &cmp, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 1);
        cpu.write_reg(iced_x86::Register::BL, 2);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 1);
    assert!(machine.cpu.get_flag(FLAG_CF));

    let sbb = [0x18, 0xD8, 0xF4]; // sbb al,bl
    let machine = run_test(CpuMode::Real, &sbb, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0);
        cpu.write_reg(iced_x86::Register::BL, 0);
        cpu.set_flag(FLAG_CF, true);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0xFF);
    assert!(machine.cpu.get_flag(FLAG_CF));
}

#[test]
fn inc_dec_preserve_cf() {
    let code = [
        0xFE, 0xC0, // inc al
        0xFE, 0xC8, // dec al
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0x7F);
        cpu.set_flag(FLAG_CF, true);
    });

    assert!(machine.cpu.get_flag(FLAG_CF));
    assert!(machine.cpu.get_flag(FLAG_OF)); // inc 0x7F => 0x80 overflows
}

#[test]
fn mul_div_idiv_and_divide_error() {
    let mul = [0xF6, 0xE3, 0xF4]; // mul bl
    let machine = run_test(CpuMode::Real, &mul, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0x10);
        cpu.write_reg(iced_x86::Register::BL, 0x10);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0x0100);
    assert!(machine.cpu.get_flag(FLAG_CF));
    assert!(machine.cpu.get_flag(FLAG_OF));

    let div = [0xF6, 0xF3, 0xF4]; // div bl
    let machine = run_test(CpuMode::Real, &div, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AX, 0x0100);
        cpu.write_reg(iced_x86::Register::BL, 0x10);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0x10);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AH), 0x00);

    let idiv = [0xF6, 0xFB, 0xF4]; // idiv bl
    let machine = run_test(CpuMode::Real, &idiv, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AX, 0xFFF0); // -16
        cpu.write_reg(iced_x86::Register::BL, 0xF0); // -16
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 1);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AH), 0);

    // Divide by zero -> #DE
    let mut machine = make_machine(CpuMode::Real, &[0xF6, 0xF3, 0xF4], |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AX, 1);
        cpu.write_reg(iced_x86::Register::BL, 0);
    });
    let err = machine.run(10).unwrap_err();
    assert_eq!(err, Exception::DivideError);
}

#[test]
fn logic_and_shift_rotate() {
    let and_ = [0x20, 0xD8, 0xF4]; // and al,bl
    let machine = run_test(CpuMode::Real, &and_, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0xF0);
        cpu.write_reg(iced_x86::Register::BL, 0x0F);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0);
    assert!(machine.cpu.get_flag(FLAG_ZF));
    assert!(!machine.cpu.get_flag(FLAG_CF));
    assert!(!machine.cpu.get_flag(FLAG_OF));

    let neg = [0xF6, 0xD8, 0xF4]; // neg al
    let machine = run_test(CpuMode::Real, &neg, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 1);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0xFF);
    assert!(machine.cpu.get_flag(FLAG_CF));

    let shl = [0xD0, 0xE0, 0xF4]; // shl al,1
    let machine = run_test(CpuMode::Real, &shl, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0x81);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0x02);
    assert!(machine.cpu.get_flag(FLAG_CF));
    assert!(machine.cpu.get_flag(FLAG_OF));

    let ror = [0xD0, 0xC8, 0xF4]; // ror al,1
    let machine = run_test(CpuMode::Real, &ror, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        cpu.write_reg(iced_x86::Register::AL, 0x81);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0xC0);
    assert!(machine.cpu.get_flag(FLAG_CF));
}

#[test]
fn cmovcc_and_setcc() {
    // cmp eax,ebx sets CF=1; cmovb and setb should observe it.
    let code = [
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xBB, 0x02, 0x00, 0x00, 0x00, // mov ebx,2
        0xB9, 0x11, 0x11, 0x11, 0x11, // mov ecx,0x11111111
        0xBA, 0x22, 0x22, 0x22, 0x22, // mov edx,0x22222222
        0x39, 0xD8, // cmp eax,ebx
        0x0F, 0x42, 0xCA, // cmovb ecx,edx
        0x0F, 0x92, 0xC0, // setb al
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::ECX), 0x2222_2222);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 1);
    assert!(machine.cpu.get_flag(FLAG_CF));
}

#[test]
fn pushf_popf_and_clc_stc_cmc() {
    // stc; pushf; clc; popf (restores CF=1); cmc (toggles to 0)
    let code = [0xF9, 0x9C, 0xF8, 0x9D, 0xF5, 0xF4];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x0200);
    });
    assert!(!machine.cpu.get_flag(FLAG_CF));
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::SP), 0x0200);
}

#[test]
fn xadd_and_bswap() {
    // xadd al,bl; bswap ecx
    let code = [
        0xB0, 0x01, // mov al,1
        0xB3, 0x02, // mov bl,2
        0x0F, 0xC0, 0xD8, // xadd al,bl
        0xB9, 0x78, 0x56, 0x34, 0x12, // mov ecx,0x12345678
        0x0F, 0xC9, // bswap ecx
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 3);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::BL), 1);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::ECX), 0x7856_3412);
}

#[test]
fn bit_ops_bt_bts_btr_btc() {
    let code = [
        0xB8, 0x00, 0x00, // mov ax,0
        0x0F, 0xBA, 0xE8, 0x02, // bts ax,2
        0x0F, 0xBA, 0xE8, 0x02, // bts ax,2
        0x0F, 0xBA, 0xF0, 0x02, // btr ax,2
        0x0F, 0xBA, 0xF8, 0x01, // btc ax,1
        0x0F, 0xBA, 0xF8, 0x01, // btc ax,1
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0);
    assert!(machine.cpu.get_flag(FLAG_CF));
}

#[test]
fn bsf_and_bsr() {
    let code = [
        0xB8, 0x10, 0x00, 0x00, 0x00, // mov eax,0x10
        0x0F, 0xBC, 0xC8, // bsf ecx,eax
        0x89, 0xCB, // mov ebx,ecx
        0x0F, 0xBD, 0xD0, // bsr edx,eax
        0x31, 0xC0, // xor eax,eax
        0xB9, 0x78, 0x56, 0x34, 0x12, // mov ecx,0x12345678
        0x0F, 0xBC, 0xC8, // bsf ecx,eax (src==0)
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EBX), 4);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX), 4);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::ECX), 0x1234_5678);
    assert!(machine.cpu.get_flag(FLAG_ZF));
}

#[test]
fn rcl_rcr_shld_shrd() {
    let code = [
        0xF9, // stc
        0xB0, 0x80, // mov al,0x80
        0xD0, 0xD0, // rcl al,1
        0xD0, 0xD8, // rcr al,1
        0x89, 0xC6, // mov si,ax
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0xBB, 0xCD, 0xAB, // mov bx,0xABCD
        0x0F, 0xA4, 0xD8, 0x04, // shld ax,bx,4
        0x89, 0xC1, // mov cx,ax
        0xBA, 0x34, 0x12, // mov dx,0x1234
        0x0F, 0xAC, 0xDA, 0x04, // shrd dx,bx,4
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::SI), 0x80);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::CX), 0x234A);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::DX), 0xD123);
}

#[test]
fn jecxz() {
    let code = [
        0xB9, 0x00, 0x00, 0x00, 0x00, // mov ecx,0
        0xE3, 0x05, // jecxz +5
        0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0xF4,
    ];
    let machine = run_test(CpuMode::Protected, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX), 0);
}

#[test]
fn cbw_cwd_cwde_cdq_cdqe_cqo() {
    let real = [0xB0, 0x80, 0x98, 0x99, 0xF4]; // mov al,0x80; cbw; cwd; hlt
    let machine = run_test(CpuMode::Real, &real, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0xFF80);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::DX), 0xFFFF);

    let prot = [0x66, 0xB8, 0x00, 0x80, 0x98, 0x99, 0xF4]; // mov ax,0x8000; cwde; cdq; hlt
    let machine = run_test(CpuMode::Protected, &prot, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX), 0xFFFF_8000);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX), 0xFFFF_FFFF);

    let long = [0xB8, 0x00, 0x00, 0x00, 0x80, 0x48, 0x98, 0x48, 0x99, 0xF4]; // mov eax,0x80000000; cdqe; cqo; hlt
    let machine = run_test(CpuMode::Long, &long, |_cpu, _mem, _ports| {});
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::RAX),
        0xFFFF_FFFF_8000_0000
    );
    assert_eq!(
        machine.cpu.read_reg(iced_x86::Register::RDX),
        0xFFFF_FFFF_FFFF_FFFF
    );
}

#[test]
fn control_flow() {
    // jmp short skips mov ax,1.
    let jmp = [0xEB, 0x03, 0xB8, 0x01, 0x00, 0x31, 0xC0, 0xF4];
    let machine = run_test(CpuMode::Real, &jmp, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0);

    // near call/ret
    let call = [
        0xB8, 0x00, 0x00, 0xE8, 0x01, 0x00, 0xF4, 0xB8, 0x34, 0x12, 0xC3,
    ];
    let machine = run_test(CpuMode::Real, &call, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0x1234);

    // far call/retf
    let far = [
        0x9A, 0x08, 0x00, 0x00, 0x00, // call far 0000:0008
        0xF4, // hlt
        0x90, 0x90, // padding
        0xB8, 0x34, 0x12, // mov ax,0x1234
        0xCB, // retf
    ];
    let machine = run_test(CpuMode::Real, &far, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0x1234);

    // loop
    let loop_ = [0xB9, 0x03, 0x00, 0x31, 0xC0, 0x40, 0xE2, 0xFD, 0xF4];
    let machine = run_test(CpuMode::Real, &loop_, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 3);

    // jcxz
    let jcxz = [0xB9, 0x00, 0x00, 0xE3, 0x03, 0xB8, 0x01, 0x00, 0xF4];
    let machine = run_test(CpuMode::Real, &jcxz, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AX), 0);
}

#[test]
fn string_ops_rep_movs_stos_lods_cmps_scas() {
    let src = 0x0100u16;
    let dst = 0x0200u16;
    let code = [
        0xFC, // cld
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xBF,
        (dst & 0xFF) as u8,
        (dst >> 8) as u8, // mov di,dst
        0xB9,
        0x04,
        0x00, // mov cx,4
        0xF3,
        0xA4, // rep movsb
        0xBF,
        (dst & 0xFF) as u8,
        (dst >> 8) as u8, // mov di,dst
        0xB0,
        0xAA, // mov al,0xAA
        0xB9,
        0x04,
        0x00, // mov cx,4
        0xF3,
        0xAA, // rep stosb
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xAC,             // lodsb
        0xBE,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov si,src
        0xBF,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov di,src
        0xA6,             // cmpsb (equal)
        0xBF,
        (src & 0xFF) as u8,
        (src >> 8) as u8, // mov di,src
        0xAE,             // scasb (equal)
        0xF4,
    ];

    let machine = run_test(CpuMode::Real, &code, |_cpu, mem, _ports| {
        mem.load(src as u64, &[1, 2, 3, 4]);
        mem.load(dst as u64, &[1, 2, 3, 4]);
    });

    assert_eq!(
        &machine.mem.data[dst as usize..dst as usize + 4],
        &[0xAA; 4]
    );
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 1);
    assert!(machine.cpu.get_flag(FLAG_ZF));
}

#[test]
fn system_in_out_cli_sti_cpuid_rdtsc_rdmsr_wrmsr_lgdt_lidt_ltr() {
    // in/out + cli/sti
    let in_out = [0xFB, 0xFA, 0xE4, 0x10, 0xE6, 0x11, 0xF4];
    let machine = run_test(CpuMode::Real, &in_out, |cpu, _mem, ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        ports.in_u8.insert(0x10, 0xAB);
    });
    assert!(!machine.cpu.get_flag(RFLAGS_IF));
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::AL), 0xAB);
    assert!(machine.ports.out_u8.contains(&(0x11, 0xAB)));

    // cpuid vendor string (leaf 0)
    let cpuid = [0x0F, 0xA2, 0xF4];
    let machine = run_test(CpuMode::Protected, &cpuid, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
        cpu.write_reg(iced_x86::Register::EAX, 0);
        cpu.write_reg(iced_x86::Register::ECX, 0);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX), 1);

    // rdtsc deterministic readback
    let rdtsc = [0x0F, 0x31, 0xF4];
    let machine = run_test(CpuMode::Protected, &rdtsc, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
        cpu.msr.tsc = 0x1234_5678_9ABC_DEF0;
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX), 0x9ABC_DEF0);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX), 0x1234_5678);

    // wrmsr/rdmsr (use a non-special MSR so the value roundtrips exactly).
    let msr = [0x0F, 0x30, 0x0F, 0x32, 0xF4];
    let machine = run_test(CpuMode::Protected, &msr, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::ESP, 0x2000);
        cpu.write_reg(iced_x86::Register::ECX, 0x11);
        cpu.write_reg(iced_x86::Register::EAX, 0x1234_5678);
        cpu.write_reg(iced_x86::Register::EDX, 0x9ABC_DEF0);
    });
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EAX), 0x1234_5678);
    assert_eq!(machine.cpu.read_reg(iced_x86::Register::EDX), 0x9ABC_DEF0);

    // lgdt/lidt/ltr
    let ldt = [
        0x0F, 0x01, 0x16, 0x00, 0x03, // lgdt [0x0300]
        0x0F, 0x01, 0x1E, 0x06, 0x03, // lidt [0x0306]
        0xB8, 0x28, 0x00, // mov ax,0x28
        0x0F, 0x00, 0xD8, // ltr ax
        0xF4,
    ];
    let machine = run_test(CpuMode::Real, &ldt, |cpu, mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
        mem.write_u16(0x0300, 0x0017).unwrap();
        mem.write_u32(0x0302, 0x1122_3344).unwrap();
        mem.write_u16(0x0306, 0x00FF).unwrap();
        mem.write_u32(0x0308, 0x5566_7788).unwrap();
    });
    assert_eq!(machine.cpu.tables.tr.selector, 0x28);
    assert_eq!(machine.cpu.tables.gdtr.limit, 0x0017);
    assert_eq!(machine.cpu.tables.gdtr.base, 0x1122_3344);
    assert_eq!(machine.cpu.tables.idtr.limit, 0x00FF);
    assert_eq!(machine.cpu.tables.idtr.base, 0x5566_7788);
}

#[test]
fn mov_to_cr0_pe_switches_to_protected_mode() {
    let code = [
        0x66, 0xB8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
        0x66, 0x0F, 0x22, 0xC0, // mov cr0,eax
        0xF4, // hlt
    ];
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::SP, 0x200);
    });
    assert_eq!(machine.cpu.mode, CpuMode::Protected);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RealRegs16 {
    ax: u16,
    bx: u16,
    cx: u16,
    dx: u16,
    sp: u16,
    bp: u16,
    si: u16,
    di: u16,
    flags: u16,
}

fn run_aero_real(test: &[u8], init: RealRegs16) -> RealRegs16 {
    let mut code = Vec::from(test);
    code.push(0xF4); // hlt
    let machine = run_test(CpuMode::Real, &code, |cpu, _mem, _ports| {
        cpu.write_reg(iced_x86::Register::AX, init.ax as u64);
        cpu.write_reg(iced_x86::Register::BX, init.bx as u64);
        cpu.write_reg(iced_x86::Register::CX, init.cx as u64);
        cpu.write_reg(iced_x86::Register::DX, init.dx as u64);
        cpu.write_reg(iced_x86::Register::SP, init.sp as u64);
        cpu.write_reg(iced_x86::Register::BP, init.bp as u64);
        cpu.write_reg(iced_x86::Register::SI, init.si as u64);
        cpu.write_reg(iced_x86::Register::DI, init.di as u64);
    });
    RealRegs16 {
        ax: machine.cpu.read_reg(iced_x86::Register::AX) as u16,
        bx: machine.cpu.read_reg(iced_x86::Register::BX) as u16,
        cx: machine.cpu.read_reg(iced_x86::Register::CX) as u16,
        dx: machine.cpu.read_reg(iced_x86::Register::DX) as u16,
        sp: machine.cpu.read_reg(iced_x86::Register::SP) as u16,
        bp: machine.cpu.read_reg(iced_x86::Register::BP) as u16,
        si: machine.cpu.read_reg(iced_x86::Register::SI) as u16,
        di: machine.cpu.read_reg(iced_x86::Register::DI) as u16,
        flags: machine.cpu.rflags() as u16,
    }
}

fn qemu_available() -> bool {
    std::process::Command::new("qemu-system-i386")
        .arg("--version")
        .output()
        .is_ok()
}

fn build_conformance_boot_sector(init: RealRegs16, test: &[u8]) -> [u8; 512] {
    const BUF: u16 = 0x0500;
    const OUT_LEN: u16 = 18;

    let mut code: Vec<u8> = Vec::new();

    // Basic real-mode setup.
    code.extend_from_slice(&[
        0xFA, // cli
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD8, // mov ds,ax
        0x8E, 0xC0, // mov es,ax
        0x8E, 0xD0, // mov ss,ax
    ]);

    // Initialize stack first so test code can use it.
    code.extend_from_slice(&[0xBC, (init.sp & 0xFF) as u8, (init.sp >> 8) as u8]); // mov sp,imm16

    // Initialize registers.
    code.extend_from_slice(&[0xB8, (init.ax & 0xFF) as u8, (init.ax >> 8) as u8]); // mov ax,imm16
    code.extend_from_slice(&[0xBB, (init.bx & 0xFF) as u8, (init.bx >> 8) as u8]); // mov bx,imm16
    code.extend_from_slice(&[0xB9, (init.cx & 0xFF) as u8, (init.cx >> 8) as u8]); // mov cx,imm16
    code.extend_from_slice(&[0xBA, (init.dx & 0xFF) as u8, (init.dx >> 8) as u8]); // mov dx,imm16
    code.extend_from_slice(&[0xBE, (init.si & 0xFF) as u8, (init.si >> 8) as u8]); // mov si,imm16
    code.extend_from_slice(&[0xBF, (init.di & 0xFF) as u8, (init.di >> 8) as u8]); // mov di,imm16
    code.extend_from_slice(&[0xBD, (init.bp & 0xFF) as u8, (init.bp >> 8) as u8]); // mov bp,imm16

    // Execute test bytes.
    code.extend_from_slice(test);

    // Store regs to [BUF..].
    // mov [imm16], ax  (A3 iw)
    code.extend_from_slice(&[0xA3, (BUF & 0xFF) as u8, (BUF >> 8) as u8]);
    // mov [imm16], bx  (89 1E iw)
    code.extend_from_slice(&[
        0x89,
        0x1E,
        (BUF.wrapping_add(2) & 0xFF) as u8,
        (BUF.wrapping_add(2) >> 8) as u8,
    ]);
    code.extend_from_slice(&[
        0x89,
        0x0E,
        (BUF.wrapping_add(4) & 0xFF) as u8,
        (BUF.wrapping_add(4) >> 8) as u8,
    ]); // cx
    code.extend_from_slice(&[
        0x89,
        0x16,
        (BUF.wrapping_add(6) & 0xFF) as u8,
        (BUF.wrapping_add(6) >> 8) as u8,
    ]); // dx
    code.extend_from_slice(&[
        0x89,
        0x26,
        (BUF.wrapping_add(8) & 0xFF) as u8,
        (BUF.wrapping_add(8) >> 8) as u8,
    ]); // sp
    code.extend_from_slice(&[
        0x89,
        0x2E,
        (BUF.wrapping_add(10) & 0xFF) as u8,
        (BUF.wrapping_add(10) >> 8) as u8,
    ]); // bp
    code.extend_from_slice(&[
        0x89,
        0x36,
        (BUF.wrapping_add(12) & 0xFF) as u8,
        (BUF.wrapping_add(12) >> 8) as u8,
    ]); // si
    code.extend_from_slice(&[
        0x89,
        0x3E,
        (BUF.wrapping_add(14) & 0xFF) as u8,
        (BUF.wrapping_add(14) >> 8) as u8,
    ]); // di
        // flags -> ax
    code.extend_from_slice(&[0x9C, 0x58]); // pushf; pop ax
    code.extend_from_slice(&[
        0xA3,
        (BUF.wrapping_add(16) & 0xFF) as u8,
        (BUF.wrapping_add(16) >> 8) as u8,
    ]);

    // Output buffer bytes.
    code.extend_from_slice(&[0xBE, (BUF & 0xFF) as u8, (BUF >> 8) as u8]); // mov si,BUF
    code.extend_from_slice(&[0xB9, (OUT_LEN & 0xFF) as u8, (OUT_LEN >> 8) as u8]); // mov cx,OUT_LEN
    let loop_start = code.len();
    code.push(0xAC); // lodsb
    code.extend_from_slice(&[0xE6, 0xE9]); // out 0xE9,al
    code.extend_from_slice(&[0xE2, 0x00]); // loop rel8 (patched below)
    let loop_end = code.len();
    let disp = loop_start as i32 - loop_end as i32;
    code[loop_end - 1] = disp as i8 as u8;

    // Exit QEMU (isa-debug-exit).
    code.extend_from_slice(&[0xB0, 0x00, 0xE6, 0xF4]); // mov al,0; out 0xF4,al

    assert!(code.len() <= 510, "boot sector too large: {}", code.len());

    let mut img = [0u8; 512];
    img[..code.len()].copy_from_slice(&code);
    img[510] = 0x55;
    img[511] = 0xAA;
    img
}

fn run_qemu_real(test: &[u8], init: RealRegs16) -> Option<RealRegs16> {
    if !qemu_available() {
        return None;
    }

    std::fs::create_dir_all("target").ok()?;
    let img_path = "target/qemu-conformance.img";
    let out_path = "target/qemu-debugcon.bin";
    let img = build_conformance_boot_sector(init, test);
    std::fs::write(img_path, img).ok()?;
    let _ = std::fs::remove_file(out_path);

    let _output = std::process::Command::new("qemu-system-i386")
        .args([
            "-display",
            "none",
            "-serial",
            "none",
            "-monitor",
            "none",
            "-no-reboot",
            "-drive",
            &format!("format=raw,file={img_path},if=floppy"),
            "-debugcon",
            &format!("file:{out_path}"),
            "-global",
            "isa-debugcon.iobase=0xe9",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x04",
        ])
        .output()
        .ok()?;

    let bytes = std::fs::read(out_path).ok()?;
    if bytes.len() < 18 {
        return None;
    }
    let w = |off: usize| u16::from_le_bytes([bytes[off], bytes[off + 1]]);
    Some(RealRegs16 {
        ax: w(0),
        bx: w(2),
        cx: w(4),
        dx: w(6),
        sp: w(8),
        bp: w(10),
        si: w(12),
        di: w(14),
        flags: w(16),
    })
}

#[test]
#[ignore]
fn conformance_add_al_bl_against_qemu() {
    let init = RealRegs16 {
        ax: 0x00FF,
        bx: 0x0001,
        cx: 0,
        dx: 0,
        sp: 0x7C00,
        bp: 0,
        si: 0,
        di: 0,
        flags: 0x0002,
    };
    let test = [0x00, 0xD8]; // add al,bl
    let Some(qemu) = run_qemu_real(&test, init) else {
        return;
    };
    let aero = run_aero_real(&test, init);
    assert_eq!(aero, qemu);
}
