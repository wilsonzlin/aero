//! x86/x86-64 decoding wrapper used by the interpreter.
//!
//! This crate intentionally exposes a small surface area. The rest of the
//! emulator should depend on `aero_x86` (not `iced-x86`) so we can swap the
//! decoder implementation later without touching CPU semantics code.

use iced_x86::{Decoder, DecoderOptions};

pub use iced_x86::{Code, Instruction, MemorySize, Mnemonic, OpKind, Register};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    InvalidInstruction,
}

#[derive(Debug, Clone)]
pub struct DecodedInst {
    pub instr: Instruction,
    pub len: u8,
}

pub fn decode(bytes: &[u8], ip: u64, bitness: u32) -> Result<DecodedInst, DecodeError> {
    // `bytes` is expected to be at least 1 byte and at most 15 bytes. The
    // interpreter always fetches 15 bytes, matching the architectural max
    // instruction length.
    let mut decoder = Decoder::with_ip(bitness, bytes, ip, DecoderOptions::NONE);
    let instr = decoder.decode();
    if instr.is_invalid() {
        return Err(DecodeError::InvalidInstruction);
    }
    Ok(DecodedInst {
        len: instr.len() as u8,
        instr,
    })
}
