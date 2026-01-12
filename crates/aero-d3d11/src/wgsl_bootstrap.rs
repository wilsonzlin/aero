//! Bootstrap SM4/SM5 → WGSL translator.
//!
//! This module exists solely to keep early `aerogpu_cmd`-style executors running
//! while the real SM4/SM5 decoder (Task 454) is still under development.
//!
//! It supports only a tiny subset of SM4/SM5:
//! - `mov` between input/output registers
//! - `ret`
//! - ignores `nop` and comment custom-data blocks
//!
//! New code should prefer [`crate::shader_translate`] + a proper SM4/SM5 IR
//! decoder.

use core::fmt;

use crate::sm4::opcode::{
    OPCODE_CUSTOMDATA, OPCODE_EXTENDED_BIT, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK,
    OPCODE_MOV, OPCODE_NOP, OPCODE_RET, OPERAND_EXTENDED_BIT,
};
use crate::sm4::{ShaderStage, Sm4Program};

#[derive(Debug, Clone)]
pub struct WgslBootstrapTranslation {
    pub wgsl: String,
}

#[derive(Debug)]
pub enum WgslBootstrapError {
    UnsupportedStage(ShaderStage),
    UnexpectedTokenStream(&'static str),
    UnsupportedInstruction { opcode: u32 },
    BadInstructionLength { opcode: u32, len: usize },
    OperandOutOfBounds,
    UnsupportedOperand(&'static str),
}

impl fmt::Display for WgslBootstrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WgslBootstrapError::UnsupportedStage(stage) => {
                write!(f, "unsupported shader stage {stage:?}")
            }
            WgslBootstrapError::UnexpectedTokenStream(msg) => {
                write!(f, "unexpected SM4/5 token stream: {msg}")
            }
            WgslBootstrapError::UnsupportedInstruction { opcode } => {
                write!(f, "unsupported SM4/5 instruction opcode {opcode}")
            }
            WgslBootstrapError::BadInstructionLength { opcode, len } => write!(
                f,
                "unexpected SM4/5 instruction length {len} for opcode {opcode}"
            ),
            WgslBootstrapError::OperandOutOfBounds => {
                write!(f, "operand token stream out of bounds")
            }
            WgslBootstrapError::UnsupportedOperand(msg) => {
                write!(f, "unsupported operand encoding: {msg}")
            }
        }
    }
}

impl std::error::Error for WgslBootstrapError {}

pub fn translate_sm4_to_wgsl_bootstrap(
    program: &Sm4Program,
) -> Result<WgslBootstrapTranslation, WgslBootstrapError> {
    match program.stage {
        ShaderStage::Vertex => translate_vs(program),
        ShaderStage::Pixel => translate_ps(program),
        other => Err(WgslBootstrapError::UnsupportedStage(other)),
    }
}

