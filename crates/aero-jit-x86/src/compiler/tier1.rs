//! Pure Tier-1 compilation helpers.
//!
//! These helpers compile a single x86 basic block into a standalone WASM module. For integrating
//! with [`aero_cpu_core::jit::runtime::JitRuntime`], prefer [`crate::tier1::pipeline::Tier1Compiler`]
//! which handles page-version metadata snapshots and WASM registration.

use thiserror::Error;

use crate::tier1::wasm::{Tier1WasmCodegen, Tier1WasmOptions};
use crate::tier1::{discover_block, translate_block, BlockLimits};
use crate::tier1_ir::{IrInst, IrTerminator};
use crate::Tier1Bus;

#[derive(Debug, Error)]
pub enum Tier1CompileError {
    #[error("Tier-1 IR contains unsupported helper call: {helper}")]
    UnsupportedHelper { helper: String },
}

/// Output of the Tier-1 compilation pipeline.
#[derive(Debug, Clone)]
pub struct Tier1Compilation {
    pub entry_rip: u64,
    pub byte_len: u32,
    pub wasm_bytes: Vec<u8>,
    pub exit_to_interpreter: bool,
}

/// Compile a single basic block starting at `entry_rip` into a standalone WASM module.
pub fn compile_tier1_block<B: Tier1Bus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
) -> Result<Tier1Compilation, Tier1CompileError> {
    compile_tier1_block_with_options(bus, entry_rip, limits, Tier1WasmOptions::default())
}

/// Compile a single basic block starting at `entry_rip` into a standalone WASM module, using the
/// provided Tier-1 WASM codegen options.
pub fn compile_tier1_block_with_options<B: Tier1Bus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
    options: Tier1WasmOptions,
) -> Result<Tier1Compilation, Tier1CompileError> {
    let block = discover_block(bus, entry_rip, limits);
    let byte_len: u32 = block.insts.iter().map(|inst| inst.len as u32).sum();
    let ir = translate_block(&block);

    if let Some(helper) = ir.insts.iter().find_map(|inst| match inst {
        IrInst::CallHelper { helper, .. } => Some(helper),
        _ => None,
    }) {
        return Err(Tier1CompileError::UnsupportedHelper {
            helper: helper.to_string(),
        });
    }

    let exit_to_interpreter = matches!(ir.terminator, IrTerminator::ExitToInterpreter { .. });
    let wasm_bytes = Tier1WasmCodegen::new().compile_block_with_options(&ir, options);

    Ok(Tier1Compilation {
        entry_rip,
        byte_len,
        wasm_bytes,
        exit_to_interpreter,
    })
}
