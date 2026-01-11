use super::ops_data::{calc_ea, op_bits, read_op_sized, reg_bits};
use super::{ExecOutcome, Tier0Config};
use crate::cpuid::bits as cpuid_bits;
use crate::exception::Exception;
use crate::interp::{sse3, sse41, sse42, ssse3};
use crate::mem::CpuBus;
use crate::state::{
    mask_bits, CpuState, CR0_EM, CR0_TS, CR4_OSFXSR, CR4_OSXMMEXCPT, FLAG_CF, FLAG_OF, FLAG_SF,
    FLAG_ZF,
};
use aero_x86::{DecodedInst, Instruction, Mnemonic, OpKind, Register};

const MXCSR_EXCEPTION_MASK: u32 = 0x1F80;
const MXCSR_IE: u32 = 1 << 0;
const MXCSR_PE: u32 = 1 << 5;
const MXCSR_RC_MASK: u32 = 0b11 << 13;

#[derive(Clone, Copy, Debug)]
enum RoundingMode {
    Nearest,
    Down,
    Up,
    TowardZero,
}

#[inline]
fn rounding_mode(mxcsr: u32) -> RoundingMode {
    match (mxcsr & MXCSR_RC_MASK) >> 13 {
        0 => RoundingMode::Nearest,
        1 => RoundingMode::Down,
        2 => RoundingMode::Up,
        3 => RoundingMode::TowardZero,
        _ => unreachable!(),
    }
}

pub fn handles_mnemonic(m: Mnemonic) -> bool {
    matches!(
        m,
        // SSE
        Mnemonic::Movaps
            | Mnemonic::Movups
            | Mnemonic::Movss
            | Mnemonic::Movhlps
            | Mnemonic::Movlhps
            | Mnemonic::Unpcklps
            | Mnemonic::Unpckhps
            | Mnemonic::Shufps
            | Mnemonic::Andps
            | Mnemonic::Orps
            | Mnemonic::Xorps
            | Mnemonic::Andnps
            | Mnemonic::Addss
            | Mnemonic::Subss
            | Mnemonic::Mulss
            | Mnemonic::Divss
            | Mnemonic::Addps
            | Mnemonic::Subps
            | Mnemonic::Mulps
            | Mnemonic::Divps
            | Mnemonic::Cvtsi2ss
            | Mnemonic::Cvtss2si
            | Mnemonic::Cvttss2si
        // SSE2
        | Mnemonic::Movdqa
            | Mnemonic::Movdqu
            | Mnemonic::Movsd
            | Mnemonic::Movd
            | Mnemonic::Movq
            | Mnemonic::Pand
            | Mnemonic::Por
            | Mnemonic::Pxor
            | Mnemonic::Pandn
            | Mnemonic::Paddb
            | Mnemonic::Paddw
            | Mnemonic::Paddd
            | Mnemonic::Paddq
            | Mnemonic::Psubb
            | Mnemonic::Psubw
            | Mnemonic::Psubd
            | Mnemonic::Psubq
            | Mnemonic::Paddsw
            | Mnemonic::Paddusw
            | Mnemonic::Psubsw
            | Mnemonic::Psubusw
            | Mnemonic::Paddsb
            | Mnemonic::Paddusb
            | Mnemonic::Psubsb
            | Mnemonic::Psubusb
            | Mnemonic::Psllw
            | Mnemonic::Pslld
            | Mnemonic::Psllq
            | Mnemonic::Psrlw
            | Mnemonic::Psrld
            | Mnemonic::Psrlq
            | Mnemonic::Psraw
            | Mnemonic::Psrad
            | Mnemonic::Pslldq
            | Mnemonic::Psrldq
            | Mnemonic::Pshufd
            | Mnemonic::Pcmpeqb
            | Mnemonic::Pcmpeqw
            | Mnemonic::Pcmpeqd
            | Mnemonic::Pcmpgtb
            | Mnemonic::Pcmpgtw
            | Mnemonic::Pcmpgtd
            | Mnemonic::Pmullw
            | Mnemonic::Pmuludq
            | Mnemonic::Addsd
            | Mnemonic::Subsd
            | Mnemonic::Mulsd
            | Mnemonic::Divsd
            | Mnemonic::Addpd
            | Mnemonic::Subpd
            | Mnemonic::Mulpd
            | Mnemonic::Divpd
            | Mnemonic::Cvtsi2sd
            | Mnemonic::Cvtsd2si
            | Mnemonic::Cvttsd2si
        // SSE3
        | Mnemonic::Lddqu
            | Mnemonic::Haddps
            | Mnemonic::Haddpd
            | Mnemonic::Hsubps
            | Mnemonic::Hsubpd
            | Mnemonic::Movddup
            | Mnemonic::Movsldup
            | Mnemonic::Movshdup
        // SSSE3
        | Mnemonic::Pshufb
            | Mnemonic::Phaddw
            | Mnemonic::Phaddd
            | Mnemonic::Phaddsw
            | Mnemonic::Pmaddubsw
            | Mnemonic::Pabsb
            | Mnemonic::Pabsw
            | Mnemonic::Pabsd
            | Mnemonic::Palignr
        // SSE4.1
        | Mnemonic::Insertps
            | Mnemonic::Pblendw
            | Mnemonic::Ptest
            | Mnemonic::Pmovsxbw
            | Mnemonic::Pmovsxbd
            | Mnemonic::Pmovsxbq
            | Mnemonic::Pmovzxbw
            | Mnemonic::Pmovzxbd
            | Mnemonic::Pmovzxbq
            | Mnemonic::Pmulld
            | Mnemonic::Pcmpeqq
        // SSE4.2
        | Mnemonic::Crc32
            | Mnemonic::Pcmpestri
            | Mnemonic::Pcmpestrm
            | Mnemonic::Pcmpistri
            | Mnemonic::Pcmpistrm
        // POPCNT (scalar extension)
        | Mnemonic::Popcnt
    )
}

