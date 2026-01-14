use super::opcode as sm4_opcode;
use super::Sm4Program;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sm5GsStreamViolation {
    pub op_name: &'static str,
    pub stream: u32,
    /// DWORD index (token index) of the violating opcode token within the SM4/SM5 token stream.
    pub at_dword: usize,
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
                let Some(ext) = toks.get(operand_pos).copied() else {
                    break;
                };
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
                    let Some(ext) = toks.get(operand_pos).copied() else {
                        break;
                    };
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
                        return Some(Sm5GsStreamViolation {
                            op_name,
                            stream,
                            at_dword: i,
                        });
                    }
                }
            }
        }

        i += len;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_program(mut tokens: Vec<u32>) -> Sm4Program {
        // Patch the declared length.
        tokens[1] = tokens.len() as u32;
        Sm4Program {
            stage: super::super::ShaderStage::Geometry,
            model: super::super::ShaderModel { major: 5, minor: 0 },
            tokens,
        }
    }

    fn opcode_token(opcode: u32, len: u32) -> u32 {
        opcode | (len << sm4_opcode::OPCODE_LEN_SHIFT)
    }

    fn opcode_token_extended(opcode: u32, len: u32) -> u32 {
        opcode | (len << sm4_opcode::OPCODE_LEN_SHIFT) | sm4_opcode::OPCODE_EXTENDED_BIT
    }

    fn operand_token_immediate32(num_components: u32) -> u32 {
        let mut token = 0u32;
        token |= num_components & sm4_opcode::OPERAND_NUM_COMPONENTS_MASK;
        token |= (sm4_opcode::OPERAND_SEL_SELECT1 & sm4_opcode::OPERAND_SELECTION_MODE_MASK)
            << sm4_opcode::OPERAND_SELECTION_MODE_SHIFT;
        token |= (sm4_opcode::OPERAND_TYPE_IMMEDIATE32 & sm4_opcode::OPERAND_TYPE_MASK)
            << sm4_opcode::OPERAND_TYPE_SHIFT;
        token |= (sm4_opcode::OPERAND_INDEX_DIMENSION_0D
            & sm4_opcode::OPERAND_INDEX_DIMENSION_MASK)
            << sm4_opcode::OPERAND_INDEX_DIMENSION_SHIFT;
        token
    }

    fn assert_detects_nonzero_stream(opcode: u32, expected_op_name: &'static str) {
        let program = make_program(vec![
            0,
            0,
            opcode_token(opcode, 3),
            operand_token_immediate32(1),
            1,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: expected_op_name,
                stream: 1,
                at_dword: 2,
            })
        );
    }

    #[test]
    fn stream0_implicit_operand_is_ok() {
        let program = make_program(vec![
            0, // version (ignored by scan)
            0, // declared length patched by helper
            opcode_token(sm4_opcode::OPCODE_EMIT_STREAM, 1),
        ]);
        assert_eq!(scan_sm5_nonzero_gs_stream(&program), None);
    }

    #[test]
    fn stream0_explicit_operand_is_ok() {
        let program = make_program(vec![
            0,
            0,
            opcode_token(sm4_opcode::OPCODE_EMIT_STREAM, 3),
            operand_token_immediate32(1),
            0,
        ]);
        assert_eq!(scan_sm5_nonzero_gs_stream(&program), None);
    }

    #[test]
    fn detects_nonzero_stream_scalar_immediate() {
        assert_detects_nonzero_stream(sm4_opcode::OPCODE_EMIT_STREAM, "emit_stream");
    }

    #[test]
    fn detects_nonzero_cut_stream_scalar_immediate() {
        assert_detects_nonzero_stream(sm4_opcode::OPCODE_CUT_STREAM, "cut_stream");
    }

    #[test]
    fn detects_nonzero_emitthen_cut_stream_scalar_immediate() {
        assert_detects_nonzero_stream(sm4_opcode::OPCODE_EMITTHENCUT_STREAM, "emitthen_cut_stream");
    }

    #[test]
    fn detects_nonzero_stream_after_implicit_zero() {
        let program = make_program(vec![
            0,
            0,
            opcode_token(sm4_opcode::OPCODE_EMIT_STREAM, 1),
            opcode_token(sm4_opcode::OPCODE_EMIT_STREAM, 3),
            operand_token_immediate32(1),
            1,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: "emit_stream",
                stream: 1,
                at_dword: 3,
            })
        );
    }

    #[test]
    fn detects_nonzero_stream_with_extended_opcode_token() {
        let program = make_program(vec![
            0,
            0,
            opcode_token_extended(sm4_opcode::OPCODE_EMIT_STREAM, 4),
            0, // extended opcode token
            operand_token_immediate32(1),
            1,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: "emit_stream",
                stream: 1,
                at_dword: 2,
            })
        );
    }

    #[test]
    fn detects_nonzero_stream_with_extended_operand_token_and_vec4_immediate() {
        let mut operand = operand_token_immediate32(2);
        operand |= sm4_opcode::OPERAND_EXTENDED_BIT;
        let program = make_program(vec![
            0,
            0,
            opcode_token(sm4_opcode::OPCODE_EMIT_STREAM, 7),
            operand,
            0, // extended operand token
            1, // lane0 stream index
            0,
            0,
            0,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: "emit_stream",
                stream: 1,
                at_dword: 2,
            })
        );
    }

    #[test]
    fn detects_nonzero_stream_with_extended_opcode_and_operand_tokens() {
        let mut operand = operand_token_immediate32(1);
        operand |= sm4_opcode::OPERAND_EXTENDED_BIT;
        let program = make_program(vec![
            0,
            0,
            opcode_token_extended(sm4_opcode::OPCODE_EMIT_STREAM, 5),
            0, // extended opcode token
            operand,
            0, // extended operand token
            1,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: "emit_stream",
                stream: 1,
                at_dword: 2,
            })
        );
    }

    #[test]
    fn detects_nonzero_stream_with_multiple_extended_opcode_and_operand_tokens() {
        let mut operand = operand_token_immediate32(1);
        operand |= sm4_opcode::OPERAND_EXTENDED_BIT;
        let program = make_program(vec![
            0,
            0,
            // len = opcode + 2 opcode-ext + operand + 2 operand-ext + imm32 value
            opcode_token_extended(sm4_opcode::OPCODE_EMIT_STREAM, 7),
            sm4_opcode::OPCODE_EXTENDED_BIT, // opcode extension token 0 (more to follow)
            0,                               // opcode extension token 1 (terminates)
            operand,
            sm4_opcode::OPERAND_EXTENDED_BIT, // operand extension token 0 (more to follow)
            0,                                // operand extension token 1 (terminates)
            1,
        ]);
        assert_eq!(
            scan_sm5_nonzero_gs_stream(&program),
            Some(Sm5GsStreamViolation {
                op_name: "emit_stream",
                stream: 1,
                at_dword: 2,
            })
        );
    }
}
