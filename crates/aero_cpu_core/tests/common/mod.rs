#![allow(dead_code)]

use aero_cpu_core::interp::tier0::exec::{run_batch, step, BatchExit, StepExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_AF, FLAG_CF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};
use aero_x86::Register;

pub mod machine;

pub const BUS_SIZE: usize = 0x10000;
pub const MEM_BASE: u64 = 0x0500;
pub const MEM_HASH_LEN: usize = 256;
pub const CODE_BASE: u64 = 0x0700;

/// Sentinel return address used by the Aero-side harness.
///
/// The QEMU harness executes snippets via `call CODE_BASE`, so the real return address is into the
/// boot sector. For Aero we use an arbitrary sentinel and stop once `ret` returns to it.
pub const RETURN_IP: u16 = 0xFFFF;

pub const FLAG_MASK: u16 =
    (FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF | 0x2) as u16;

pub const MAX_INSTRUCTIONS: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuOutcome {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub flags: u16,
    pub mem_hash: u32,
}

impl CpuOutcome {
    pub fn from_state_and_bus(state: &CpuState, bus: &FlatTestBus) -> Self {
        let mem_hash = fnv1a32(bus.slice(MEM_BASE, MEM_HASH_LEN));
        Self {
            ax: state.read_reg(Register::AX) as u16,
            bx: state.read_reg(Register::BX) as u16,
            cx: state.read_reg(Register::CX) as u16,
            dx: state.read_reg(Register::DX) as u16,
            si: state.read_reg(Register::SI) as u16,
            di: state.read_reg(Register::DI) as u16,
            bp: state.read_reg(Register::BP) as u16,
            sp: state.read_reg(Register::SP) as u16,
            flags: (state.rflags() & 0xFFFF) as u16,
            mem_hash,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnippetCase {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub flags: u16,
    pub mem_init: [u8; MEM_HASH_LEN],
    pub code: Vec<u8>,
}

impl SnippetCase {
    pub fn with_code(code: Vec<u8>) -> Self {
        Self {
            ax: 0,
            bx: 0,
            cx: 0,
            dx: 0,
            si: 0,
            di: 0,
            bp: 0,
            sp: 0x9000,
            flags: 0x2,
            mem_init: [0; MEM_HASH_LEN],
            code,
        }
    }
}

pub fn run_tier0_batch(case: &SnippetCase) -> CpuOutcome {
    let (mut state, mut bus) = init_case(case);
    run_until_rip_batch(&mut state, &mut bus, RETURN_IP as u64);
    CpuOutcome::from_state_and_bus(&state, &bus)
}

pub fn run_tier0_single_step(case: &SnippetCase) -> CpuOutcome {
    let (mut state, mut bus) = init_case(case);
    run_until_rip_single_step(&mut state, &mut bus, RETURN_IP as u64);
    CpuOutcome::from_state_and_bus(&state, &bus)
}

fn init_case(case: &SnippetCase) -> (CpuState, FlatTestBus) {
    let mut bus = FlatTestBus::new(BUS_SIZE);
    bus.load(MEM_BASE, &case.mem_init);
    bus.load(CODE_BASE, &case.code);

    let mut state = CpuState::new(CpuMode::Bit16);
    state.set_rip(CODE_BASE);
    state.set_rflags(case.flags as u64);

    state.write_reg(Register::AX, case.ax as u64);
    state.write_reg(Register::BX, case.bx as u64);
    state.write_reg(Register::CX, case.cx as u64);
    state.write_reg(Register::DX, case.dx as u64);
    state.write_reg(Register::SI, case.si as u64);
    state.write_reg(Register::DI, case.di as u64);
    state.write_reg(Register::BP, case.bp as u64);

    // Emulate the pre-state of a near CALL into CODE_BASE (push return address).
    let sp_pushed = case.sp.wrapping_sub(2);
    bus.write_u16(sp_pushed as u64, RETURN_IP)
        .expect("stack write");
    state.write_reg(Register::SP, sp_pushed as u64);

    (state, bus)
}

fn run_until_rip_batch(state: &mut CpuState, bus: &mut FlatTestBus, stop_rip: u64) {
    let mut executed = 0u64;
    while executed < MAX_INSTRUCTIONS {
        if state.rip() == stop_rip {
            return;
        }
        let res = run_batch(state, bus, 1024);
        executed += res.executed;
        match res.exit {
            BatchExit::Completed | BatchExit::Branch => continue,
            BatchExit::Halted => panic!("unexpected HLT at rip=0x{:X}", state.rip()),
            BatchExit::BiosInterrupt(vector) => {
                panic!("unexpected BIOS interrupt {vector:#x} at rip=0x{:X}", state.rip())
            }
            BatchExit::Assist(r) => panic!("unexpected assist: {r:?}"),
            BatchExit::Exception(e) => panic!("unexpected exception: {e:?}"),
        }
    }
    panic!("program did not reach stop RIP");
}

fn run_until_rip_single_step(state: &mut CpuState, bus: &mut FlatTestBus, stop_rip: u64) {
    let mut executed = 0u64;
    while executed < MAX_INSTRUCTIONS {
        if state.rip() == stop_rip {
            return;
        }
        let exit = step(state, bus).expect("step");
        executed += 1;
        match exit {
            StepExit::Continue | StepExit::Branch => continue,
            StepExit::Halted => panic!("unexpected HLT at rip=0x{:X}", state.rip()),
            StepExit::BiosInterrupt(vector) => {
                panic!("unexpected BIOS interrupt {vector:#x} at rip=0x{:X}", state.rip())
            }
            StepExit::Assist(r) => panic!("unexpected assist: {r:?}"),
        }
    }
    panic!("program did not reach stop RIP");
}

pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811C_9DC5;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

// --- Small assembler helpers (16-bit real mode) -----------------------------

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reg16 {
    Ax = 0,
    Cx = 1,
    Dx = 2,
    Bx = 3,
    Sp = 4,
    Bp = 5,
    Si = 6,
    Di = 7,
}

pub fn mov_reg_imm16(reg: Reg16, imm: u16) -> [u8; 3] {
    let op = 0xB8u8 + (reg as u8);
    [op, (imm & 0xFF) as u8, (imm >> 8) as u8]
}

pub fn add_reg_imm16(reg: Reg16, imm: u16) -> [u8; 4] {
    // 81 /0 iw : ADD r/m16, imm16 (modrm mod=11)
    let modrm = 0xC0u8 | (reg as u8);
    [0x81, modrm, (imm & 0xFF) as u8, (imm >> 8) as u8]
}

pub fn sub_reg_imm16(reg: Reg16, imm: u16) -> [u8; 4] {
    // 81 /5 iw : SUB r/m16, imm16 (modrm mod=11)
    let modrm = 0xC0u8 | (5 << 3) | (reg as u8);
    [0x81, modrm, (imm & 0xFF) as u8, (imm >> 8) as u8]
}

pub fn cmp_reg_imm16(reg: Reg16, imm: u16) -> [u8; 4] {
    // 81 /7 iw : CMP r/m16, imm16 (modrm mod=11)
    let modrm = 0xC0u8 | (7 << 3) | (reg as u8);
    [0x81, modrm, (imm & 0xFF) as u8, (imm >> 8) as u8]
}

pub fn inc_reg(reg: Reg16) -> [u8; 1] {
    [0x40u8 + (reg as u8)]
}

pub fn dec_reg(reg: Reg16) -> [u8; 1] {
    [0x48u8 + (reg as u8)]
}

pub fn mov_mem_abs_reg16(disp: u16, reg: Reg16) -> [u8; 4] {
    // 89 /r : MOV r/m16, r16 with mod=00 rm=110 disp16
    let modrm = ((reg as u8) << 3) | 0x06;
    [0x89, modrm, (disp & 0xFF) as u8, (disp >> 8) as u8]
}

pub fn mov_reg16_mem_abs(reg: Reg16, disp: u16) -> [u8; 4] {
    // 8B /r : MOV r16, r/m16 with mod=00 rm=110 disp16
    let modrm = ((reg as u8) << 3) | 0x06;
    [0x8B, modrm, (disp & 0xFF) as u8, (disp >> 8) as u8]
}

pub fn jz(disp: i8) -> [u8; 2] {
    [0x74, disp as u8]
}

pub fn jnz(disp: i8) -> [u8; 2] {
    [0x75, disp as u8]
}

pub fn jc(disp: i8) -> [u8; 2] {
    [0x72, disp as u8]
}

pub fn jmp(disp: i8) -> [u8; 2] {
    [0xEB, disp as u8]
}

pub fn ret() -> [u8; 1] {
    [0xC3]
}

pub struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    pub fn next_u16(&mut self) -> u16 {
        self.next_u32() as u16
    }

    pub fn next_u8(&mut self) -> u8 {
        self.next_u32() as u8
    }

    pub fn fill_bytes(&mut self, buf: &mut [u8]) {
        for b in buf {
            *b = self.next_u8();
        }
    }
}
