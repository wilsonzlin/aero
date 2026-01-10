//! Aero x86/x86-64 instruction decoder.
//!
//! This crate provides a production-grade instruction decoder suitable for
//! feeding both an interpreter and a JIT. The API is designed around decoding
//! *one* instruction from up to [`MAX_INSTRUCTION_LEN`] bytes.
//!
//! ## Design notes
//!
//! - Deterministic: decoding does not depend on global state.
//! - No per-instruction heap allocations in the hot path.
//! - Full legacy prefix + REX + VEX/EVEX/XOP parsing hooks.
//! - Broad opcode coverage via a table-driven backend.
//!
//! The current backend is [`iced_x86`], which is table-driven and widely used.
//! We wrap it to provide a stable interface for the rest of Aero.

#![deny(missing_docs)]

use iced_x86::{Decoder as IcedDecoder, DecoderError as IcedDecoderError, DecoderOptions};

/// Decoded instruction type used by this crate (re-exported from `iced-x86`).
pub use iced_x86::Instruction;
/// Register enum used by decoded instructions (re-exported from `iced-x86`).
pub use iced_x86::Register;
/// Instruction kind/code enum (re-exported from `iced-x86`).
pub use iced_x86::Code;
/// Mnemonic enum (re-exported from `iced-x86`).
pub use iced_x86::Mnemonic;
/// Operand kind enum (re-exported from `iced-x86`).
pub use iced_x86::OpKind;
/// Memory size enum (re-exported from `iced-x86`).
pub use iced_x86::MemorySize;

/// Maximum architectural x86 instruction length.
pub const MAX_INSTRUCTION_LEN: usize = 15;

/// Decode mode/bitness.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecodeMode {
    /// 16-bit mode (real mode / 16-bit protected mode / v8086).
    Bits16,
    /// 32-bit mode (protected mode).
    Bits32,
    /// 64-bit mode (long mode).
    Bits64,
}

impl DecodeMode {
    #[inline]
    fn bitness(self) -> u32 {
        match self {
            DecodeMode::Bits16 => 16,
            DecodeMode::Bits32 => 32,
            DecodeMode::Bits64 => 64,
        }
    }
}

/// Instruction prefix state (legacy + REX + VEX/EVEX/XOP).
///
/// Only the *architecturally relevant* "last prefix wins" behavior is
/// reflected in this struct.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Prefixes {
    /// `LOCK` prefix (`F0`)
    pub lock: bool,
    /// `REP`/`REPE`/`REPZ` prefix (`F3`)
    pub rep: bool,
    /// `REPNE`/`REPNZ` prefix (`F2`)
    pub repne: bool,

    /// Segment override prefix, if present.
    pub segment: Option<Segment>,

    /// Operand-size override prefix (`66`)
    pub operand_size_override: bool,
    /// Address-size override prefix (`67`)
    pub address_size_override: bool,

    /// REX prefix (64-bit mode only), raw byte value (`0x40..=0x4F`).
    pub rex: Option<Rex>,
    /// VEX prefix, if present.
    pub vex: Option<Vex>,
    /// EVEX prefix, if present.
    pub evex: Option<Evex>,
    /// XOP prefix, if present.
    pub xop: Option<Xop>,
}

/// Segment override prefix.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Segment {
    /// ES (`26`)
    Es,
    /// CS (`2E`)
    Cs,
    /// SS (`36`)
    Ss,
    /// DS (`3E`)
    Ds,
    /// FS (`64`)
    Fs,
    /// GS (`65`)
    Gs,
}

/// REX prefix byte and decoded bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rex {
    /// Raw REX byte (`0x40..=0x4F`).
    pub byte: u8,
}

impl Rex {
    #[inline]
    fn new(byte: u8) -> Option<Self> {
        if (0x40..=0x4F).contains(&byte) {
            Some(Self { byte })
        } else {
            None
        }
    }

