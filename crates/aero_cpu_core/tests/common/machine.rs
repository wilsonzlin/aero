use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::interrupts;
use aero_cpu_core::interrupts::PendingEventState;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::CpuState;
use aero_cpu_core::{AssistReason, Exception};
use aero_x86::{Mnemonic, OpKind, Register};

/// Simple test bus backed by [`FlatTestBus`] with a "debugcon" port at `0xE9`.
///
/// Tier-0 uses [`CpuBus`] for memory accesses and shares the same bus surface
/// with the Tier-0 interrupt/exception delivery helpers in
/// [`aero_cpu_core::interrupts`].
#[derive(Debug, Clone)]
pub struct TestBus {
    mem: FlatTestBus,
    debugcon: Vec<u8>,
}

impl TestBus {
    pub fn new(size: usize) -> Self {
        Self {
            mem: FlatTestBus::new(size),
            debugcon: Vec::new(),
        }
    }

    pub fn load(&mut self, addr: u64, bytes: &[u8]) {
        self.mem.load(addr, bytes);
    }

    pub fn debugcon(&self) -> &[u8] {
        &self.debugcon
    }
}

impl CpuBus for TestBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.mem.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.mem.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.mem.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.mem.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.mem.read_u128(vaddr)
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
        self.mem.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.mem.write_u128(vaddr, val)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.mem.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        if port == 0xE9 {
            match size {
                1 => self.debugcon.push(val as u8),
                2 => self.debugcon.extend_from_slice(&(val as u16).to_le_bytes()),
                4 => self.debugcon.extend_from_slice(&(val as u32).to_le_bytes()),
                8 => self.debugcon.extend_from_slice(&val.to_le_bytes()),
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        Ok(())
    }
}

/// A tiny Tier-0 execution harness that can run real-mode snippets which use:
/// - INT/IRET (via the `aero_cpu_core::interrupts` delivery implementation)
/// - IN/OUT (via [`CpuBus::io_read`] / [`CpuBus::io_write`])
#[derive(Debug)]
pub struct Tier0Machine {
    pub cpu: CpuState,
    pending: PendingEventState,
    pub bus: TestBus,
}

impl Tier0Machine {
    pub fn new(cpu: CpuState, bus: TestBus) -> Self {
        Self {
            cpu,
            pending: PendingEventState::default(),
            bus,
        }
    }

    pub fn run(&mut self, max_instructions: u64) {
        let mut executed = 0u64;
        while executed < max_instructions {
            let res = run_batch(&mut self.cpu, &mut self.bus, 1024);
            executed += res.executed;
            match res.exit {
                BatchExit::Completed | BatchExit::Branch => continue,
                BatchExit::Halted => return,
                BatchExit::Exception(e) => panic!("unexpected exception: {e:?}"),
                BatchExit::Assist(r) => {
                    self.handle_assist(r);
                    executed += 1;
                }
            }
        }
        panic!("machine did not halt (executed {executed} instructions)");
    }

    fn handle_assist(&mut self, reason: AssistReason) {
        let ip = self.cpu.rip();
        let fetch_addr = self.cpu.seg_base_reg(Register::CS).wrapping_add(ip);
        let bytes = self.bus.fetch(fetch_addr, 15).expect("fetch");
        let decoded = aero_x86::decode(&bytes, ip, self.cpu.bitness()).expect("decode");
        let next_ip = ip.wrapping_add(decoded.len as u64) & self.cpu.mode.ip_mask();

        match reason {
            AssistReason::Io => self.assist_io(&decoded.instr, next_ip),
            AssistReason::Interrupt => self.assist_interrupt(&decoded.instr, next_ip),
            other => panic!("unhandled assist reason: {other:?} at rip=0x{ip:X}"),
        }
    }

    fn assist_io(&mut self, instr: &aero_x86::Instruction, next_ip: u64) {
        match instr.mnemonic() {
            Mnemonic::In => {
                let port = read_io_port(&self.cpu, instr, 1);
                let dst = instr.op0_register();
                let size = io_reg_size(dst);
                let val = self
                    .bus
                    .io_read(port, size)
                    .unwrap_or_else(|e| panic!("io_read failed: {e:?}"));
                self.cpu.write_reg(dst, val);
                self.cpu.set_rip(next_ip);
            }
            Mnemonic::Out => {
                let port = read_io_port(&self.cpu, instr, 0);
                let src = instr.op1_register();
                let size = io_reg_size(src);
                let val = self.cpu.read_reg(src);
                self.bus
                    .io_write(port, size, val)
                    .unwrap_or_else(|e| panic!("io_write failed: {e:?}"));
                self.cpu.set_rip(next_ip);
            }
            other => panic!("unhandled IO assist mnemonic: {other:?}"),
        }
    }

    fn assist_interrupt(&mut self, instr: &aero_x86::Instruction, next_ip: u64) {
        match instr.mnemonic() {
            Mnemonic::Int => {
                let vector = instr.immediate8();
                self.deliver_software_interrupt(vector, next_ip);
            }
            Mnemonic::Int1 => self.deliver_software_interrupt(1, next_ip),
            Mnemonic::Int3 => self.deliver_software_interrupt(3, next_ip),
            Mnemonic::Into => {
                // INTO triggers INT 4 when OF=1, otherwise it is a no-op.
                if self.cpu.get_flag(aero_cpu_core::state::RFLAGS_OF) {
                    self.deliver_software_interrupt(4, next_ip);
                } else {
                    self.cpu.set_rip(next_ip);
                }
            }
            Mnemonic::Iret | Mnemonic::Iretd | Mnemonic::Iretq => self.exec_iret(),
            Mnemonic::Cli => {
                // Tier-0 uses an assist for CLI/STI because privileged checks depend
                // on system state. For real-mode tests we only model IF itself.
                self.cpu.set_flag(aero_cpu_core::state::RFLAGS_IF, false);
                self.cpu.set_rip(next_ip);
            }
            Mnemonic::Sti => {
                self.cpu.set_flag(aero_cpu_core::state::RFLAGS_IF, true);
                self.pending.inhibit_interrupts_for_one_instruction();
                self.cpu.set_rip(next_ip);
            }
            other => panic!("unhandled interrupt assist mnemonic: {other:?}"),
        }
    }

    fn deliver_software_interrupt(&mut self, vector: u8, return_rip: u64) {
        self.pending.raise_software_interrupt(vector, return_rip);
        interrupts::deliver_pending_event(&mut self.cpu, &mut self.bus, &mut self.pending)
            .expect("interrupt delivery");
    }

    fn exec_iret(&mut self) {
        interrupts::iret(&mut self.cpu, &mut self.bus, &mut self.pending).expect("iret");
    }
}

fn read_io_port(cpu: &CpuState, instr: &aero_x86::Instruction, op: u32) -> u16 {
    match instr.op_kind(op) {
        OpKind::Immediate8 => instr.immediate8() as u16,
        OpKind::Immediate16 => instr.immediate16(),
        OpKind::Register => cpu.read_reg(instr.op_register(op)) as u16,
        other => panic!("unsupported IO port operand: {other:?}"),
    }
}

fn io_reg_size(reg: Register) -> u32 {
    match reg {
        Register::AL => 1,
        Register::AX => 2,
        Register::EAX => 4,
        _ => panic!("unsupported IO data register: {reg:?}"),
    }
}