pub fn exec<B: CpuBus>(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut B,
    decoded: &DecodedInst,
    next_ip: u64,
) -> Result<ExecOutcome, Exception> {
    let instr = &decoded.instr;
    match instr.mnemonic() {
        // ---- Scalar extensions (no XMM state) ------------------------------
        Mnemonic::Popcnt => {
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_POPCNT)?;
            exec_popcnt(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Crc32 => {
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE42)?;
            exec_crc32(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }

        // ---- XMM / MXCSR-gated instructions -------------------------------
        Mnemonic::Movaps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_mov128(state, bus, instr, next_ip, Some(16))?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movups => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_mov128(state, bus, instr, next_ip, None)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movdqa => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_mov128(state, bus, instr, next_ip, Some(16))?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movdqu => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_mov128(state, bus, instr, next_ip, None)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movss => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_movss(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movsd => {
            check_xmm_available(state)?;
            // Note: `Mnemonic::Movsd` is shared with the string instruction, but
            // Tier-0 currently only implements the scalar SSE2 form.
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_movsd(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movd => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_movd(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movq => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_movq(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movhlps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_movhlps(state, instr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movlhps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_movlhps(state, instr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Unpcklps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_unpcklps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Unpckhps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_unpckhps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Shufps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_shufps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Andps | Mnemonic::Orps | Mnemonic::Xorps | Mnemonic::Andnps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_logic_ps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pand | Mnemonic::Por | Mnemonic::Pxor | Mnemonic::Pandn => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_logic_pd(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Paddb
        | Mnemonic::Paddw
        | Mnemonic::Paddd
        | Mnemonic::Paddq
        | Mnemonic::Psubb
        | Mnemonic::Psubw
        | Mnemonic::Psubd
        | Mnemonic::Psubq
        | Mnemonic::Paddsb
        | Mnemonic::Paddusb
        | Mnemonic::Psubsb
        | Mnemonic::Psubusb
        | Mnemonic::Paddsw
        | Mnemonic::Paddusw
        | Mnemonic::Psubsw
        | Mnemonic::Psubusw
        | Mnemonic::Pcmpeqb
        | Mnemonic::Pcmpeqw
        | Mnemonic::Pcmpeqd
        | Mnemonic::Pcmpgtb
        | Mnemonic::Pcmpgtw
        | Mnemonic::Pcmpgtd
        | Mnemonic::Pmullw
        | Mnemonic::Pmuludq
        | Mnemonic::Pshufd => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_sse2_int(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Psllw
        | Mnemonic::Pslld
        | Mnemonic::Psllq
        | Mnemonic::Psrlw
        | Mnemonic::Psrld
        | Mnemonic::Psrlq
        | Mnemonic::Psraw
        | Mnemonic::Psrad
        | Mnemonic::Pslldq
        | Mnemonic::Psrldq => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_sse2_shift(state, instr)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Addss | Mnemonic::Subss | Mnemonic::Mulss | Mnemonic::Divss => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_scalar_f32(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Addps | Mnemonic::Subps | Mnemonic::Mulps | Mnemonic::Divps => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_packed_f32(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Addsd | Mnemonic::Subsd | Mnemonic::Mulsd | Mnemonic::Divsd => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            match instr.mnemonic() {
                Mnemonic::Addsd => exec_scalar_f64(state, bus, instr, next_ip, |a, b| a + b)?,
                Mnemonic::Subsd => exec_scalar_f64(state, bus, instr, next_ip, |a, b| a - b)?,
                Mnemonic::Mulsd => exec_scalar_f64(state, bus, instr, next_ip, |a, b| a * b)?,
                Mnemonic::Divsd => exec_scalar_f64(state, bus, instr, next_ip, |a, b| a / b)?,
                _ => unreachable!(),
            }
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Addpd | Mnemonic::Subpd | Mnemonic::Mulpd | Mnemonic::Divpd => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_packed_f64(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cvtsi2ss => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_cvtsi2ss(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cvtss2si | Mnemonic::Cvttss2si => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE)?;
            exec_cvtss2si(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cvtsi2sd => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_cvtsi2sd(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Cvtsd2si | Mnemonic::Cvttsd2si => {
            check_xmm_available(state)?;
            require_feature_edx(cfg, cpuid_bits::LEAF1_EDX_SSE2)?;
            exec_cvtsd2si(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Lddqu => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_lddqu(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Haddps => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_haddps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Haddpd => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_haddpd(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Hsubps | Mnemonic::Hsubpd => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_hsub(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movddup => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_movddup(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Movsldup | Mnemonic::Movshdup => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE3)?;
            exec_movdup_ps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pshufb => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSSE3)?;
            exec_pshufb(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Phaddw | Mnemonic::Phaddd | Mnemonic::Phaddsw | Mnemonic::Pmaddubsw => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSSE3)?;
            exec_ssse3_binop(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pabsb | Mnemonic::Pabsw | Mnemonic::Pabsd => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSSE3)?;
            exec_ssse3_abs(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Palignr => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSSE3)?;
            exec_palignr(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Insertps => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_insertps(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pblendw => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_pblendw(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Ptest => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_ptest(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pmovsxbw
        | Mnemonic::Pmovsxbd
        | Mnemonic::Pmovsxbq
        | Mnemonic::Pmovzxbw
        | Mnemonic::Pmovzxbd
        | Mnemonic::Pmovzxbq => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_pmovx(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pmulld => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_pmulld(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pcmpeqq => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE41)?;
            exec_pcmpeqq(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        Mnemonic::Pcmpestri | Mnemonic::Pcmpestrm | Mnemonic::Pcmpistri | Mnemonic::Pcmpistrm => {
            check_xmm_available(state)?;
            require_feature_ecx(cfg, cpuid_bits::LEAF1_ECX_SSE42)?;
            exec_pcmpxstri(state, bus, instr, next_ip)?;
            Ok(ExecOutcome::Continue)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

#[inline]
fn require_feature_edx(cfg: &Tier0Config, mask: u32) -> Result<(), Exception> {
    if (cfg.features.leaf1_edx & mask) == 0 {
        return Err(Exception::InvalidOpcode);
    }
    Ok(())
}

#[inline]
fn require_feature_ecx(cfg: &Tier0Config, mask: u32) -> Result<(), Exception> {
    if (cfg.features.leaf1_ecx & mask) == 0 {
        return Err(Exception::InvalidOpcode);
    }
    Ok(())
}

fn check_xmm_available(state: &mut CpuState) -> Result<(), Exception> {
    // Architectural SSE gating rules:
    // - CR0.EM => #UD
    // - CR4.OSFXSR => #UD
    // - CR0.TS => #NM
    if (state.control.cr0 & CR0_EM) != 0 {
        return Err(Exception::InvalidOpcode);
    }
    if (state.control.cr4 & CR4_OSFXSR) == 0 {
        return Err(Exception::InvalidOpcode);
    }
    if (state.control.cr0 & CR0_TS) != 0 {
        return Err(Exception::DeviceNotAvailable);
    }

    // If the OS hasn't opted into SIMD FP exception delivery, ensure all
    // exception masks remain set so we never need to inject #XM/#XF.
    if (state.control.cr4 & CR4_OSXMMEXCPT) == 0 {
        state.sse.mxcsr |= MXCSR_EXCEPTION_MASK;
    }
    Ok(())
}

fn xmm_index(reg: Register) -> Option<usize> {
    // `Register` also has a `None` variant, so avoid glob imports that would
    // shadow `Option::None`.
    Some(match reg {
        Register::XMM0 => 0,
        Register::XMM1 => 1,
        Register::XMM2 => 2,
        Register::XMM3 => 3,
        Register::XMM4 => 4,
        Register::XMM5 => 5,
        Register::XMM6 => 6,
        Register::XMM7 => 7,
        Register::XMM8 => 8,
        Register::XMM9 => 9,
        Register::XMM10 => 10,
        Register::XMM11 => 11,
        Register::XMM12 => 12,
        Register::XMM13 => 13,
        Register::XMM14 => 14,
        Register::XMM15 => 15,
        _ => return None,
    })
}

#[inline]
fn read_xmm_reg(state: &CpuState, reg: Register) -> Result<u128, Exception> {
    let idx = xmm_index(reg).ok_or(Exception::InvalidOpcode)?;
    Ok(state.sse.xmm[idx])
}

#[inline]
fn write_xmm_reg(state: &mut CpuState, reg: Register, val: u128) -> Result<(), Exception> {
    let idx = xmm_index(reg).ok_or(Exception::InvalidOpcode)?;
    state.sse.xmm[idx] = val;
    Ok(())
}

#[inline]
fn check_alignment(addr: u64, align: u64) -> Result<(), Exception> {
    if (addr & (align - 1)) != 0 {
        return Err(Exception::gp0());
    }
    Ok(())
}

fn read_xmm_operand_u128<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    next_ip: u64,
    align: Option<u64>,
) -> Result<u128, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => read_xmm_reg(state, instr.op_register(op as u32)),
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            if let Some(align) = align {
                check_alignment(addr, align)?;
            }
            bus.read_u128(addr)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn read_xmm_operand_u32<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    next_ip: u64,
) -> Result<u32, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => Ok(read_xmm_reg(state, instr.op_register(op as u32))? as u32),
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            Ok(bus.read_u32(addr)?)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn read_xmm_operand_u64<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    next_ip: u64,
) -> Result<u64, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => Ok(read_xmm_reg(state, instr.op_register(op as u32))? as u64),
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            Ok(bus.read_u64(addr)?)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

#[inline]
fn u128_set_low_u32_preserve(high: u128, low: u32) -> u128 {
    (high & !0xFFFF_FFFFu128) | (low as u128)
}

#[inline]
fn u128_set_low_u64_preserve(high: u128, low: u64) -> u128 {
    (high & !0xFFFF_FFFF_FFFF_FFFFu128) | (low as u128)
}

fn exec_mov128<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
    align: Option<u64>,
) -> Result<(), Exception> {
    match (instr.op_kind(0), instr.op_kind(1)) {
        (OpKind::Register, OpKind::Register | OpKind::Memory) => {
            let dst = instr.op0_register();
            let val = read_xmm_operand_u128(state, bus, instr, 1, next_ip, align)?;
            write_xmm_reg(state, dst, val)?;
            Ok(())
        }
        (OpKind::Memory, OpKind::Register) => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            if let Some(align) = align {
                check_alignment(addr, align)?;
            }
            let src = read_xmm_reg(state, instr.op1_register())?;
            bus.write_u128(addr, src)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_movss<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Register => {
            let dst_reg = instr.op0_register();
            let src_bits = read_xmm_operand_u32(state, bus, instr, 1, next_ip)?;
            let dst_old = read_xmm_reg(state, dst_reg)?;
            write_xmm_reg(state, dst_reg, u128_set_low_u32_preserve(dst_old, src_bits))?;
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            let src_reg = instr.op1_register();
            let v = read_xmm_reg(state, src_reg)? as u32;
            bus.write_u32(addr, v)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_movsd<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match instr.op_kind(0) {
        OpKind::Register => {
            let dst_reg = instr.op0_register();
            let src_bits = read_xmm_operand_u64(state, bus, instr, 1, next_ip)?;
            let dst_old = read_xmm_reg(state, dst_reg)?;
            write_xmm_reg(state, dst_reg, u128_set_low_u64_preserve(dst_old, src_bits))?;
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            let src_reg = instr.op1_register();
            let v = read_xmm_reg(state, src_reg)? as u64;
            bus.write_u64(addr, v)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_movd<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match (instr.op_kind(0), instr.op_kind(1)) {
        (OpKind::Register, _) if xmm_index(instr.op0_register()).is_some() => {
            let dst = instr.op0_register();
            let src = match instr.op_kind(1) {
                OpKind::Register => (state.read_reg(instr.op1_register()) & 0xFFFF_FFFF) as u32,
                OpKind::Memory => {
                    let addr = calc_ea(state, instr, next_ip, true)?;
                    bus.read_u32(addr)?
                }
                _ => return Err(Exception::InvalidOpcode),
            };
            write_xmm_reg(state, dst, src as u128)?;
            Ok(())
        }
        (_, OpKind::Register) if xmm_index(instr.op1_register()).is_some() => {
            let src = read_xmm_reg(state, instr.op1_register())? as u32;
            match instr.op_kind(0) {
                OpKind::Register => {
                    state.write_reg(instr.op0_register(), src as u64);
                    Ok(())
                }
                OpKind::Memory => {
                    let addr = calc_ea(state, instr, next_ip, true)?;
                    bus.write_u32(addr, src)
                }
                _ => Err(Exception::InvalidOpcode),
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn read_op_u64_allow_xmm<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    op: usize,
    next_ip: u64,
) -> Result<u64, Exception> {
    match instr.op_kind(op as u32) {
        OpKind::Register => {
            let reg = instr.op_register(op as u32);
            if xmm_index(reg).is_some() {
                Ok(read_xmm_reg(state, reg)? as u64)
            } else {
                Ok(state.read_reg(reg))
            }
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            Ok(bus.read_u64(addr)?)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_movq<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    match (instr.op_kind(0), instr.op_kind(1)) {
        (OpKind::Register, _) if xmm_index(instr.op0_register()).is_some() => {
            let dst = instr.op0_register();
            let src = read_op_u64_allow_xmm(state, bus, instr, 1, next_ip)?;
            write_xmm_reg(state, dst, src as u128)?;
            Ok(())
        }
        (_, OpKind::Register) if xmm_index(instr.op1_register()).is_some() => {
            let src = read_xmm_reg(state, instr.op1_register())? as u64;
            match instr.op_kind(0) {
                OpKind::Register => {
                    state.write_reg(instr.op0_register(), src);
                    Ok(())
                }
                OpKind::Memory => {
                    let addr = calc_ea(state, instr, next_ip, true)?;
                    bus.write_u64(addr, src)
                }
                _ => Err(Exception::InvalidOpcode),
            }
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_movhlps(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = instr.op1_register();
    let dst_old = read_xmm_reg(state, dst)?;
    let src_val = read_xmm_reg(state, src)?;
    let dst_high = (dst_old >> 64) as u64;
    let src_high = (src_val >> 64) as u64;
    write_xmm_reg(state, dst, ((dst_high as u128) << 64) | (src_high as u128))?;
    Ok(())
}

fn exec_movlhps(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = instr.op1_register();
    let dst_old = read_xmm_reg(state, dst)?;
    let src_val = read_xmm_reg(state, src)?;
    let dst_low = dst_old as u64;
    let src_low = src_val as u64;
    write_xmm_reg(state, dst, ((src_low as u128) << 64) | (dst_low as u128))?;
    Ok(())
}

fn u128_to_u32x4(v: u128) -> [u32; 4] {
    let bytes = v.to_le_bytes();
    let mut out = [0u32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = u32::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u32x4_to_u128(v: [u32; 4]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn u128_to_f32x4(v: u128) -> [f32; 4] {
    u128_to_u32x4(v).map(f32::from_bits)
}

fn f32x4_to_u128(v: [f32; 4]) -> u128 {
    u32x4_to_u128(v.map(f32::to_bits))
}

fn u128_to_u16x8(v: u128) -> [u16; 8] {
    let bytes = v.to_le_bytes();
    let mut out = [0u16; 8];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        out[i] = u16::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u16x8_to_u128(v: [u16; 8]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn u128_to_u64x2(v: u128) -> [u64; 2] {
    let bytes = v.to_le_bytes();
    let mut out = [0u64; 2];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        out[i] = u64::from_le_bytes(chunk.try_into().unwrap());
    }
    out
}

fn u64x2_to_u128(v: [u64; 2]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn u128_to_f64x2(v: u128) -> [f64; 2] {
    u128_to_u64x2(v).map(f64::from_bits)
}

fn f64x2_to_u128(v: [f64; 2]) -> u128 {
    u64x2_to_u128(v.map(f64::to_bits))
}

fn exec_unpcklps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let dst_val = read_xmm_reg(state, dst)?;
    let a = u128_to_u32x4(dst_val);
    let b = u128_to_u32x4(read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?);
    write_xmm_reg(state, dst, u32x4_to_u128([a[0], b[0], a[1], b[1]]))?;
    Ok(())
}

fn exec_unpckhps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let dst_val = read_xmm_reg(state, dst)?;
    let a = u128_to_u32x4(dst_val);
    let b = u128_to_u32x4(read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?);
    write_xmm_reg(state, dst, u32x4_to_u128([a[2], b[2], a[3], b[3]]))?;
    Ok(())
}

fn exec_shufps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let imm8 = instr.immediate8();
    let a = u128_to_u32x4(read_xmm_reg(state, dst)?);
    let b = u128_to_u32x4(read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?);
    let out = [
        a[(imm8 & 0b11) as usize],
        a[((imm8 >> 2) & 0b11) as usize],
        b[((imm8 >> 4) & 0b11) as usize],
        b[((imm8 >> 6) & 0b11) as usize],
    ];
    write_xmm_reg(state, dst, u32x4_to_u128(out))?;
    Ok(())
}

fn exec_logic_ps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_old = read_xmm_reg(state, dst)?;
    let res = match instr.mnemonic() {
        Mnemonic::Andps => dst_old & src,
        Mnemonic::Orps => dst_old | src,
        Mnemonic::Xorps => dst_old ^ src,
        Mnemonic::Andnps => (!dst_old) & src,
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_logic_pd<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_old = read_xmm_reg(state, dst)?;
    let res = match instr.mnemonic() {
        Mnemonic::Pand => dst_old & src,
        Mnemonic::Por => dst_old | src,
        Mnemonic::Pxor => dst_old ^ src,
        Mnemonic::Pandn => (!dst_old) & src,
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_sse2_int<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_old = read_xmm_reg(state, dst)?;

    let res = match instr.mnemonic() {
        Mnemonic::Paddb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = a[i].wrapping_add(b[i]);
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Paddw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = a[i].wrapping_add(b[i]);
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Paddd => {
            let a = u128_to_u32x4(dst_old);
            let b = u128_to_u32x4(src);
            u32x4_to_u128([
                a[0].wrapping_add(b[0]),
                a[1].wrapping_add(b[1]),
                a[2].wrapping_add(b[2]),
                a[3].wrapping_add(b[3]),
            ])
        }
        Mnemonic::Paddq => {
            let a = u128_to_u64x2(dst_old);
            let b = u128_to_u64x2(src);
            u64x2_to_u128([a[0].wrapping_add(b[0]), a[1].wrapping_add(b[1])])
        }
        Mnemonic::Psubb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = a[i].wrapping_sub(b[i]);
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Psubw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = a[i].wrapping_sub(b[i]);
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Psubd => {
            let a = u128_to_u32x4(dst_old);
            let b = u128_to_u32x4(src);
            u32x4_to_u128([
                a[0].wrapping_sub(b[0]),
                a[1].wrapping_sub(b[1]),
                a[2].wrapping_sub(b[2]),
                a[3].wrapping_sub(b[3]),
            ])
        }
        Mnemonic::Psubq => {
            let a = u128_to_u64x2(dst_old);
            let b = u128_to_u64x2(src);
            u64x2_to_u128([a[0].wrapping_sub(b[0]), a[1].wrapping_sub(b[1])])
        }
        Mnemonic::Paddsb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                let sum = (a[i] as i8) as i16 + (b[i] as i8) as i16;
                out[i] = sum.clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8;
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Paddusb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = a[i].saturating_add(b[i]);
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Psubsb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                let diff = (a[i] as i8 as i16) - (b[i] as i8 as i16);
                out[i] = diff.clamp(i8::MIN as i16, i8::MAX as i16) as i8 as u8;
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Psubusb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = a[i].saturating_sub(b[i]);
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Paddsw => {
            fn sat_add(x: i16, y: i16) -> i16 {
                let sum = x as i32 + y as i32;
                sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16
            }
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = sat_add(a[i] as i16, b[i] as i16) as u16;
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Paddusw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                let sum = (a[i] as u32) + (b[i] as u32);
                out[i] = sum.min(u16::MAX as u32) as u16;
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Psubsw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                let diff = (a[i] as i16 as i32) - (b[i] as i16 as i32);
                out[i] = diff.clamp(i16::MIN as i32, i16::MAX as i32) as i16 as u16;
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Psubusw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = a[i].saturating_sub(b[i]);
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Pcmpeqb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = if a[i] == b[i] { 0xFF } else { 0 };
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Pcmpeqw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = if a[i] == b[i] { 0xFFFF } else { 0 };
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Pcmpeqd => {
            let a = u128_to_u32x4(dst_old);
            let b = u128_to_u32x4(src);
            let mut out = [0u32; 4];
            for i in 0..4 {
                out[i] = if a[i] == b[i] { u32::MAX } else { 0 };
            }
            u32x4_to_u128(out)
        }
        Mnemonic::Pcmpgtb => {
            let a = dst_old.to_le_bytes();
            let b = src.to_le_bytes();
            let mut out = [0u8; 16];
            for i in 0..16 {
                out[i] = if (a[i] as i8) > (b[i] as i8) { 0xFF } else { 0 };
            }
            u128::from_le_bytes(out)
        }
        Mnemonic::Pcmpgtw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = if (a[i] as i16) > (b[i] as i16) { 0xFFFF } else { 0 };
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Pcmpgtd => {
            let a = u128_to_u32x4(dst_old);
            let b = u128_to_u32x4(src);
            let mut out = [0u32; 4];
            for i in 0..4 {
                out[i] = if (a[i] as i32) > (b[i] as i32) {
                    u32::MAX
                } else {
                    0
                };
            }
            u32x4_to_u128(out)
        }
        Mnemonic::Pmullw => {
            let a = u128_to_u16x8(dst_old);
            let b = u128_to_u16x8(src);
            let mut out = [0u16; 8];
            for i in 0..8 {
                let prod = (a[i] as i16 as i32) * (b[i] as i16 as i32);
                out[i] = (prod as i16) as u16;
            }
            u16x8_to_u128(out)
        }
        Mnemonic::Pmuludq => {
            let a = u128_to_u32x4(dst_old);
            let b = u128_to_u32x4(src);
            let lo = (a[0] as u64) * (b[0] as u64);
            let hi = (a[2] as u64) * (b[2] as u64);
            u64x2_to_u128([lo, hi])
        }
        Mnemonic::Pshufd => {
            let imm8 = instr.immediate8();
            let a = u128_to_u32x4(src);
            let out = [
                a[(imm8 & 0b11) as usize],
                a[((imm8 >> 2) & 0b11) as usize],
                a[((imm8 >> 4) & 0b11) as usize],
                a[((imm8 >> 6) & 0b11) as usize],
            ];
            u32x4_to_u128(out)
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_pcmpeqq<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_old = read_xmm_reg(state, dst)?;

    let a = u128_to_u64x2(dst_old);
    let b = u128_to_u64x2(src);
    let out = [
        if a[0] == b[0] { u64::MAX } else { 0 },
        if a[1] == b[1] { u64::MAX } else { 0 },
    ];
    write_xmm_reg(state, dst, u64x2_to_u128(out))?;
    Ok(())
}

fn exec_sse2_shift(state: &mut CpuState, instr: &Instruction) -> Result<(), Exception> {
    let dst = instr.op0_register();
    if instr.op_count() < 2 || instr.op_kind(1) != OpKind::Immediate8 {
        return Err(Exception::InvalidOpcode);
    }
    let imm8 = instr.immediate8();
    let dst_old = read_xmm_reg(state, dst)?;
    match instr.mnemonic() {
        Mnemonic::Psllw => {
            if imm8 > 15 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u16x8(dst_old);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = a[i].wrapping_shl(imm8 as u32);
            }
            write_xmm_reg(state, dst, u16x8_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Pslld => {
            if imm8 > 31 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u32x4(dst_old);
            let mut out = [0u32; 4];
            for i in 0..4 {
                out[i] = a[i].wrapping_shl(imm8 as u32);
            }
            write_xmm_reg(state, dst, u32x4_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Psllq => {
            if imm8 > 63 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u64x2(dst_old);
            write_xmm_reg(
                state,
                dst,
                u64x2_to_u128([
                    a[0].wrapping_shl(imm8 as u32),
                    a[1].wrapping_shl(imm8 as u32),
                ]),
            )?;
            Ok(())
        }
        Mnemonic::Psrlw => {
            if imm8 > 15 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u16x8(dst_old);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = a[i].wrapping_shr(imm8 as u32);
            }
            write_xmm_reg(state, dst, u16x8_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Psrld => {
            if imm8 > 31 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u32x4(dst_old);
            let mut out = [0u32; 4];
            for i in 0..4 {
                out[i] = a[i].wrapping_shr(imm8 as u32);
            }
            write_xmm_reg(state, dst, u32x4_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Psrlq => {
            if imm8 > 63 {
                write_xmm_reg(state, dst, 0)?;
                return Ok(());
            }
            let a = u128_to_u64x2(dst_old);
            write_xmm_reg(
                state,
                dst,
                u64x2_to_u128([
                    a[0].wrapping_shr(imm8 as u32),
                    a[1].wrapping_shr(imm8 as u32),
                ]),
            )?;
            Ok(())
        }
        Mnemonic::Psraw => {
            let count = imm8.min(15);
            let a = u128_to_u16x8(dst_old);
            let mut out = [0u16; 8];
            for i in 0..8 {
                out[i] = ((a[i] as i16) >> count) as u16;
            }
            write_xmm_reg(state, dst, u16x8_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Psrad => {
            let count = imm8.min(31);
            let a = u128_to_u32x4(dst_old);
            let mut out = [0u32; 4];
            for i in 0..4 {
                out[i] = ((a[i] as i32) >> count) as u32;
            }
            write_xmm_reg(state, dst, u32x4_to_u128(out))?;
            Ok(())
        }
        Mnemonic::Pslldq => {
            let res = if imm8 >= 16 {
                0u128
            } else {
                dst_old << ((imm8 as u32) * 8)
            };
            write_xmm_reg(state, dst, res)?;
            Ok(())
        }
        Mnemonic::Psrldq => {
            let res = if imm8 >= 16 {
                0u128
            } else {
                dst_old >> ((imm8 as u32) * 8)
            };
            write_xmm_reg(state, dst, res)?;
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_scalar_f32<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let a_bits = read_xmm_reg(state, dst)? as u32;
    let b_bits = read_xmm_operand_u32(state, bus, instr, 1, next_ip)?;
    let a = f32::from_bits(a_bits);
    let b = f32::from_bits(b_bits);
    let res = match instr.mnemonic() {
        Mnemonic::Addss => a + b,
        Mnemonic::Subss => a - b,
        Mnemonic::Mulss => a * b,
        Mnemonic::Divss => a / b,
        _ => return Err(Exception::InvalidOpcode),
    };
    let dst_old = read_xmm_reg(state, dst)?;
    write_xmm_reg(
        state,
        dst,
        u128_set_low_u32_preserve(dst_old, res.to_bits()),
    )?;
    Ok(())
}

fn exec_scalar_f64<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
    op: impl FnOnce(f64, f64) -> f64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let a_bits = read_xmm_reg(state, dst)? as u64;
    let b_bits = read_xmm_operand_u64(state, bus, instr, 1, next_ip)?;
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let res = op(a, b);
    let dst_old = read_xmm_reg(state, dst)?;
    write_xmm_reg(
        state,
        dst,
        u128_set_low_u64_preserve(dst_old, res.to_bits()),
    )?;
    Ok(())
}

fn exec_packed_f32<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let a = u128_to_f32x4(read_xmm_reg(state, dst)?);
    let b = u128_to_f32x4(read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?);
    let mut out = [0f32; 4];
    for i in 0..4 {
        out[i] = match instr.mnemonic() {
            Mnemonic::Addps => a[i] + b[i],
            Mnemonic::Subps => a[i] - b[i],
            Mnemonic::Mulps => a[i] * b[i],
            Mnemonic::Divps => a[i] / b[i],
            _ => return Err(Exception::InvalidOpcode),
        };
    }
    write_xmm_reg(state, dst, f32x4_to_u128(out))?;
    Ok(())
}

fn exec_packed_f64<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let a = u128_to_f64x2(read_xmm_reg(state, dst)?);
    let b = u128_to_f64x2(read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?);
    let out = match instr.mnemonic() {
        Mnemonic::Addpd => [a[0] + b[0], a[1] + b[1]],
        Mnemonic::Subpd => [a[0] - b[0], a[1] - b[1]],
        Mnemonic::Mulpd => [a[0] * b[0], a[1] * b[1]],
        Mnemonic::Divpd => [a[0] / b[0], a[1] / b[1]],
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, f64x2_to_u128(out))?;
    Ok(())
}

fn apply_rounding_mode_f64(val: f64, mode: RoundingMode) -> f64 {
    match mode {
        RoundingMode::Nearest => val.round_ties_even(),
        RoundingMode::Down => val.floor(),
        RoundingMode::Up => val.ceil(),
        RoundingMode::TowardZero => val.trunc(),
    }
}

fn or_mxcsr_flags(state: &mut CpuState, flags: u32) {
    state.sse.mxcsr |= flags;
}

fn sign_extend_i64(raw: u64, bits: u32) -> Result<i64, Exception> {
    match bits {
        8 => Ok((raw as u8 as i8) as i64),
        16 => Ok((raw as u16 as i16) as i64),
        32 => Ok((raw as u32 as i32) as i64),
        64 => Ok(raw as i64),
        _ => Err(Exception::InvalidOpcode),
    }
}

fn cvt_i64_to_f32(state: &mut CpuState, src: i64) -> f32 {
    if src == 0 {
        return 0.0;
    }

    let sign = src < 0;
    let mag: u64 = if sign {
        src.wrapping_neg() as u64
    } else {
        src as u64
    };

    let msb = 63 - mag.leading_zeros();
    let mut exp = (msb as i32) + 127;
    let shift = (msb as i32) - 23;

    let (mut mantissa_full, rem) = if shift <= 0 {
        ((mag << (-shift as u32)) as u64, 0u64)
    } else {
        let shift = shift as u32;
        ((mag >> shift) as u64, mag & ((1u64 << shift) - 1))
    };

    if rem != 0 {
        or_mxcsr_flags(state, MXCSR_PE);
    }

    let inc = if rem == 0 {
        false
    } else {
        match rounding_mode(state.sse.mxcsr) {
            RoundingMode::Nearest => {
                let shift = shift as u32;
                let half = 1u64 << (shift - 1);
                rem > half || (rem == half && (mantissa_full & 1) == 1)
            }
            RoundingMode::Down => sign,
            RoundingMode::Up => !sign,
            RoundingMode::TowardZero => false,
        }
    };

    if inc {
        mantissa_full = mantissa_full.wrapping_add(1);
        if mantissa_full == (1u64 << 24) {
            mantissa_full >>= 1;
            exp += 1;
        }
    }

    let mantissa = (mantissa_full & ((1u64 << 23) - 1)) as u32;
    let bits = ((sign as u32) << 31) | ((exp as u32) << 23) | mantissa;
    f32::from_bits(bits)
}

fn cvt_i64_to_f64(state: &mut CpuState, src: i64) -> f64 {
    if src == 0 {
        return 0.0;
    }

    let sign = src < 0;
    let mag: u64 = if sign {
        src.wrapping_neg() as u64
    } else {
        src as u64
    };

    let msb = 63 - mag.leading_zeros();
    let mut exp = (msb as i32) + 1023;
    let shift = (msb as i32) - 52;

    let (mut mantissa_full, rem) = if shift <= 0 {
        ((mag << (-shift as u32)) as u64, 0u64)
    } else {
        let shift = shift as u32;
        ((mag >> shift) as u64, mag & ((1u64 << shift) - 1))
    };

    if rem != 0 {
        or_mxcsr_flags(state, MXCSR_PE);
    }

    let inc = if rem == 0 {
        false
    } else {
        match rounding_mode(state.sse.mxcsr) {
            RoundingMode::Nearest => {
                let shift = shift as u32;
                let half = 1u64 << (shift - 1);
                rem > half || (rem == half && (mantissa_full & 1) == 1)
            }
            RoundingMode::Down => sign,
            RoundingMode::Up => !sign,
            RoundingMode::TowardZero => false,
        }
    };

    if inc {
        mantissa_full = mantissa_full.wrapping_add(1);
        if mantissa_full == (1u64 << 53) {
            mantissa_full >>= 1;
            exp += 1;
        }
    }

    let mantissa = mantissa_full & ((1u64 << 52) - 1);
    let bits = ((sign as u64) << 63) | ((exp as u64) << 52) | mantissa;
    f64::from_bits(bits)
}

fn cvt_float_to_i32(state: &mut CpuState, val: f64, truncate: bool) -> i32 {
    if !val.is_finite() || val.is_nan() {
        or_mxcsr_flags(state, MXCSR_IE);
        return i32::MIN;
    }

    let rounded = if truncate {
        val.trunc()
    } else {
        apply_rounding_mode_f64(val, rounding_mode(state.sse.mxcsr))
    };

    if rounded < (i32::MIN as f64) || rounded > (i32::MAX as f64) {
        or_mxcsr_flags(state, MXCSR_IE);
        return i32::MIN;
    }

    if val != rounded {
        or_mxcsr_flags(state, MXCSR_PE);
    }

    rounded as i32
}

fn cvt_float_to_i64(state: &mut CpuState, val: f64, truncate: bool) -> i64 {
    if !val.is_finite() || val.is_nan() {
        or_mxcsr_flags(state, MXCSR_IE);
        return i64::MIN;
    }

    let rounded = if truncate {
        val.trunc()
    } else {
        apply_rounding_mode_f64(val, rounding_mode(state.sse.mxcsr))
    };

    if rounded < (i64::MIN as f64) || rounded > (i64::MAX as f64) {
        or_mxcsr_flags(state, MXCSR_IE);
        return i64::MIN;
    }

    if val != rounded {
        or_mxcsr_flags(state, MXCSR_PE);
    }

    rounded as i64
}

fn exec_cvtsi2ss<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Register || xmm_index(instr.op0_register()).is_none() {
        return Err(Exception::InvalidOpcode);
    }
    let dst = instr.op0_register();
    let src_bits = op_bits(state, instr, 1)?;
    let raw = read_op_sized(state, bus, instr, 1, src_bits, next_ip)?;
    let src = sign_extend_i64(raw, src_bits)?;
    let f = cvt_i64_to_f32(state, src);
    let dst_old = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, u128_set_low_u32_preserve(dst_old, f.to_bits()))?;
    Ok(())
}

fn exec_cvtsi2sd<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(0) != OpKind::Register || xmm_index(instr.op0_register()).is_none() {
        return Err(Exception::InvalidOpcode);
    }
    let dst = instr.op0_register();
    let src_bits = op_bits(state, instr, 1)?;
    let raw = read_op_sized(state, bus, instr, 1, src_bits, next_ip)?;
    let src = sign_extend_i64(raw, src_bits)?;
    let f = cvt_i64_to_f64(state, src);
    let dst_old = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, u128_set_low_u64_preserve(dst_old, f.to_bits()))?;
    Ok(())
}

fn exec_cvtss2si<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let truncate = instr.mnemonic() == Mnemonic::Cvttss2si;
    let dst_reg = instr.op0_register();
    let dst_bits = reg_bits(dst_reg)?;
    let bits = read_xmm_operand_u32(state, bus, instr, 1, next_ip)?;
    let val = f32::from_bits(bits) as f64;

    match dst_bits {
        32 => {
            let res = cvt_float_to_i32(state, val, truncate);
            state.write_reg(dst_reg, res as u32 as u64);
        }
        64 => {
            let res = cvt_float_to_i64(state, val, truncate);
            state.write_reg(dst_reg, res as u64);
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_cvtsd2si<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let truncate = instr.mnemonic() == Mnemonic::Cvttsd2si;
    let dst_reg = instr.op0_register();
    let dst_bits = reg_bits(dst_reg)?;
    let bits = read_xmm_operand_u64(state, bus, instr, 1, next_ip)?;
    let val = f64::from_bits(bits);
    match dst_bits {
        32 => {
            let res = cvt_float_to_i32(state, val, truncate);
            state.write_reg(dst_reg, res as u32 as u64);
        }
        64 => {
            let res = cvt_float_to_i64(state, val, truncate);
            state.write_reg(dst_reg, res as u64);
        }
        _ => return Err(Exception::InvalidOpcode),
    }
    Ok(())
}

fn exec_pshufb<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, ssse3::pshufb(dst_val, src))?;
    Ok(())
}

fn exec_lddqu<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    if instr.op_kind(1) != OpKind::Memory {
        return Err(Exception::InvalidOpcode);
    }
    let dst = instr.op0_register();
    let addr = calc_ea(state, instr, next_ip, true)?;
    let val = bus.read_u128(addr)?;
    write_xmm_reg(state, dst, val)?;
    Ok(())
}

fn exec_haddps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, sse3::haddps(dst_val, src))?;
    Ok(())
}

fn exec_haddpd<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, sse3::haddpd(dst_val, src))?;
    Ok(())
}

fn exec_hsub<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    let res = match instr.mnemonic() {
        Mnemonic::Hsubps => sse3::hsubps(dst_val, src),
        Mnemonic::Hsubpd => sse3::hsubpd(dst_val, src),
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_movddup<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = match instr.op_kind(1) {
        OpKind::Register => read_xmm_reg(state, instr.op1_register())?,
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            bus.read_u64(addr)? as u128
        }
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, sse3::movddup(src))?;
    Ok(())
}

fn exec_movdup_ps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let res = match instr.mnemonic() {
        Mnemonic::Movsldup => sse3::movsldup(src),
        Mnemonic::Movshdup => sse3::movshdup(src),
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_ssse3_binop<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    let res = match instr.mnemonic() {
        Mnemonic::Phaddw => ssse3::phaddw(dst_val, src),
        Mnemonic::Phaddd => ssse3::phaddd(dst_val, src),
        Mnemonic::Phaddsw => ssse3::phaddsw(dst_val, src),
        Mnemonic::Pmaddubsw => ssse3::pmaddubsw(dst_val, src),
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_ssse3_abs<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let res = match instr.mnemonic() {
        Mnemonic::Pabsb => ssse3::pabsb(src),
        Mnemonic::Pabsw => ssse3::pabsw(src),
        Mnemonic::Pabsd => ssse3::pabsd(src),
        _ => return Err(Exception::InvalidOpcode),
    };
    write_xmm_reg(state, dst, res)?;
    Ok(())
}

fn exec_palignr<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let imm8 = instr.immediate8();
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, ssse3::palignr(dst_val, src, imm8))?;
    Ok(())
}

fn exec_pblendw<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let imm8 = instr.immediate8();
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, sse41::pblendw(dst_val, src, imm8))?;
    Ok(())
}

fn exec_ptest<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let a = instr.op0_register();
    let b = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let a_val = read_xmm_reg(state, a)?;
    let (zf, cf) = sse41::ptest(a_val, b);
    state.set_flag(FLAG_ZF, zf);
    state.set_flag(FLAG_CF, cf);
    state.set_flag(FLAG_OF, false);
    state.set_flag(FLAG_SF, false);
    Ok(())
}

fn exec_pmovx<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();

    let out = match instr.mnemonic() {
        Mnemonic::Pmovsxbw => {
            let mut buf = [0u8; 8];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovsxbw(&buf)
        }
        Mnemonic::Pmovsxbd => {
            let mut buf = [0u8; 4];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovsxbd(&buf)
        }
        Mnemonic::Pmovsxbq => {
            let mut buf = [0u8; 2];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovsxbq(&buf)
        }
        Mnemonic::Pmovzxbw => {
            let mut buf = [0u8; 8];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovzxbw(&buf)
        }
        Mnemonic::Pmovzxbd => {
            let mut buf = [0u8; 4];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovzxbd(&buf)
        }
        Mnemonic::Pmovzxbq => {
            let mut buf = [0u8; 2];
            read_pmov_src(state, bus, instr, next_ip, &mut buf)?;
            sse41::pmovzxbq(&buf)
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    write_xmm_reg(state, dst, out)?;
    Ok(())
}

fn read_pmov_src<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
    dst: &mut [u8],
) -> Result<(), Exception> {
    match instr.op_kind(1) {
        OpKind::Register => {
            let src = read_xmm_reg(state, instr.op1_register())?.to_le_bytes();
            dst.copy_from_slice(&src[..dst.len()]);
            Ok(())
        }
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            bus.read_bytes(addr, dst)
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_insertps<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let imm8 = instr.immediate8();
    let src = match instr.op_kind(1) {
        OpKind::Register => read_xmm_reg(state, instr.op1_register())?,
        OpKind::Memory => {
            let addr = calc_ea(state, instr, next_ip, true)?;
            bus.read_u32(addr)? as u128
        }
        _ => return Err(Exception::InvalidOpcode),
    };
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, sse41::insertps(dst_val, src, imm8))?;
    Ok(())
}

fn exec_pmulld<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let src = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let dst_val = read_xmm_reg(state, dst)?;
    write_xmm_reg(state, dst, sse41::pmulld(dst_val, src))?;
    Ok(())
}

fn exec_pcmpxstri<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let a = instr.op0_register();
    let b = read_xmm_operand_u128(state, bus, instr, 1, next_ip, None)?;
    let imm = instr.immediate8();
    let a_val = read_xmm_reg(state, a)?;

    match instr.mnemonic() {
        Mnemonic::Pcmpestri => {
            let (index, flags) = sse42::pcmpe_stri(
                a_val,
                b,
                imm,
                state.read_reg(Register::EAX) as u32,
                state.read_reg(Register::EDX) as u32,
            );
            state.write_reg(Register::ECX, index as u64);
            set_pcmp_flags(state, flags);
        }
        Mnemonic::Pcmpestrm => {
            let (mask, flags) = sse42::pcmpe_strm(
                a_val,
                b,
                imm,
                state.read_reg(Register::EAX) as u32,
                state.read_reg(Register::EDX) as u32,
            );
            state.sse.xmm[0] = mask;
            set_pcmp_flags(state, flags);
        }
        Mnemonic::Pcmpistri => {
            let (index, flags) = sse42::pcmpi_stri(a_val, b, imm);
            state.write_reg(Register::ECX, index as u64);
            set_pcmp_flags(state, flags);
        }
        Mnemonic::Pcmpistrm => {
            let (mask, flags) = sse42::pcmpi_strm(a_val, b, imm);
            state.sse.xmm[0] = mask;
            set_pcmp_flags(state, flags);
        }
        _ => return Err(Exception::InvalidOpcode),
    }

    Ok(())
}

fn set_pcmp_flags(state: &mut CpuState, flags: sse42::PcmpFlags) {
    state.set_flag(FLAG_CF, flags.cf);
    state.set_flag(FLAG_ZF, flags.zf);
    state.set_flag(FLAG_SF, flags.sf);
    state.set_flag(FLAG_OF, flags.of);
}

fn exec_popcnt<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let width_bits = op_bits(state, instr, 0)?;
    let src = read_op_sized(state, bus, instr, 1, width_bits, next_ip)?;
    let masked = src & mask_bits(width_bits);
    let res = match width_bits {
        16 => (masked as u16).count_ones(),
        32 => (masked as u32).count_ones(),
        64 => (masked as u64).count_ones(),
        _ => return Err(Exception::InvalidOpcode),
    } as u64;

    // POPCNT updates ZF and clears CF/OF; other flags are architecturally undefined.
    state.set_flag(FLAG_ZF, res == 0);
    state.set_flag(FLAG_CF, false);
    state.set_flag(FLAG_OF, false);
    state.set_flag(FLAG_SF, false);

    match instr.op_kind(0) {
        OpKind::Register => {
            state.write_reg(instr.op0_register(), res);
            Ok(())
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

fn exec_crc32<B: CpuBus>(
    state: &mut CpuState,
    bus: &mut B,
    instr: &Instruction,
    next_ip: u64,
) -> Result<(), Exception> {
    let dst = instr.op0_register();
    let seed = state.read_reg(dst) as u32;

    let src_bits = op_bits(state, instr, 1)?;
    let res = match src_bits {
        8 => {
            let v = read_op_sized(state, bus, instr, 1, 8, next_ip)? as u8;
            sse42::crc32_u8(seed, v)
        }
        16 => {
            let v = read_op_sized(state, bus, instr, 1, 16, next_ip)? as u16;
            sse42::crc32_u16(seed, v)
        }
        32 => {
            let v = read_op_sized(state, bus, instr, 1, 32, next_ip)? as u32;
            sse42::crc32_u32(seed, v)
        }
        64 => {
            let v = read_op_sized(state, bus, instr, 1, 64, next_ip)?;
            sse42::crc32_u64(seed, v)
        }
        _ => return Err(Exception::InvalidOpcode),
    };

    // CRC32 always produces a 32-bit CRC which is zero-extended into the destination.
    state.write_reg(dst, res as u64);
    Ok(())
}
