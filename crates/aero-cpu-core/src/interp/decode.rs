use crate::cpu::CpuMode;
use crate::interp::{DecodedInst, ExecError, InstKind};

use super::string::{DecodedStringInst, RepPrefix, StringOp};
use crate::cpu::Segment;

#[derive(Clone, Copy, Debug, Default)]
pub struct RexPrefix {
    pub present: bool,
    pub w: bool,
    pub r: bool,
    pub x: bool,
    pub b: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PrefixState {
    pub rep: RepPrefix,
    pub lock: bool,
    pub operand_size_override: bool,
    pub address_size_override: bool,
    pub segment_override: Option<Segment>,
    pub rex: RexPrefix,
}

fn is_segment_override(byte: u8) -> Option<Segment> {
    Some(match byte {
        0x2E => Segment::Cs,
        0x36 => Segment::Ss,
        0x3E => Segment::Ds,
        0x26 => Segment::Es,
        0x64 => Segment::Fs,
        0x65 => Segment::Gs,
        _ => return None,
    })
}

pub fn decode(mode: CpuMode, bytes: &[u8]) -> Result<DecodedInst, ExecError> {
    let mut idx = 0usize;
    let mut p = PrefixState::default();

    // Parse legacy prefixes. We only care about those that affect string ops.
    loop {
        let b = *bytes.get(idx).ok_or(ExecError::TruncatedInstruction)?;
        let mut consumed = true;
        match b {
            0xF3 => p.rep = RepPrefix::F3,
            0xF2 => p.rep = RepPrefix::F2,
            0xF0 => p.lock = true,
            0x66 => p.operand_size_override = true,
            0x67 => p.address_size_override = true,
            _ => {
                if let Some(seg) = is_segment_override(b) {
                    p.segment_override = Some(seg);
                } else {
                    consumed = false;
                }
            }
        }

        if !consumed {
            break;
        }
        idx += 1;
        if idx >= 15 {
            break;
        }
    }

    // Parse REX (64-bit mode only).
    if mode == CpuMode::Long64 {
        if let Some(&b) = bytes.get(idx) {
            if (0x40..=0x4F).contains(&b) {
                p.rex = RexPrefix {
                    present: true,
                    w: (b & 0x08) != 0,
                    r: (b & 0x04) != 0,
                    x: (b & 0x02) != 0,
                    b: (b & 0x01) != 0,
                };
                idx += 1;
            }
        }
    }

    let opcode = *bytes.get(idx).ok_or(ExecError::TruncatedInstruction)?;
    idx += 1;

    let kind = match opcode {
        0xA4 => InstKind::String(DecodedStringInst::new(StringOp::Movs, 1, p)),
        0xA5 => InstKind::String(DecodedStringInst::new(
            StringOp::Movs,
            element_size_non_byte(mode, &p)?,
            p,
        )),
        0xAA => InstKind::String(DecodedStringInst::new(StringOp::Stos, 1, p)),
        0xAB => InstKind::String(DecodedStringInst::new(
            StringOp::Stos,
            element_size_non_byte(mode, &p)?,
            p,
        )),
        0xAC => InstKind::String(DecodedStringInst::new(StringOp::Lods, 1, p)),
        0xAD => InstKind::String(DecodedStringInst::new(
            StringOp::Lods,
            element_size_non_byte(mode, &p)?,
            p,
        )),
        0xA6 => InstKind::String(DecodedStringInst::new(StringOp::Cmps, 1, p)),
        0xA7 => InstKind::String(DecodedStringInst::new(
            StringOp::Cmps,
            element_size_non_byte(mode, &p)?,
            p,
        )),
        0xAE => InstKind::String(DecodedStringInst::new(StringOp::Scas, 1, p)),
        0xAF => InstKind::String(DecodedStringInst::new(
            StringOp::Scas,
            element_size_non_byte(mode, &p)?,
            p,
        )),
        other => {
            return super::atomics::decode_atomics(mode, bytes, idx, other, p);
        }
    };

    Ok(DecodedInst { len: idx, kind })
}

fn element_size_non_byte(mode: CpuMode, p: &PrefixState) -> Result<usize, ExecError> {
    // For opcodes A5/AB/AD/A7/AF: operand size selects W/D/Q.
    let bits = match mode {
        CpuMode::Real16 => {
            if p.operand_size_override {
                32
            } else {
                16
            }
        }
        CpuMode::Protected32 => {
            if p.operand_size_override {
                16
            } else {
                32
            }
        }
        CpuMode::Long64 => {
            if p.rex.w {
                64
            } else if p.operand_size_override {
                16
            } else {
                32
            }
        }
    };

    Ok((bits / 8) as usize)
}
