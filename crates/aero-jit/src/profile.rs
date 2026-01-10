use crate::microvm::{BlockId, Cond, FuncId};
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub struct BranchProfile {
    pub then_count: u64,
    pub else_count: u64,
}

impl BranchProfile {
    pub fn record(&mut self, taken_then: bool) {
        if taken_then {
            self.then_count = self.then_count.wrapping_add(1);
        } else {
            self.else_count = self.else_count.wrapping_add(1);
        }
    }

    pub fn hot_successor(&self, then_tgt: BlockId, else_tgt: BlockId) -> (BlockId, BlockId) {
        if self.then_count >= self.else_count {
            (then_tgt, else_tgt)
        } else {
            (else_tgt, then_tgt)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BlockProfile {
    pub exec_count: u64,
    pub branch: Option<(Cond, BranchProfile)>,
}

#[derive(Clone, Debug)]
pub struct FuncProfile {
    pub blocks: Vec<BlockProfile>,
    /// Outgoing call counts: `callee -> count`.
    pub call_edges: HashMap<FuncId, u64>,
}

impl FuncProfile {
    pub fn new(block_count: usize) -> Self {
        Self {
            blocks: vec![BlockProfile::default(); block_count],
            call_edges: HashMap::new(),
        }
    }

    pub fn record_block_entry(&mut self, block: BlockId) -> u64 {
        let bp = &mut self.blocks[block];
        bp.exec_count = bp.exec_count.wrapping_add(1);
        bp.exec_count
    }

    pub fn record_branch(&mut self, block: BlockId, cond: Cond, taken_then: bool) {
        let bp = &mut self.blocks[block];
        match &mut bp.branch {
            Some((existing_cond, prof)) if *existing_cond == cond => prof.record(taken_then),
            Some((_existing_cond, prof)) => {
                // Different condition observed at same block (shouldn't happen in this toy ISA).
                prof.record(taken_then);
            }
            None => {
                let mut prof = BranchProfile::default();
                prof.record(taken_then);
                bp.branch = Some((cond, prof));
            }
        }
    }

    pub fn record_call(&mut self, callee: FuncId) {
        *self.call_edges.entry(callee).or_insert(0) += 1;
    }
}

