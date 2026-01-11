//! Tier-2 (optimizing) JIT pipeline: CFG builder + trace IR + optimizer + WASM codegen.

pub mod builder;

pub use builder::{build_function_from_x86, CfgBuildConfig};

pub mod ir {
    pub use crate::t2_ir::*;
}

pub mod exec {
    pub use crate::t2_exec::*;
}

pub mod opt {
    pub use crate::opt::*;
}

pub mod trace {
    pub use crate::trace::*;
}

pub mod wasm {
    pub use crate::wasm::tier2::*;
}