    /// REX.W: 64-bit operand size.
    #[inline]
    pub fn w(self) -> bool {
        self.byte & 0b1000 != 0
    }
    /// REX.R: extends ModR/M.reg.
    #[inline]
    pub fn r(self) -> bool {
        self.byte & 0b0100 != 0
    }
    /// REX.X: extends SIB.index.
    #[inline]
    pub fn x(self) -> bool {
        self.byte & 0b0010 != 0
    }
    /// REX.B: extends ModR/M.rm or SIB.base.
    #[inline]
    pub fn b(self) -> bool {
        self.byte & 0b0001 != 0
    }
}

/// VEX prefix (2-byte `C5` or 3-byte `C4`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Vex {
    /// Raw VEX prefix bytes (including the leading `C4`/`C5` byte).
    pub bytes: [u8; 3],
    /// Number of bytes used (2 or 3).
    pub len: u8,
}

/// EVEX prefix (`62` + 3 bytes).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Evex {
    /// Raw EVEX bytes (including leading `62`).
    pub bytes: [u8; 4],
}

/// XOP prefix (`8F` + 2 bytes).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Xop {
    /// Raw XOP bytes (including leading `8F`).
    pub bytes: [u8; 3],
}

/// Decoder error.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// Input slice was empty.
    EmptyInput,
    /// Input slice did not contain enough bytes to decode a complete instruction.
    UnexpectedEof,
    /// Instruction was invalid for the selected mode/options.
    InvalidInstruction,
}

/// Decoded instruction with prefix metadata.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DecodedInstruction {
    /// Parsed prefix state (best-effort, independent of the backend).
    pub prefixes: Prefixes,
    /// Decoded instruction (full operand forms).
    pub instruction: Instruction,
}

impl DecodedInstruction {
    /// Returns the instruction length in bytes.
    #[inline]
    pub fn len(&self) -> u8 {
        self.instruction.len() as u8
    }
}

/// Decode a single instruction from `bytes` without parsing prefix metadata.
///
/// This is the lowest-overhead entry point and is suitable for hot interpreter
/// loops that only need the decoded operand forms.
#[inline]
pub fn decode_instruction(mode: DecodeMode, ip: u64, bytes: &[u8]) -> Result<Instruction, DecodeError> {
    if bytes.is_empty() {
        return Err(DecodeError::EmptyInput);
    }

    let bytes = if bytes.len() > MAX_INSTRUCTION_LEN {
        &bytes[..MAX_INSTRUCTION_LEN]
    } else {
        bytes
    };

    let mut decoder = IcedDecoder::with_ip(mode.bitness(), bytes, ip, DecoderOptions::NONE);
    let instruction = decoder.decode();
    match decoder.last_error() {
        IcedDecoderError::None => {}
        IcedDecoderError::NoMoreBytes => return Err(DecodeError::UnexpectedEof),
        IcedDecoderError::InvalidInstruction => return Err(DecodeError::InvalidInstruction),
        // `DecoderError` is `#[non_exhaustive]`. Treat any future error kinds
        // as invalid instructions (this keeps the API stable and conservative).
        _ => return Err(DecodeError::InvalidInstruction),
    }

    let len = instruction.len() as usize;
    if len == 0 || len > MAX_INSTRUCTION_LEN {
        return Err(DecodeError::InvalidInstruction);
    }

    Ok(instruction)
}

/// Decode a single instruction from `bytes`.
///
/// `bytes` should contain at least the next [`MAX_INSTRUCTION_LEN`] bytes from
/// the instruction stream when possible. If fewer bytes are available, decode
/// may return [`DecodeError::UnexpectedEof`].
///
/// `ip` is used for RIP-relative addressing and branch target calculation.
#[inline]
pub fn decode_one(mode: DecodeMode, ip: u64, bytes: &[u8]) -> Result<DecodedInstruction, DecodeError> {
    let instruction = decode_instruction(mode, ip, bytes)?;

    // Best-effort prefix parsing for consumers that want raw prefix info.
    // This does not affect the backend decode.
    let bytes = if bytes.len() > MAX_INSTRUCTION_LEN {
        &bytes[..MAX_INSTRUCTION_LEN]
    } else {
        bytes
    };
    let prefixes = parse_prefixes(mode, bytes).map_err(|_| DecodeError::UnexpectedEof)?;

    Ok(DecodedInstruction { prefixes, instruction })
}

