use aero_cpu::CpuBus;
use aero_x86::tier1::{decode_one, DecodedInst, InstKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLimits {
    pub max_insts: usize,
    pub max_bytes: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self { max_insts: 64, max_bytes: 1024 }
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
pub fn discover_block<B: CpuBus>(bus: &B, entry_rip: u64, limits: BlockLimits) -> BasicBlock {
    let mut insts = Vec::new();
    let mut rip = entry_rip;
    let mut total_bytes = 0usize;

    loop {
        if insts.len() >= limits.max_insts || total_bytes >= limits.max_bytes {
            return BasicBlock { entry_rip, insts, end_kind: BlockEndKind::Limit { next_rip: rip } };
        }

        let bytes = bus.fetch(rip, 15);
        let inst = decode_one(rip, &bytes);
        total_bytes += inst.len as usize;
        rip = inst.next_rip();

        let end_kind = match inst.kind {
            InstKind::JmpRel { .. } => Some(BlockEndKind::Jmp),
            InstKind::JccRel { .. } => Some(BlockEndKind::Jcc),
            InstKind::CallRel { .. } => Some(BlockEndKind::Call),
            InstKind::Ret => Some(BlockEndKind::Ret),
            InstKind::Invalid => Some(BlockEndKind::ExitToInterpreter { next_rip: inst.next_rip() }),
            _ => None,
        };

        insts.push(inst);

        if let Some(kind) = end_kind {
            return BasicBlock { entry_rip, insts, end_kind: kind };
        }
    }
}
