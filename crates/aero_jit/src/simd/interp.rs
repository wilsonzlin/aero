use crate::simd::sse::{Inst, Operand, Program};
use crate::simd::state::SseState;
use thiserror::Error;

pub fn interpret(program: &Program, state: &mut SseState, mem: &mut [u8]) -> Result<(), ExecError> {
    for inst in &program.insts {
        match *inst {
            Inst::MovdquLoad { dst, addr } => {
                let v = load_mem_v128(mem, addr)?;
                state.xmm[dst.index()] = v;
            }
            Inst::MovdquStore { addr, src } => {
                let v = state.xmm[src.index()];
                store_mem_v128(mem, addr, v)?;
            }

            Inst::Addps { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f32x4_binop(a, b, |x, y| x + y);
            }
            Inst::Subps { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f32x4_binop(a, b, |x, y| x - y);
            }
            Inst::Mulps { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f32x4_binop(a, b, |x, y| x * y);
            }
            Inst::Divps { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f32x4_binop(a, b, |x, y| x / y);
            }

            Inst::Addpd { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f64x2_binop(a, b, |x, y| x + y);
            }
            Inst::Subpd { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f64x2_binop(a, b, |x, y| x - y);
            }
            Inst::Mulpd { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f64x2_binop(a, b, |x, y| x * y);
            }
            Inst::Divpd { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f64x2_binop(a, b, |x, y| x / y);
            }

            Inst::Pand { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = a & b;
            }
            Inst::Por { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = a | b;
            }
            Inst::Pxor { dst, src } => {
                let a = state.xmm[dst.index()];
                let b = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = a ^ b;
            }

            Inst::Pshufb { dst, src } => {
                let data_bytes = state.xmm[dst.index()].to_le_bytes();
                let control = load_operand(state, mem, src)?.to_le_bytes();

                let mut out = [0u8; 16];
                for (i, &ctrl) in control.iter().enumerate() {
                    if (ctrl & 0x80) != 0 {
                        out[i] = 0;
                    } else {
                        out[i] = data_bytes[(ctrl & 0x0F) as usize];
                    }
                }
                state.xmm[dst.index()] = u128::from_le_bytes(out);
            }

            Inst::Sqrtps { dst, src } => {
                let v = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f32x4_unop(v, |x| x.sqrt());
            }
            Inst::Sqrtpd { dst, src } => {
                let v = load_operand(state, mem, src)?;
                state.xmm[dst.index()] = f64x2_unop(v, |x| x.sqrt());
            }

            Inst::PslldImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i32x4(v, imm, ShiftDir::Left);
            }
            Inst::PsrldImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i32x4(v, imm, ShiftDir::RightLogical);
            }

            Inst::PsllwImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i16x8(v, imm, ShiftDir::Left);
            }
            Inst::PsrlwImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i16x8(v, imm, ShiftDir::RightLogical);
            }

            Inst::PsllqImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i64x2(v, imm, ShiftDir::Left);
            }
            Inst::PsrlqImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_i64x2(v, imm, ShiftDir::RightLogical);
            }

            Inst::PslldqImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_bytes(v, imm, ShiftDir::Left);
            }
            Inst::PsrldqImm { dst, imm } => {
                let v = state.xmm[dst.index()];
                state.xmm[dst.index()] = shift_bytes(v, imm, ShiftDir::RightLogical);
            }
        }
    }

    Ok(())
}

fn load_operand(state: &SseState, mem: &[u8], op: Operand) -> Result<u128, ExecError> {
    match op {
        Operand::Reg(reg) => Ok(state.xmm[reg.index()]),
        Operand::Mem(addr) => load_mem_v128(mem, addr),
    }
}

fn load_mem_v128(mem: &[u8], addr: u32) -> Result<u128, ExecError> {
    let start = addr as usize;
    let end = start.checked_add(16).ok_or(ExecError::MemOob {
        addr,
        access_size: 16,
        mem_len: mem.len(),
    })?;
    let bytes: [u8; 16] = mem
        .get(start..end)
        .ok_or(ExecError::MemOob {
            addr,
            access_size: 16,
            mem_len: mem.len(),
        })?
        .try_into()
        .expect("slice len matches");
    Ok(u128::from_le_bytes(bytes))
}

