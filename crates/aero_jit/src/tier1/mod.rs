//! Tier-1 (baseline) JIT pipeline: block discovery + x86→IR translation + WASM codegen.

pub mod block {
    pub use crate::block::*;
}

pub mod translate {
    pub use crate::translate::*;
}

pub mod ir {
    pub use crate::tier1_ir::*;
}

pub mod wasm {
    pub use crate::wasm::tier1::*;
}

pub mod pipeline {
    pub use crate::tier1_pipeline::*;
}

pub use crate::block::{discover_block, BasicBlock, BlockEndKind, BlockLimits};
pub use crate::translate::translate_block;
