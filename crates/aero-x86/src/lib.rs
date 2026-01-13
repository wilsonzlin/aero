//! x86/x86-64 decoding helpers.
//!
//! ## Decoder layering
//!
//! `aero-x86` intentionally contains **multiple** decoding-related APIs. They serve
//! different tiers of the CPU pipeline and are not interchangeable:
//!
//! - **Tier-0 / interpreter decode (canonical)**: [`decode()`]
//!   - This is the production decoder used by the Tier-0 interpreter.
//!   - It wraps [`aero_cpu_decoder`] (which wraps `iced-x86`).
//!   - We re-export the `iced-x86` instruction model types (e.g. [`Instruction`],
//!     [`Mnemonic`], [`Register`]) via `aero-cpu-decoder` so the rest of the codebase
//!     does **not** depend on `iced-x86` directly, and so the decoder backend/version
//!     can be controlled from one place.
//!
//! - **Tier-1 minimal decoder**: [`tier1`]
//!   - A handwritten, *intentionally incomplete* decoder/normalizer.
//!   - Used by the Tier-1 JIT front-end for basic-block discovery and unit tests.
//!   - It is **not** a general-purpose x86 decoder. Unsupported instructions decode
//!     as [`tier1::InstKind::Invalid`] so callers can conservatively fall back to the
//!     interpreter.
//!
//! - **Structured decoder (validation / differential testing only)**: `decoder` /
//!   `inst` / `opcode_tables`
//!   - A legacy structured decoder primarily used by tests for correctness checking.
//!   - These modules are compiled only in `cfg(test)` builds (and non-`wasm32`) to
//!     avoid pulling the heavier `yaxpeax-*` dependencies into production builds.
//!     (If Task 54 lands, these may become feature-gated instead.)
//!
//! ## Which decoder should I use?
//!
//! ### Interpreter / full ISA coverage (Tier-0)
//!
//! ```rust,no_run
//! use aero_x86::decode;
//!
//! // `ip` is used for RIP-relative addressing and branch targets.
//! let bytes = [0x90u8]; // NOP
//! let decoded = decode(&bytes, 0x1000, 64).unwrap();
//! assert_eq!(decoded.len, decoded.instr.len() as u8);
//! ```
//!
//! ### Tier-1 JIT front-end (block discovery / normalization)
//!
//! ```rust
//! use aero_x86::tier1::{decode_one, InstKind};
//!
//! let bytes = [0x90u8]; // NOP
//! let inst = decode_one(0x1000, &bytes);
//! assert!(matches!(inst.kind, InstKind::Nop));
//! ```
//!
//! ### Validation / differential tests (structured decoder)
//!
//! The structured decoder modules are not part of the production API surface.
//! They are only available in test builds.
//!
//! ```rust,ignore
//! use aero_x86::decoder;
//!
//! let bytes = [0x90u8];
//! let inst = decoder::decode(&bytes, decoder::DecodeMode::Bits64, 0x1000).unwrap();
//! assert_eq!(inst.length, 1);
//! ```

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
