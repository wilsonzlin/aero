//! Minimal SM4/SM5 token stream walker.
//!
//! This is primarily intended for diagnostics / opcode discovery: given a token
//! stream (DWORDs) it splits it into instruction/declaration records using the
//! length field in the opcode token. Operands are not decoded.

use core::fmt;

use super::opcode::{OPCODE_EXTENDED_BIT, OPCODE_LEN_MASK, OPCODE_LEN_SHIFT, OPCODE_MASK};

/// A token stream instruction/declaration record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sm4TokenInst<'a> {
    /// Start DWORD index within the program token stream.
    pub start: usize,
    /// Number of DWORDs for this instruction (including opcode token).
    pub len: usize,
    /// Opcode ID (low 11 bits of the opcode token).
    pub opcode: u32,
    /// Raw opcode token DWORD.
    pub opcode_token: u32,
    /// All DWORDs belonging to this instruction (including the opcode token).
    pub tokens: &'a [u32],
    /// Extended opcode tokens (if any).
    pub ext_tokens: &'a [u32],
    /// Remaining tokens after opcode + extended opcode tokens (typically operand tokens).
    pub operand_tokens: &'a [u32],
}

/// Errors that can occur while walking a token stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sm4TokenDumpError {
    /// DWORD index within the program token stream where the error occurred.
    pub at_dword: usize,
    /// Error details.
    pub kind: Sm4TokenDumpErrorKind,
}

/// Error variants for [`Sm4TokenDumpError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sm4TokenDumpErrorKind {
    /// The token stream is too short to contain the version + declared length.
    TooShort { dwords: usize },
    /// Declared length field is out of bounds.
    InvalidDeclaredLength { declared: usize, available: usize },
    /// Instruction length field is zero.
    InstructionLengthZero,
    /// Instruction length overruns the declared program length.
    InstructionOutOfBounds {
        start: usize,
        len: usize,
        available: usize,
    },
    /// Opcode token declared extended tokens but the instruction does not contain them.
    MissingExtendedToken,
}

impl fmt::Display for Sm4TokenDumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SM4/5 token dump error at dword {}: ", self.at_dword)?;
        match &self.kind {
            Sm4TokenDumpErrorKind::TooShort { dwords } => {
                write!(f, "token stream too short ({dwords} dwords)")
            }
            Sm4TokenDumpErrorKind::InvalidDeclaredLength {
                declared,
                available,
            } => write!(
                f,
                "declared program length {declared} is out of bounds (available {available})"
            ),
            Sm4TokenDumpErrorKind::InstructionLengthZero => write!(f, "instruction length is zero"),
            Sm4TokenDumpErrorKind::InstructionOutOfBounds {
                start,
                len,
                available,
            } => write!(
                f,
                "instruction at {start} with length {len} overruns program (available {available})"
            ),
            Sm4TokenDumpErrorKind::MissingExtendedToken => {
                write!(f, "missing extended opcode token")
            }
        }
    }
}

impl std::error::Error for Sm4TokenDumpError {}

/// Split a SM4/SM5 program token stream into instruction/declaration records.
///
/// The input slice must include the version token at DWORD 0 and declared length at DWORD 1.
pub fn tokenize_instructions<'a>(
    tokens: &'a [u32],
) -> Result<Vec<Sm4TokenInst<'a>>, Sm4TokenDumpError> {
    if tokens.len() < 2 {
        return Err(Sm4TokenDumpError {
            at_dword: 0,
            kind: Sm4TokenDumpErrorKind::TooShort {
                dwords: tokens.len(),
            },
        });
    }

    let declared_len = tokens[1] as usize;
    if declared_len < 2 || declared_len > tokens.len() {
        return Err(Sm4TokenDumpError {
            at_dword: 1,
            kind: Sm4TokenDumpErrorKind::InvalidDeclaredLength {
                declared: declared_len,
                available: tokens.len(),
            },
        });
    }

    let toks = &tokens[..declared_len];

    let mut out = Vec::new();
    let mut i = 2usize;
    while i < toks.len() {
        let opcode_token = toks[i];
        let opcode = opcode_token & OPCODE_MASK;
        let len = ((opcode_token >> OPCODE_LEN_SHIFT) & OPCODE_LEN_MASK) as usize;
        if len == 0 {
            return Err(Sm4TokenDumpError {
                at_dword: i,
                kind: Sm4TokenDumpErrorKind::InstructionLengthZero,
            });
        }
        if i + len > toks.len() {
            return Err(Sm4TokenDumpError {
                at_dword: i,
                kind: Sm4TokenDumpErrorKind::InstructionOutOfBounds {
                    start: i,
                    len,
                    available: toks.len(),
                },
            });
        }

        let inst_tokens = &toks[i..i + len];

        // Extended opcode tokens directly follow the opcode token and can be chained.
        let mut ext_count = 0usize;
        if (opcode_token & OPCODE_EXTENDED_BIT) != 0 {
            let mut more = true;
            while more {
                let idx = 1 + ext_count;
                let ext = *inst_tokens.get(idx).ok_or_else(|| Sm4TokenDumpError {
                    at_dword: i + idx,
                    kind: Sm4TokenDumpErrorKind::MissingExtendedToken,
                })?;
                ext_count += 1;
                more = (ext & OPCODE_EXTENDED_BIT) != 0;
            }
        }

        let ext_tokens = &inst_tokens[1..1 + ext_count];
        let operand_tokens = &inst_tokens[1 + ext_count..];

        out.push(Sm4TokenInst {
            start: i,
            len,
            opcode,
            opcode_token,
            tokens: inst_tokens,
            ext_tokens,
            operand_tokens,
        });
        i += len;
    }

    Ok(out)
}