fn translate_vs(program: &Sm4Program) -> Result<WgslBootstrapTranslation, WgslBootstrapError> {
    let movs = extract_movs(program)?;
    let max_in = movs
        .iter()
        .filter_map(|m| match m.src.kind {
            RegKind::Input => Some(m.src.index),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let max_out = movs
        .iter()
        .filter_map(|m| match m.dst.kind {
            RegKind::Output => Some(m.dst.index),
            _ => None,
        })
        .max()
        .unwrap_or(0);

    let mut s = String::new();
    s.push_str("struct VsIn {\n");
    for idx in 0..=max_in {
        s.push_str(&format!("  @location({idx}) v{idx}: vec4<f32>,\n"));
    }
    s.push_str("};\n\n");

    s.push_str("struct VsOut {\n");
    s.push_str("  @builtin(position) pos: vec4<f32>,\n");
    for idx in 1..=max_out {
        // Use the D3D output register index as the WGSL location. This matches the
        // signature-driven translator and avoids mismatches when mixing bootstrap and
        // signature-driven shaders in the same pipeline.
        s.push_str(&format!("  @location({idx}) o{idx}: vec4<f32>,\n"));
    }
    s.push_str("};\n\n");

    s.push_str("@vertex\nfn vs_main(input: VsIn) -> VsOut {\n");
    s.push_str("  var out: VsOut;\n");

    for mov in movs {
        match (mov.dst.kind, mov.src.kind) {
            (RegKind::Output, RegKind::Input) => {
                if mov.dst.index == 0 {
                    s.push_str(&format!("  out.pos = input.v{};\n", mov.src.index));
                } else {
                    s.push_str(&format!(
                        "  out.o{} = input.v{};\n",
                        mov.dst.index, mov.src.index
                    ));
                }
            }
            _ => {
                return Err(WgslBootstrapError::UnsupportedOperand(
                    "expected output<-input mov",
                ))
            }
        }
    }

    s.push_str("  return out;\n}\n");

    Ok(WgslBootstrapTranslation { wgsl: s })
}

fn translate_ps(program: &Sm4Program) -> Result<WgslBootstrapTranslation, WgslBootstrapError> {
    let movs = extract_movs(program)?;

    let mov = movs
        .into_iter()
        .find(|m| matches!(m.dst.kind, RegKind::Output) && m.dst.index == 0)
        .ok_or(WgslBootstrapError::UnexpectedTokenStream(
            "pixel shader missing mov to o0",
        ))?;

    let max_in = match mov.src.kind {
        RegKind::Input => mov.src.index,
        _ => {
            return Err(WgslBootstrapError::UnsupportedOperand(
                "expected o0<-input mov",
            ))
        }
    };

    let mut s = String::new();
    s.push_str("struct PsIn {\n");
    s.push_str("  @builtin(position) pos: vec4<f32>,\n");
    for idx in 1..=max_in {
        // Mirror the vertex stage: interpolants are at `@location(v#)` (not shifted down).
        s.push_str(&format!("  @location({idx}) v{idx}: vec4<f32>,\n"));
    }
    s.push_str("};\n\n");

    s.push_str("@fragment\nfn fs_main(input: PsIn) -> @location(0) vec4<f32> {\n");
    if mov.src.index == 0 {
        // The bootstrap translator assumes v0 is the pixel shader's position input. For the
        // common debug pattern `mov o0, v0`, return the builtin `position` value.
        s.push_str("  return input.pos;\n");
    } else {
        s.push_str(&format!("  return input.v{};\n", mov.src.index));
    }
    s.push_str("}\n");

    Ok(WgslBootstrapTranslation { wgsl: s })
}

#[derive(Debug, Clone, Copy)]
enum RegKind {
    Input,
    Output,
    Temp,
    Other(#[allow(dead_code)] u32),
}

#[derive(Debug, Clone, Copy)]
struct RegRef {
    kind: RegKind,
    index: u32,
}

#[derive(Debug, Clone, Copy)]
struct Mov {
    dst: RegRef,
    src: RegRef,
}

fn extract_movs(program: &Sm4Program) -> Result<Vec<Mov>, WgslBootstrapError> {
    let declared_len = program.tokens.get(1).copied().unwrap_or(0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        return Err(WgslBootstrapError::UnexpectedTokenStream(
            "declared token length out of bounds",
        ));
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2;
    let mut movs = Vec::new();
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        if len == 0 {
            return Err(WgslBootstrapError::UnexpectedTokenStream(
                "instruction length cannot be zero",
            ));
        }
        if i + len > toks.len() {
            return Err(WgslBootstrapError::UnexpectedTokenStream(
                "instruction overruns declared token stream",
            ));
        }

        let inst_tokens = &toks[i..i + len];

        // NOTE: opcode numeric ranges:
        // - executable instructions: < 0x100
        // - declarations: >= 0x100
        const DECL_OPCODE_MIN: u32 = 0x100;
        const CUSTOMDATA_CLASS_COMMENT: u32 = 0;

        match opcode {
            OPCODE_NOP => {
                // Ignore.
            }
            OPCODE_CUSTOMDATA => {
                // Ignore comment blocks; other custom-data classes are not supported by the
                // bootstrap translator because they can affect shader semantics (e.g. immediate
                // constant buffers).
                if inst_tokens.get(1).copied() == Some(CUSTOMDATA_CLASS_COMMENT) {
                    // Ignore.
                } else {
                    return Err(WgslBootstrapError::UnsupportedInstruction { opcode });
                }
            }
            OPCODE_MOV => {
                let mut cursor = 1usize;
                let mut has_extended = (opcode_token & OPCODE_EXTENDED_BIT) != 0;
                while has_extended {
                    if cursor >= inst_tokens.len() {
                        return Err(WgslBootstrapError::BadInstructionLength { opcode, len });
                    }
                    let ext = inst_tokens[cursor];
                    cursor += 1;
                    has_extended = (ext & OPCODE_EXTENDED_BIT) != 0;
                }

                if inst_tokens.len() < cursor + 4 {
                    return Err(WgslBootstrapError::BadInstructionLength { opcode, len });
                }
                let dst = parse_reg_operand(&inst_tokens[cursor..cursor + 2])?;
                let src = parse_reg_operand(&inst_tokens[cursor + 2..cursor + 4])?;
                movs.push(Mov { dst, src });
            }
            OPCODE_RET => {
                break;
            }
            _ => {
                // Ignore declarations, but fail on unsupported executable instructions so we don't
                // silently generate incorrect shaders.
                if opcode < DECL_OPCODE_MIN {
                    return Err(WgslBootstrapError::UnsupportedInstruction { opcode });
                }
            }
        }

        i += len;
    }

    Ok(movs)
}

fn parse_reg_operand(tokens: &[u32]) -> Result<RegRef, WgslBootstrapError> {
    if tokens.len() != 2 {
        return Err(WgslBootstrapError::UnsupportedOperand(
            "operand must be 2 dwords",
        ));
    }
    let token = tokens[0];
    if (token & OPERAND_EXTENDED_BIT) != 0 {
        return Err(WgslBootstrapError::UnsupportedOperand(
            "extended operand token",
        ));
    }

    let ty = (token >> 4) & 0xff;
    let kind = match ty {
        0 => RegKind::Temp,
        1 => RegKind::Input,
        2 => RegKind::Output,
        other => RegKind::Other(other),
    };

    Ok(RegRef {
        kind,
        index: tokens[1],
    })
}
