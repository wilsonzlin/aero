/// Instruction-length normalization for D3D9 SM2/SM3 token streams.
///
/// Some historical shader blobs encode the opcode token length nibble (bits 24..27) as the number
/// of operand tokens, excluding the opcode token itself. The SM2/SM3 specification and most
/// toolchains instead encode the total instruction length (including the opcode token).
///
/// The legacy parser expects the total-length encoding. This module detects operand-count-encoded
/// streams and rewrites the length nibble in-place so the rest of the parser can operate
/// unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sm2Sm3InstructionLengthEncoding {
    /// Bits 24..27 encode the *total* instruction length in DWORD tokens, including the opcode
    /// token itself.
    TotalLength,
    /// Bits 24..27 encode the number of operand tokens, excluding the opcode token.
    OperandCount,
}

fn expected_operand_count_range(opcode: u16) -> Option<(usize, usize)> {
    // Expected operand token count for a subset of common SM2/SM3 opcodes. This is used only for
    // heuristically detecting operand-count-encoded token streams.
    //
    // Notes:
    // - Some opcodes are variable-length (e.g. `dcl`) and are omitted.
    // - Operand-less instructions are omitted since they do not distinguish encodings.
    Some(match opcode {
        0x0001 => (2, 2), // mov dst, src
        0x0002 => (3, 3), // add dst, src0, src1
        0x0003 => (3, 3), // sub
        0x0004 => (4, 4), // mad dst, src0, src1, src2
        0x0005 => (3, 3), // mul
        0x0006 => (2, 2), // rcp
        0x0007 => (2, 2), // rsq
        0x0008 => (3, 3), // dp3
        0x0009 => (3, 3), // dp4
        0x000A => (3, 3), // min
        0x000B => (3, 3), // max
        0x000C => (3, 3), // slt
        0x000D => (3, 3), // sge
        0x000E => (2, 2), // exp
        0x000F => (2, 2), // log
        0x0012 => (4, 4), // lrp
        0x0013 => (2, 2), // frc
        0x001B => (2, 2), // loop aL, i#
        0x0020 => (3, 3), // pow
        0x0026 => (1, 1), // rep i#
        0x0028 => (1, 1), // if
        0x0029 => (2, 2), // ifc
        0x002D => (2, 2), // breakc src0, src1 (compare op encoded in opcode token)
        0x0041 => (1, 1), // texkill src
        0x0042 => (3, 3), // texld dst, coord, sampler
        0x0051 => (5, 5), // def
        0x0052 => (5, 5), // defi
        0x0053 => (2, 2), // defb
        0x0054 => (3, 3), // seq
        0x0055 => (3, 3), // sne
        0x0056 => (2, 2), // dsx/ddx
        0x0057 => (2, 2), // dsy/ddy
        0x0058 => (4, 4), // cmp
        0x0059 => (4, 4), // dp2add
        0x005A => (3, 3), // dp2
        0x005D => (5, 5), // texldd dst, coord, ddx, ddy, sampler
        0x005E => (3, 3), // setp dst, src0, src1
        0x005F => (3, 3), // texldl dst, coord, sampler
        _ => return None,
    })
}

fn score_sm2_sm3_length_encoding(
    tokens: &[u32],
    encoding: Sm2Sm3InstructionLengthEncoding,
) -> Option<i32> {
    if tokens.is_empty() {
        return None;
    }

    let mut score = 0i32;
    let mut idx = 1usize;
    let mut steps = 0usize;
    while idx < tokens.len() && steps < tokens.len() {
        let token = *tokens.get(idx)?;
        let opcode = (token & 0xFFFF) as u16;

        // Comment blocks are length-prefixed in bits 16..30 and must be skipped verbatim.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize.checked_add(comment_len)?;
            if idx + total_len > tokens.len() {
                return None;
            }
            idx += total_len;
            steps += 1;
            continue;
        }

        if opcode == 0xFFFF {
            break;
        }

        let len_field = ((token >> 24) & 0x0F) as usize;
        let total_len = match encoding {
            Sm2Sm3InstructionLengthEncoding::TotalLength => {
                if len_field == 0 {
                    1
                } else {
                    len_field
                }
            }
            Sm2Sm3InstructionLengthEncoding::OperandCount => 1usize.checked_add(len_field)?,
        };
        if idx + total_len > tokens.len() {
            return None;
        }
        let operand_len = total_len - 1;

        if let Some((min, max)) = expected_operand_count_range(opcode) {
            if operand_len >= min && operand_len <= max {
                score += 2;
            } else {
                score -= 1;
            }
        }

        idx += total_len;
        steps += 1;
    }

    Some(score)
}

/// Returns a patched copy of `tokens` if the stream appears to use operand-count instruction length
/// encoding.
pub(crate) fn normalize_sm2_sm3_instruction_lengths(tokens: &[u32]) -> Option<Vec<u32>> {
    let score_total = score_sm2_sm3_length_encoding(tokens, Sm2Sm3InstructionLengthEncoding::TotalLength)
        .unwrap_or(i32::MIN);
    let score_operands =
        score_sm2_sm3_length_encoding(tokens, Sm2Sm3InstructionLengthEncoding::OperandCount)
            .unwrap_or(i32::MIN);
    if score_operands <= score_total {
        return None;
    }

    let mut out = tokens.to_vec();
    let mut idx = 1usize;
    while idx < out.len() {
        let token = out[idx];
        let opcode = (token & 0xFFFF) as u16;

        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize.checked_add(comment_len)?;
            if idx + total_len > out.len() {
                return None;
            }
            idx += total_len;
            continue;
        }

        if opcode == 0xFFFF {
            break;
        }

        let operand_count = ((token >> 24) & 0x0F) as usize;
        if operand_count > 0xE {
            return None;
        }
        let length = operand_count + 1;
        if idx + length > out.len() {
            return None;
        }

        out[idx] = (token & !(0x0F << 24)) | (((operand_count as u32) + 1) << 24);
        idx += length;
    }

    Some(out)
}

