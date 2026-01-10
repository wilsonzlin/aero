use std::collections::{HashMap, HashSet};

use crate::t2_ir::BlockId;

/// Runtime profiling data consumed by the Tier-2 trace builder.
#[derive(Clone, Debug, Default)]
pub struct ProfileData {
    /// Execution counts per basic block.
    pub block_counts: HashMap<BlockId, u64>,
    /// Execution counts per edge (`from` -> `to`).
    pub edge_counts: HashMap<(BlockId, BlockId), u64>,
    /// Detected hot backedges (`from` -> `to`), typically loop latches.
    pub hot_backedges: HashSet<(BlockId, BlockId)>,
    /// Code page versions at the time of trace compilation (self-modifying code guard).
    pub code_page_versions: HashMap<u64, u64>,
}

impl ProfileData {
    pub fn block_count(&self, block: BlockId) -> u64 {
        self.block_counts.get(&block).copied().unwrap_or(0)
    }

    pub fn edge_count(&self, from: BlockId, to: BlockId) -> u64 {
        self.edge_counts.get(&(from, to)).copied().unwrap_or(0)
    }

    pub fn is_hot_backedge(&self, from: BlockId, to: BlockId) -> bool {
        self.hot_backedges.contains(&(from, to))
    }

    pub fn code_page_version(&self, page: u64) -> u64 {
        self.code_page_versions.get(&page).copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug)]
pub struct TraceConfig {
    pub hot_block_threshold: u64,
    pub max_blocks: usize,
    pub max_instrs: usize,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            hot_block_threshold: 1000,
            max_blocks: 32,
            max_instrs: 4096,
        }
    }
}
