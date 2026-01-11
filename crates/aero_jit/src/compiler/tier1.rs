use aero_cpu::CpuBus;

use crate::{discover_block, translate_block, BlockLimits};
use crate::wasm::tier1::Tier1WasmOptions;

/// Output of the Tier-1 compilation pipeline.
#[derive(Debug, Clone)]
pub struct Tier1Compilation {
    pub entry_rip: u64,
    pub byte_len: u32,
    pub wasm_bytes: Vec<u8>,
}

/// Compile a single basic block starting at `entry_rip` into a standalone WASM module.
#[must_use]
pub fn compile_tier1_block<B: CpuBus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
) -> Tier1Compilation {
    compile_tier1_block_with_options(bus, entry_rip, limits, Tier1WasmOptions::default())
}

/// Compile a single basic block starting at `entry_rip` into a standalone WASM module, using the
/// provided Tier-1 WASM codegen options.
#[must_use]
pub fn compile_tier1_block_with_options<B: CpuBus>(
    bus: &B,
    entry_rip: u64,
    limits: BlockLimits,
    options: Tier1WasmOptions,
) -> Tier1Compilation {
    let block = discover_block(bus, entry_rip, limits);
    let byte_len: u32 = block.insts.iter().map(|inst| inst.len as u32).sum();
    let ir = translate_block(&block);

    let wasm_bytes = crate::wasm::tier1::Tier1WasmCodegen::new()
        .compile_block_with_options(&ir, options);

    Tier1Compilation {
        entry_rip,
        byte_len,
        wasm_bytes,
    }
}
