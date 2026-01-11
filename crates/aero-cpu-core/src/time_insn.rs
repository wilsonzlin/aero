//! Decode helpers for time/serialization primitives.
//!
//! The full x86 decoder lives at a higher layer (interpreter/JIT). This module
//! exists so those layers can share a single, deterministic classification of
//! the time/serialization primitives that affect block boundaries and interrupt
//! delivery.

use crate::Exception;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionKind {
    Rdtsc,
    Rdtscp,
    Lfence,
    Sfence,
    Mfence,
    Cpuid,
    Pause,
    Nop,
    Rdmsr,
    Wrmsr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedInstruction {
    pub kind: InstructionKind,
    pub len: u8,
    /// Approximate cycle cost used to advance the virtual TSC.
    pub cycles: u64,
    /// Whether this instruction is treated as a serializing point for the virtual pipeline.
    pub serializing: bool,
    /// Whether this instruction should terminate a JIT/interpreter basic block.
    pub terminates_block: bool,
    /// Hint that the instruction is used in spin loops and can be used to yield/check stop flags.
    pub is_pause_hint: bool,
}

pub fn decode_instruction(bytes: &[u8]) -> Result<DecodedInstruction, Exception> {
    let b0 = *bytes.first().ok_or(Exception::InvalidOpcode)?;

    // PAUSE is encoded as F3 90 (rep; nop).
    if b0 == 0xF3 {
        if bytes.len() < 2 {
            return Err(Exception::InvalidOpcode);
        }
        if bytes[1] == 0x90 {
            return Ok(DecodedInstruction {
                kind: InstructionKind::Pause,
                len: 2,
                cycles: 1,
                serializing: false,
                terminates_block: false,
                is_pause_hint: true,
            });
        }
    }

    if b0 == 0x90 {
        return Ok(DecodedInstruction {
            kind: InstructionKind::Nop,
            len: 1,
            cycles: 1,
            serializing: false,
            terminates_block: false,
            is_pause_hint: false,
        });
    }

    if b0 != 0x0F {
        return Err(Exception::InvalidOpcode);
    }

    let b1 = *bytes.get(1).ok_or(Exception::InvalidOpcode)?;
    match b1 {
        0x31 => Ok(DecodedInstruction {
            kind: InstructionKind::Rdtsc,
            len: 2,
            cycles: 1,
            serializing: false,
            terminates_block: false,
            is_pause_hint: false,
        }),
        0x30 => Ok(DecodedInstruction {
            kind: InstructionKind::Wrmsr,
            len: 2,
            cycles: 1,
            serializing: true,
            terminates_block: true,
            is_pause_hint: false,
        }),
        0x32 => Ok(DecodedInstruction {
            kind: InstructionKind::Rdmsr,
            len: 2,
            cycles: 1,
            serializing: true,
            terminates_block: true,
            is_pause_hint: false,
        }),
        0xA2 => Ok(DecodedInstruction {
            kind: InstructionKind::Cpuid,
            len: 2,
            cycles: 1,
            serializing: true,
            terminates_block: true,
            is_pause_hint: false,
        }),
        0x01 => {
            let b2 = *bytes.get(2).ok_or(Exception::InvalidOpcode)?;
            if b2 == 0xF9 {
                return Ok(DecodedInstruction {
                    kind: InstructionKind::Rdtscp,
                    len: 3,
                    cycles: 1,
                    serializing: true,
                    terminates_block: true,
                    is_pause_hint: false,
                });
            }

            Err(Exception::InvalidOpcode)
        }
        0xAE => {
            let b2 = *bytes.get(2).ok_or(Exception::InvalidOpcode)?;
            let kind = match b2 {
                0xE8 => InstructionKind::Lfence,
                0xF0 => InstructionKind::Mfence,
                0xF8 => InstructionKind::Sfence,
                _ => return Err(Exception::InvalidOpcode),
            };

            Ok(DecodedInstruction {
                kind,
                len: 3,
                cycles: 1,
                serializing: true,
                terminates_block: true,
                is_pause_hint: false,
            })
        }
        _ => Err(Exception::InvalidOpcode),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub start: usize,
    pub len: usize,
    pub instructions: Vec<DecodedInstruction>,
}

impl BasicBlock {
    pub fn end(&self) -> usize {
        self.start + self.len
    }
}

pub struct BasicBlockBuilder;

impl BasicBlockBuilder {
    pub fn decode_block(
        code: &[u8],
        start: usize,
        max_instructions: usize,
    ) -> Result<BasicBlock, Exception> {
        let mut pc = start;
        let mut instructions = Vec::new();

        while pc < code.len() && instructions.len() < max_instructions {
            let inst = decode_instruction(&code[pc..])?;
            pc += inst.len as usize;
            instructions.push(inst);

            if inst.terminates_block {
                break;
            }
        }

        Ok(BasicBlock {
            start,
            len: pc - start,
            instructions,
        })
    }
}
