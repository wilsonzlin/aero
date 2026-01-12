//! Pure Tier-1 compilation helpers.
//!
//! These helpers compile a single x86 basic block into a standalone WASM module. For integrating
//! with [`aero_cpu_core::jit::runtime::JitRuntime`], prefer [`crate::tier1::pipeline::Tier1Compiler`]
//! which handles page-version metadata snapshots and WASM registration.

use thiserror::Error;

use crate::tier1::ir::{IrInst, IrTerminator, SideEffects};
use crate::tier1::{
    discover_block, translate_block, BlockLimits, Tier1WasmCodegen, Tier1WasmOptions,
};
use crate::Tier1Bus;
use aero_x86::tier1::InstKind;

#[derive(Debug, Error)]
pub enum Tier1CompileError {
    #[error("Tier-1 IR contains unsupported helper call: {helper}")]
    UnsupportedHelper { helper: String },
    #[error("Tier-1 block bails out at entry RIP 0x{entry_rip:x} (zero progress)")]
    BailoutAtEntry { entry_rip: u64 },
}

/// Output of the Tier-1 compilation pipeline.
#[derive(Debug, Clone)]
pub struct Tier1Compilation {
    pub entry_rip: u64,
    pub byte_len: u32,
    /// Number of architectural guest instructions executed by the block.
    pub instruction_count: u32,
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
    let instruction_count = {
        let mut count = u32::try_from(block.insts.len()).unwrap_or(u32::MAX);
        if matches!(block.insts.last().map(|inst| &inst.kind), Some(InstKind::Invalid)) {
            count = count.saturating_sub(1);
        }
        count
    };
    let ir = translate_block(&block);

    if let Some(helper) = ir.insts.iter().find_map(|inst| match inst {
        IrInst::CallHelper { helper, .. } => Some(helper),
        _ => None,
    }) {
        return Err(Tier1CompileError::UnsupportedHelper {
            helper: helper.to_string(),
        });
    }

    // After fixing Tier-1 Invalid semantics to side-exit at `inst.rip`, it's possible for the
    // front-end to produce blocks that *immediately* exit to the interpreter at their entry RIP
    // without executing any meaningful work (e.g. unsupported first instruction). Installing such
    // blocks into the JIT cache causes pure overhead: every dispatch bounces into WASM only to
    // immediately request an interpreter step.
    //
    // Treat these as "non-compilable" by returning a dedicated error; higher layers may leave the
    // compile request marked as satisfied to avoid re-requesting indefinitely.
    if matches!(
        &ir.terminator,
        IrTerminator::ExitToInterpreter { next_rip } if *next_rip == entry_rip
    ) && ir
        .insts
        .iter()
        .all(|inst| inst.side_effects() == SideEffects::NONE)
    {
        return Err(Tier1CompileError::BailoutAtEntry { entry_rip });
    }

    let exit_to_interpreter = matches!(&ir.terminator, IrTerminator::ExitToInterpreter { .. });
    let wasm_bytes = Tier1WasmCodegen::new().compile_block_with_options(&ir, options);

    Ok(Tier1Compilation {
        entry_rip,
        byte_len,
        instruction_count,
        wasm_bytes,
        exit_to_interpreter,
    })
}
