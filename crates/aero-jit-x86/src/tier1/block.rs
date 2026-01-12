use crate::Tier1Bus;
use aero_x86::tier1::{decode_one_mode, DecodedInst, InstKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLimits {
    pub max_insts: usize,
    pub max_bytes: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_insts: 64,
            max_bytes: 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockEndKind {
    Jmp,
    Jcc,
    Call,
    Ret,
    ExitToInterpreter { next_rip: u64 },
    Limit { next_rip: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicBlock {
    pub entry_rip: u64,
    pub bitness: u32,
    pub insts: Vec<DecodedInst>,
    pub end_kind: BlockEndKind,
}

/// Decode a basic block starting at `entry_rip`.
///
/// Decoding stops when one of the following conditions is met:
/// - a control-flow instruction is decoded (branch/call/ret)
/// - an unsupported/invalid instruction is decoded (must exit to interpreter)
/// - `limits` are exceeded
#[must_use]
pub fn discover_block<B: Tier1Bus>(bus: &B, entry_rip: u64, limits: BlockLimits) -> BasicBlock {
    discover_block_mode(bus, entry_rip, limits, 64)
}

/// Decode a basic block starting at `entry_rip` using the requested x86 bitness.
///
/// This is a thin wrapper around the Tier1 minimal decoder (`aero_x86::tier1`) that allows
/// front-ends/tests to run 16/32-bit guest payloads without mis-decoding `0x40..=0x4F` as REX.
#[must_use]
pub fn discover_block_mode<B: Tier1Bus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
    bitness: u32,
) -> BasicBlock {
    let mut insts = Vec::new();
    let mut rip = entry_rip;
    let mut total_bytes = 0usize;

    loop {
        if insts.len() >= limits.max_insts || total_bytes >= limits.max_bytes {
            return BasicBlock {
                entry_rip,
                bitness,
                insts,
                end_kind: BlockEndKind::Limit { next_rip: rip },
            };
        }

        let bytes = bus.fetch(rip, 15);
        let inst = decode_one_mode(rip, &bytes, bitness);
        total_bytes += inst.len as usize;
        rip = inst.next_rip();

        let end_kind = match inst.kind {
            InstKind::JmpRel { .. } => Some(BlockEndKind::Jmp),
            InstKind::JccRel { .. } => Some(BlockEndKind::Jcc),
            InstKind::CallRel { .. } => Some(BlockEndKind::Call),
            InstKind::Ret => Some(BlockEndKind::Ret),
            InstKind::Invalid => Some(BlockEndKind::ExitToInterpreter {
                next_rip: inst.rip,
            }),
            _ => None,
        };

        insts.push(inst);

        if let Some(kind) = end_kind {
            return BasicBlock {
                entry_rip,
                bitness,
                insts,
                end_kind: kind,
            };
        }
    }
}
