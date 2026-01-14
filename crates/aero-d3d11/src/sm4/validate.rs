use super::opcode as sm4_opcode;
use super::Sm4Program;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sm5GsStreamViolation {
    pub op_name: &'static str,
    pub stream: u32,
}

/// Scan an SM4/SM5 token stream for SM5 geometry-shader multi-stream emission opcodes
/// (`emit_stream` / `cut_stream` / `emitthen_cut_stream`) that use a non-zero stream index.
///
/// This is intentionally a **token-level** scan (not a full decoder) so callers can enforce a
/// policy even when full decoding/translation isn't supported.
///
/// Returns the first non-zero stream-usage encountered, or `None` if:
/// - No non-zero stream usage was found
/// - The token stream appears malformed (leave detailed errors to the real decoder)
pub(crate) fn scan_sm5_nonzero_gs_stream(program: &Sm4Program) -> Option<Sm5GsStreamViolation> {
    let declared_len = program.tokens.get(1).copied().unwrap_or(0) as usize;
    if declared_len < 2 || declared_len > program.tokens.len() {
        return None;
    }

    let toks = &program.tokens[..declared_len];
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & sm4_opcode::OPCODE_MASK;
        let len =
            ((opcode_token >> sm4_opcode::OPCODE_LEN_SHIFT) & sm4_opcode::OPCODE_LEN_MASK) as usize;
        if len == 0 || i + len > toks.len() {
            return None;
        }

        let stream_opcode_name = if opcode == sm4_opcode::OPCODE_EMIT_STREAM {
            Some("emit_stream")
        } else if opcode == sm4_opcode::OPCODE_CUT_STREAM {
            Some("cut_stream")
        } else if opcode == sm4_opcode::OPCODE_EMITTHENCUT_STREAM {
            Some("emitthen_cut_stream")
        } else {
            None
        };

        if let Some(op_name) = stream_opcode_name {
            // `emit_stream` / `cut_stream` / `emitthen_cut_stream` take a single immediate operand
            // indicating the stream index, but some toolchains omit the operand entirely for
            // stream 0. Treat missing operands as implicit stream 0 and keep scanning.
            let inst_end = i + len;
            let mut operand_pos = i + 1;
            let mut extended = (opcode_token & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            while extended {
                if operand_pos >= inst_end {
                    break;
                }
                let Some(ext) = toks.get(operand_pos).copied() else { break };
                operand_pos += 1;
                extended = (ext & sm4_opcode::OPCODE_EXTENDED_BIT) != 0;
            }

            // If there is no operand token, this is an implicit stream-0 form.
            if operand_pos < inst_end {
                let operand_token = match toks.get(operand_pos).copied() {
                    Some(v) => v,
                    None => {
                        i += len;
                        continue;
                    }
                };
                operand_pos += 1;

                let ty = (operand_token >> sm4_opcode::OPERAND_TYPE_SHIFT)
                    & sm4_opcode::OPERAND_TYPE_MASK;
                if ty != sm4_opcode::OPERAND_TYPE_IMMEDIATE32 {
                    i += len;
                    continue;
                }

                // Skip extended operand tokens (modifiers).
                let mut operand_ext = (operand_token & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
                while operand_ext {
                    if operand_pos >= inst_end {
                        break;
                    }
                    let Some(ext) = toks.get(operand_pos).copied() else { break };
                    operand_pos += 1;
                    operand_ext = (ext & sm4_opcode::OPERAND_EXTENDED_BIT) != 0;
                }

                let index_dim = (operand_token >> sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT)
                    & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK;
                if index_dim != sm4_opcode::OPERAND_INDEX_DIMENSION_0D {
                    i += len;
                    continue;
                }

                let num_components = operand_token & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
                let stream = match num_components {
                    // Scalar immediate (1 DWORD payload).
                    1 => toks.get(operand_pos).copied(),
                    // 4-component immediate (4 DWORD payload); stream index is lane 0.
                    2 => toks.get(operand_pos).copied(),
                    _ => None,
                };
                if let Some(stream) = stream {
                    if stream != 0 {
                        return Some(Sm5GsStreamViolation { op_name, stream });
                    }
                }
            }
        }

        i += len;
    }

    None
}