#[derive(Copy, Clone, Debug)]
enum PrefixParseError {
    UnexpectedEof,
}

/// Parse legacy/REX/VEX/EVEX/XOP prefixes (best-effort, for metadata only).
fn parse_prefixes(mode: DecodeMode, bytes: &[u8]) -> Result<Prefixes, PrefixParseError> {
    let mut p = Prefixes::default();
    let mut i = 0usize;

    // Intel SDM: prefixes are ordered, but in practice decoders accept any order
    // and use "last prefix wins" within each prefix group.
    while i < bytes.len() && i < MAX_INSTRUCTION_LEN {
        let b = bytes[i];
        match b {
            0xF0 => {
                p.lock = true;
                i += 1;
            }
            0xF2 => {
                p.repne = true;
                p.rep = false;
                i += 1;
            }
            0xF3 => {
                p.rep = true;
                p.repne = false;
                i += 1;
            }

            0x26 => {
                p.segment = Some(Segment::Es);
                i += 1;
            }
            0x2E => {
                p.segment = Some(Segment::Cs);
                i += 1;
            }
            0x36 => {
                p.segment = Some(Segment::Ss);
                i += 1;
            }
            0x3E => {
                p.segment = Some(Segment::Ds);
                i += 1;
            }
            0x64 => {
                p.segment = Some(Segment::Fs);
                i += 1;
            }
            0x65 => {
                p.segment = Some(Segment::Gs);
                i += 1;
            }

            0x66 => {
                p.operand_size_override = true;
                i += 1;
            }
            0x67 => {
                p.address_size_override = true;
                i += 1;
            }

            // VEX/EVEX/XOP (these are mutually exclusive).
            0xC5 => {
                // 2-byte VEX: C5 xx
                // In non-64-bit modes, `C5 /r` is also `LDS`, which requires a memory
                // operand (ModRM.mod != 0b11). VEX prefixes are only unambiguous when
                // the next byte cannot be a valid LDS ModRM.
                if i + 1 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                if mode != DecodeMode::Bits64 && (bytes[i + 1] & 0xC0) != 0xC0 {
                    break;
                }
                p.vex = Some(Vex {
                    bytes: [bytes[i], bytes[i + 1], 0],
                    len: 2,
                });
                break;
            }
            0xC4 => {
                // 3-byte VEX: C4 xx xx
                // In non-64-bit modes, `C4 /r` is also `LES`, which requires a memory
                // operand (ModRM.mod != 0b11). Disambiguate the same way as `C5` above.
                if i + 1 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                if mode != DecodeMode::Bits64 && (bytes[i + 1] & 0xC0) != 0xC0 {
                    break;
                }
                if i + 2 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                p.vex = Some(Vex {
                    bytes: [bytes[i], bytes[i + 1], bytes[i + 2]],
                    len: 3,
                });
                break;
            }
            0x62 => {
                // EVEX: 62 xx xx xx
                // In non-64-bit modes, `62 /r` is also `BOUND`, which requires a memory
                // operand (ModRM.mod != 0b11). Disambiguate using the next byte.
                if i + 1 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                if mode != DecodeMode::Bits64 && (bytes[i + 1] & 0xC0) != 0xC0 {
                    break;
                }
                if i + 3 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                p.evex = Some(Evex {
                    bytes: [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]],
                });
                break;
            }
            0x8F => {
                // XOP: 8F xx xx
                // `8F /0` is also `POP r/m`. We treat it as XOP only if the next byte
                // cannot be a valid POP ModRM (reg != 0).
                if i + 1 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                if (bytes[i + 1] & 0x38) == 0 {
                    break;
                }
                if i + 2 >= bytes.len() {
                    return Err(PrefixParseError::UnexpectedEof);
                }
                p.xop = Some(Xop {
                    bytes: [bytes[i], bytes[i + 1], bytes[i + 2]],
                });
                break;
            }

            // REX (64-bit mode only). Must appear after legacy prefixes.
            0x40..=0x4F if mode == DecodeMode::Bits64 => {
                p.rex = Rex::new(b);
                i += 1;
            }

            _ => break,
        }
    }

    Ok(p)
}
