//! Tier-2 optimizer + trace compiler.
//!
//! Tier-2 consumes a trace-oriented IR and performs optimizations before lowering the result to
//! WASM.

pub mod builder;
pub mod interp;
pub mod ir;
pub mod opt;
pub mod profile;
pub mod trace;
pub mod wasm_codegen;

pub use builder::{build_function_from_x86, CfgBuildConfig};
pub use opt::optimize_trace;
pub use trace::TraceBuilder;
pub use wasm_codegen::Tier2WasmCodegen;