fn store_mem_v128(mem: &mut [u8], addr: u32, value: u128) -> Result<(), ExecError> {
    let start = addr as usize;
    let end = start.checked_add(16).ok_or(ExecError::MemOob {
        addr,
        access_size: 16,
        mem_len: mem.len(),
    })?;
    let mem_len = mem.len();
    let dst = mem.get_mut(start..end).ok_or(ExecError::MemOob {
        addr,
        access_size: 16,
        mem_len,
    })?;
    dst.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn f32x4_binop(a: u128, b: u128, f: impl Fn(f32, f32) -> f32) -> u128 {
    let a_bytes = a.to_le_bytes();
    let b_bytes = b.to_le_bytes();

    let mut out = [0u8; 16];
    for lane in 0..4 {
        let off = lane * 4;
        let av = f32::from_bits(u32::from_le_bytes(
            a_bytes[off..off + 4].try_into().expect("slice len matches"),
        ));
        let bv = f32::from_bits(u32::from_le_bytes(
            b_bytes[off..off + 4].try_into().expect("slice len matches"),
        ));
        let rv = f(av, bv).to_bits().to_le_bytes();
        out[off..off + 4].copy_from_slice(&rv);
    }
    u128::from_le_bytes(out)
}

fn f64x2_binop(a: u128, b: u128, f: impl Fn(f64, f64) -> f64) -> u128 {
    let a_bytes = a.to_le_bytes();
    let b_bytes = b.to_le_bytes();

    let mut out = [0u8; 16];
    for lane in 0..2 {
        let off = lane * 8;
        let av = f64::from_bits(u64::from_le_bytes(
            a_bytes[off..off + 8].try_into().expect("slice len matches"),
        ));
        let bv = f64::from_bits(u64::from_le_bytes(
            b_bytes[off..off + 8].try_into().expect("slice len matches"),
        ));
        let rv = f(av, bv).to_bits().to_le_bytes();
        out[off..off + 8].copy_from_slice(&rv);
    }
    u128::from_le_bytes(out)
}

fn f32x4_unop(v: u128, f: impl Fn(f32) -> f32) -> u128 {
    let bytes = v.to_le_bytes();
    let mut out = [0u8; 16];
    for lane in 0..4 {
        let off = lane * 4;
        let x = f32::from_bits(u32::from_le_bytes(
            bytes[off..off + 4].try_into().expect("slice len matches"),
        ));
        out[off..off + 4].copy_from_slice(&f(x).to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn f64x2_unop(v: u128, f: impl Fn(f64) -> f64) -> u128 {
    let bytes = v.to_le_bytes();
    let mut out = [0u8; 16];
    for lane in 0..2 {
        let off = lane * 8;
        let x = f64::from_bits(u64::from_le_bytes(
            bytes[off..off + 8].try_into().expect("slice len matches"),
        ));
        out[off..off + 8].copy_from_slice(&f(x).to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    RightLogical,
}

fn shift_i32x4(v: u128, imm: u8, dir: ShiftDir) -> u128 {
    // x86 packed shifts treat counts >= lane_bits as producing all-zero elements.
    // (Unlike WebAssembly SIMD shifts, which mask the count.)
    if imm > 31 {
        return 0;
    }

    let bytes = v.to_le_bytes();
    let mut out = [0u8; 16];
    for lane in 0..4 {
        let off = lane * 4;
        let x = u32::from_le_bytes(bytes[off..off + 4].try_into().expect("slice len matches"));
        let y = match dir {
            ShiftDir::Left => x.wrapping_shl(imm as u32),
            ShiftDir::RightLogical => x.wrapping_shr(imm as u32),
        };
        out[off..off + 4].copy_from_slice(&y.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn shift_i16x8(v: u128, imm: u8, dir: ShiftDir) -> u128 {
    if imm > 15 {
        return 0;
    }

    let bytes = v.to_le_bytes();
    let mut out = [0u8; 16];
    for lane in 0..8 {
        let off = lane * 2;
        let x = u16::from_le_bytes(bytes[off..off + 2].try_into().expect("slice len matches"));
        let y = match dir {
            ShiftDir::Left => x.wrapping_shl(imm as u32),
            ShiftDir::RightLogical => x.wrapping_shr(imm as u32),
        };
        out[off..off + 2].copy_from_slice(&y.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn shift_i64x2(v: u128, imm: u8, dir: ShiftDir) -> u128 {
    if imm > 63 {
        return 0;
    }

    let bytes = v.to_le_bytes();
    let mut out = [0u8; 16];
    for lane in 0..2 {
        let off = lane * 8;
        let x = u64::from_le_bytes(bytes[off..off + 8].try_into().expect("slice len matches"));
        let y = match dir {
            ShiftDir::Left => x.wrapping_shl(imm as u32),
            ShiftDir::RightLogical => x.wrapping_shr(imm as u32),
        };
        out[off..off + 8].copy_from_slice(&y.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn shift_bytes(v: u128, imm: u8, dir: ShiftDir) -> u128 {
    if imm > 15 {
        return 0;
    }
    let bytes = v.to_le_bytes();
    let shift = imm as usize;
    let mut out = [0u8; 16];
    match dir {
        ShiftDir::Left => {
            for i in shift..16 {
                out[i] = bytes[i - shift];
            }
        }
        ShiftDir::RightLogical => {
            for i in 0..(16 - shift) {
                out[i] = bytes[i + shift];
            }
        }
    }
    u128::from_le_bytes(out)
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExecError {
    #[error("guest memory out of bounds: addr={addr} access_size={access_size} mem_len={mem_len}")]
    MemOob {
        addr: u32,
        access_size: usize,
        mem_len: usize,
    },
}
