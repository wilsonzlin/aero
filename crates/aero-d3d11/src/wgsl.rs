use core::fmt;

use crate::sm4::{ShaderStage, Sm4Program};

#[derive(Debug, Clone)]
pub struct WgslTranslation {
    pub wgsl: String,
}

#[derive(Debug)]
pub enum WgslError {
    UnsupportedStage(ShaderStage),
    UnexpectedTokenStream(&'static str),
    UnsupportedInstruction { opcode: u32 },
    BadInstructionLength { opcode: u32, len: usize },
    OperandOutOfBounds,
    UnsupportedOperand(&'static str),
}

impl fmt::Display for WgslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WgslError::UnsupportedStage(stage) => write!(f, "unsupported shader stage {stage:?}"),
            WgslError::UnexpectedTokenStream(msg) => write!(f, "unexpected SM4/5 token stream: {msg}"),
            WgslError::UnsupportedInstruction { opcode } => {
                write!(f, "unsupported SM4/5 instruction opcode {opcode}")
            }
            WgslError::BadInstructionLength { opcode, len } => write!(
                f,
                "unexpected SM4/5 instruction length {len} for opcode {opcode}"
            ),
            WgslError::OperandOutOfBounds => write!(f, "operand token stream out of bounds"),
            WgslError::UnsupportedOperand(msg) => write!(f, "unsupported operand encoding: {msg}"),
        }
    }
}

impl std::error::Error for WgslError {}

/// Translate a *small* subset of SM4/SM5 DXBC into WGSL.
///
/// Current scope is intentionally tiny (enough for basic VS/PS passthrough shaders):
/// - `mov` between input/output registers
/// - `ret`
///
/// This is a bootstrap to get real DXBC flowing end-to-end; broader instruction
/// coverage and signature-driven IO mapping are expected to follow.
pub fn translate_sm4_to_wgsl(program: &Sm4Program) -> Result<WgslTranslation, WgslError> {
    match program.stage {
        ShaderStage::Vertex => translate_vs(program),
        ShaderStage::Pixel => translate_ps(program),
        other => Err(WgslError::UnsupportedStage(other)),
    }
}

fn translate_vs(program: &Sm4Program) -> Result<WgslTranslation, WgslError> {
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
        let loc = idx - 1;
        s.push_str(&format!("  @location({loc}) o{idx}: vec4<f32>,\n"));
    }
    s.push_str("};\n\n");

    s.push_str("@vertex\nfn main(input: VsIn) -> VsOut {\n");
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
            _ => return Err(WgslError::UnsupportedOperand("expected output<-input mov")),
        }
    }

    s.push_str("  return out;\n}\n");

    Ok(WgslTranslation { wgsl: s })
}

fn translate_ps(program: &Sm4Program) -> Result<WgslTranslation, WgslError> {
    let movs = extract_movs(program)?;

    // Expect exactly one output write (SV_Target0).
    let mov = movs
        .into_iter()
        .find(|m| matches!(m.dst.kind, RegKind::Output) && m.dst.index == 0)
        .ok_or(WgslError::UnexpectedTokenStream("pixel shader missing mov to o0"))?;

    let max_in = match mov.src.kind {
        RegKind::Input => mov.src.index,
        _ => return Err(WgslError::UnsupportedOperand("expected o0<-input mov")),
    };

    let mut s = String::new();
    s.push_str("struct PsIn {\n");
    // D3D pixel shaders typically have SV_Position in v0; map it to @builtin(position).
    s.push_str("  @builtin(position) pos: vec4<f32>,\n");
    for idx in 1..=max_in {
        let loc = idx - 1;
        s.push_str(&format!("  @location({loc}) v{idx}: vec4<f32>,\n"));
    }
    s.push_str("};\n\n");

    s.push_str("@fragment\nfn main(input: PsIn) -> @location(0) vec4<f32> {\n");
    s.push_str(&format!("  return input.v{};\n", mov.src.index));
    s.push_str("}\n");

    Ok(WgslTranslation { wgsl: s })
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

fn extract_movs(program: &Sm4Program) -> Result<Vec<Mov>, WgslError> {
    let declared_len = program.tokens.get(1).copied().unwrap_or(0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        return Err(WgslError::UnexpectedTokenStream(
            "declared token length out of bounds",
        ));
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2;
    let mut movs = Vec::new();
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & 0x7ff;
        let len = ((opcode_token >> 11) & 0x1fff) as usize;
        if len == 0 {
            return Err(WgslError::UnexpectedTokenStream(
                "instruction length cannot be zero",
            ));
        }
        if i + len > toks.len() {
            return Err(WgslError::UnexpectedTokenStream(
                "instruction overruns declared token stream",
            ));
        }

        // Opcode values are part of the D3D10+ bytecode spec; we only recognise
        // the handful needed for the bootstrap shaders used in tests.
        //
        // These numeric IDs are validated against DXC output in tests.
        const OPCODE_MOV: u32 = 0x01;
        const OPCODE_RET: u32 = 0x3e;

        match opcode {
            OPCODE_MOV => {
                if len != 5 {
                    return Err(WgslError::BadInstructionLength { opcode, len });
                }
                let dst = parse_reg_operand(toks.get(i + 1..i + 3).ok_or(WgslError::OperandOutOfBounds)?)?;
                let src = parse_reg_operand(toks.get(i + 3..i + 5).ok_or(WgslError::OperandOutOfBounds)?)?;
                movs.push(Mov { dst, src });
            }
            OPCODE_RET => {
                break;
            }
            _ => {
                // Skip declarations/other instructions for now.
            }
        }

        i += len;
    }

    Ok(movs)
}

fn parse_reg_operand(tokens: &[u32]) -> Result<RegRef, WgslError> {
    if tokens.len() != 2 {
        return Err(WgslError::UnsupportedOperand("operand must be 2 dwords"));
    }
    let token = tokens[0];
    if (token & 0x8000_0000) != 0 {
        return Err(WgslError::UnsupportedOperand("extended operand token"));
    }

    // Operand type: bits 4..=11 in the D3D10+ bytecode spec.
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
