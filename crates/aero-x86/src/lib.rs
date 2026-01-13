//! x86/x86-64 decoding helpers.
//!
//! The project uses `iced-x86` as the underlying decoder, but we keep a small
//! wrapper API so the rest of the emulator does not depend on `iced-x86`
//! directly.

use aero_cpu_decoder::{decode_instruction, DecodeMode};

pub use aero_cpu_decoder::{Code, Instruction, MemorySize, Mnemonic, OpKind, Register};

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
    let mode = match bitness {
        16 => DecodeMode::Bits16,
        32 => DecodeMode::Bits32,
        64 => DecodeMode::Bits64,
        _ => return Err(DecodeError::InvalidInstruction),
    };

    let instr = decode_instruction(mode, ip, bytes).map_err(|_| DecodeError::InvalidInstruction)?;
    Ok(DecodedInst {
        len: instr.len() as u8,
        instr,
    })
}

pub mod tier1;

// The structured decoder (`decoder` / `inst` / `opcode_tables`) is only used by
// correctness tests and intentionally kept out of non-test builds to avoid pulling
// in the `yaxpeax-*` decoder dependencies during production builds.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod decoder;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod inst;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub mod opcode_tables;
