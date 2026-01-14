use std::borrow::Cow;

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
        0x002E => (2, 2), // mova dst, src
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

fn read_token_u32_le(token_stream: &[u8], idx: usize) -> Option<u32> {
    let offset = idx.checked_mul(4)?;
    let bytes = token_stream.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn score_sm2_sm3_length_encoding(
    token_stream: &[u8],
    encoding: Sm2Sm3InstructionLengthEncoding,
) -> Option<i32> {
    let token_count = token_stream.len().checked_div(4)?;
    if token_count == 0 {
        return None;
    }

    let mut score = 0i32;
    let mut idx = 1usize;
    let mut steps = 0usize;
    let mut saw_end = false;
    while idx < token_count && steps < token_count {
        let token = read_token_u32_le(token_stream, idx)?;
        let opcode = (token & 0xFFFF) as u16;

        // Comment blocks are length-prefixed in bits 16..30 and must be skipped verbatim.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize.checked_add(comment_len)?;
            if idx + total_len > token_count {
                return None;
            }
            idx += total_len;
            steps += 1;
            continue;
        }

        if opcode == 0xFFFF {
            saw_end = true;
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
        if idx + total_len > token_count {
            return None;
        }
        let operand_len = total_len - 1;

        if let Some((min, max)) = expected_operand_count_range(opcode) {
            // Reward matching operand counts; penalize mismatches. This helps distinguish real
            // opcode tokens from register/operand tokens that happen to decode to an opcode.
            if operand_len >= min && operand_len <= max {
                score += 2;
            } else {
                score -= 1;
            }

            // Additional heuristic: for common register-only ops, operand tokens should not look
            // like opcode tokens. Penalize encodings that treat the terminating `end` token (or a
            // comment block header) as an operand.
            //
            // This makes the detection more robust against malformed streams with bogus length
            // fields that could otherwise be misclassified as operand-count encoding.
            //
            // Skip opcodes that legitimately include immediate values.
            if !matches!(opcode, 0x0051 | 0x0052 | 0x0053) && operand_len > 0 {
                let last_operand = read_token_u32_le(token_stream, idx + total_len - 1)?;
                let last_opcode = (last_operand & 0xFFFF) as u16;
                if matches!(last_opcode, 0xFFFF | 0xFFFE) {
                    score -= 4;
                }
            }
        }

        idx += total_len;
        steps += 1;
    }

    // All valid SM2/SM3 instruction streams are terminated by an explicit `end` (0xFFFF) opcode.
    // If our chosen encoding never encounters it, we likely misclassified an intentionally
    // malformed/truncated stream (e.g. by "consuming" the end token as an operand).
    if !saw_end {
        return None;
    }

    Some(score)
}

/// Some historical shader blobs encode opcode token length as the number of operand tokens rather
/// than the SM2/SM3 spec's total instruction length.
///
/// Normalize operand-count-encoded token streams to real total-length encoding so downstream SM3
/// decoders/parsers can consume them.
pub(crate) fn normalize_sm2_sm3_instruction_lengths<'a>(
    token_stream: &'a [u8],
) -> Result<Cow<'a, [u8]>, String> {
    if !token_stream.len().is_multiple_of(4) {
        return Err(format!(
            "token stream length {} is not a multiple of 4",
            token_stream.len()
        ));
    }
    if token_stream.len() < 4 {
        return Err("token stream too small".to_owned());
    }
    let token_count = token_stream.len() / 4;

    let score_total =
        score_sm2_sm3_length_encoding(token_stream, Sm2Sm3InstructionLengthEncoding::TotalLength)
            .unwrap_or(i32::MIN);
    let score_operands =
        score_sm2_sm3_length_encoding(token_stream, Sm2Sm3InstructionLengthEncoding::OperandCount)
            .unwrap_or(i32::MIN);
    let encoding = if score_operands > score_total {
        Sm2Sm3InstructionLengthEncoding::OperandCount
    } else {
        Sm2Sm3InstructionLengthEncoding::TotalLength
    };

    if encoding == Sm2Sm3InstructionLengthEncoding::TotalLength {
        return Ok(Cow::Borrowed(token_stream));
    }

    let mut out = token_stream.to_vec();
    let mut idx = 1usize;
    while idx < token_count {
        let token =
            read_token_u32_le(&out, idx).ok_or_else(|| "token read out of bounds".to_owned())?;
        let opcode = (token & 0xFFFF) as u16;

        // Comments are variable-length data blocks that should be skipped.
        // Layout: opcode=0xFFFE, length in DWORDs in bits 16..30.
        if opcode == 0xFFFE {
            let comment_len = ((token >> 16) & 0x7FFF) as usize;
            let total_len = 1usize
                .checked_add(comment_len)
                .ok_or_else(|| "comment length overflow".to_owned())?;
            if idx + total_len > token_count {
                return Err(format!(
                    "comment length {comment_len} exceeds remaining tokens {}",
                    token_count - idx
                ));
            }
            idx += total_len;
            continue;
        }

        if opcode == 0xFFFF {
            break;
        }

        // In operand-count encoding, bits 24..27 specify the number of operand tokens, so total
        // instruction length is `operands + 1`.
        let operand_count = ((token >> 24) & 0x0F) as usize;
        let length = operand_count
            .checked_add(1)
            .ok_or_else(|| "instruction length overflow".to_owned())?;
        if idx + length > token_count {
            return Err(format!(
                "instruction length {length} exceeds remaining tokens {}",
                token_count - idx
            ));
        }

        if operand_count > 0xE {
            return Err(format!(
                "operand count {operand_count} cannot be re-encoded into a 4-bit total-length field"
            ));
        }
        let total_len_field = (operand_count + 1) as u32;
        let patched = (token & !(0x0F << 24)) | ((total_len_field & 0x0F) << 24);
        let offset = idx * 4;
        out[offset..offset + 4].copy_from_slice(&patched.to_le_bytes());

        idx += length;
    }

    Ok(Cow::Owned(out))
}
