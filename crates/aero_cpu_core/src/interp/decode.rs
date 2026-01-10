use crate::cpu::CpuMode;
use crate::interp::{DecodedInst, ExecError, InstKind};

use super::string::{DecodedStringInst, RepPrefix, StringOp};
use crate::cpu::Segment;

#[derive(Clone, Copy, Debug, Default)]
pub struct PrefixState {
    pub rep: RepPrefix,
    pub operand_size_override: bool,
    pub address_size_override: bool,
    pub segment_override: Option<Segment>,
    pub rex_w: bool,
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
                p.rex_w = (b & 0x08) != 0;
                idx += 1;
            }
        }
    }

    let opcode = *bytes.get(idx).ok_or(ExecError::TruncatedInstruction)?;
    idx += 1;

    let string = match opcode {
        0xA4 => DecodedStringInst::new(StringOp::Movs, 1, p),
        0xA5 => DecodedStringInst::new(StringOp::Movs, element_size_non_byte(mode, &p)?, p),
        0xAA => DecodedStringInst::new(StringOp::Stos, 1, p),
        0xAB => DecodedStringInst::new(StringOp::Stos, element_size_non_byte(mode, &p)?, p),
        0xAC => DecodedStringInst::new(StringOp::Lods, 1, p),
        0xAD => DecodedStringInst::new(StringOp::Lods, element_size_non_byte(mode, &p)?, p),
        0xA6 => DecodedStringInst::new(StringOp::Cmps, 1, p),
        0xA7 => DecodedStringInst::new(StringOp::Cmps, element_size_non_byte(mode, &p)?, p),
        0xAE => DecodedStringInst::new(StringOp::Scas, 1, p),
        0xAF => DecodedStringInst::new(StringOp::Scas, element_size_non_byte(mode, &p)?, p),
        other => return Err(ExecError::InvalidOpcode(other)),
    };

    Ok(DecodedInst {
        len: idx,
        kind: InstKind::String(string),
    })
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
            if p.rex_w {
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

