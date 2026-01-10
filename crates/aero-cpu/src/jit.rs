use std::collections::{HashMap, HashSet};

use aero_jit::block::{BlockBuildError, BlockBuilder};
use aero_jit::cpu::CpuState;
use aero_jit::ir::{IrBlock, IrBuilder, IrOp, MemSize, ValueId};
use aero_jit::wasm::codegen_wasm;

use crate::interpreter::{ExecError, Interpreter};
use crate::memory::Memory;

#[derive(Clone, Debug)]
pub struct JitConfig {
    /// Number of executions of a given RIP before we attempt Tier-1 compilation.
    pub hot_threshold: u32,
    pub max_block_insts: usize,
    pub max_block_bytes: usize,
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            hot_threshold: 16,
            max_block_insts: 64,
            max_block_bytes: 512,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompiledBlock {
    pub start_rip: u64,
    pub ir: IrBlock,
    pub wasm: Vec<u8>,
    pub code_pages: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
struct CodeCache {
    by_rip: HashMap<u64, CompiledBlock>,
}

impl CodeCache {
    fn get(&self, rip: u64) -> Option<&CompiledBlock> {
        self.by_rip.get(&rip)
    }

    fn len(&self) -> usize {
        self.by_rip.len()
    }

    fn wasm(&self, rip: u64) -> Option<&[u8]> {
        self.by_rip.get(&rip).map(|b| b.wasm.as_slice())
    }

    fn insert(&mut self, block: CompiledBlock) {
        self.by_rip.insert(block.start_rip, block);
    }

    fn invalidate_all(&mut self) {
        self.by_rip.clear();
    }
}

/// The "CPU worker" that runs a tiered interpreter + baseline JIT.
#[derive(Clone, Debug)]
pub struct CpuWorker {
    pub cpu: CpuState,
    pub mem: Memory,

    interpreter: Interpreter,
    block_builder: BlockBuilder,
    ir_builder: IrBuilder,

    config: JitConfig,
    hotness: HashMap<u64, u32>,
    unjittable: HashSet<u64>,
    cache: CodeCache,
}

impl CpuWorker {
    pub fn new(mem: Memory) -> Self {
        let config = JitConfig::default();
        Self {
            cpu: CpuState::default(),
            mem,
            interpreter: Interpreter::default(),
            block_builder: BlockBuilder::new(config.max_block_insts, config.max_block_bytes),
            ir_builder: IrBuilder::default(),
            config,
            hotness: HashMap::new(),
            unjittable: HashSet::new(),
            cache: CodeCache::default(),
        }
    }

    pub fn with_config(mut self, config: JitConfig) -> Self {
        self.block_builder = BlockBuilder::new(config.max_block_insts, config.max_block_bytes);
        self.config = config;
        self
    }

    pub fn step(&mut self) -> Result<(), ExecError> {
        if self.cpu.is_halted() {
            return Ok(());
        }

        let rip = self.cpu.rip;

        // Hot path: already compiled.
        if let Some(block) = self.cache.get(rip) {
            let (next_rip, wrote_code_page) =
                exec_ir_block(&block.ir, &mut self.cpu, &mut self.mem);
            self.cpu.rip = next_rip;
            if wrote_code_page {
                self.cache.invalidate_all();
            }
            return Ok(());
        }

        // Cold path: increment hotness counter and optionally compile.
        if !self.unjittable.contains(&rip) {
            let count = self.hotness.entry(rip).or_insert(0);
            *count = count.saturating_add(1);
            if *count >= self.config.hot_threshold {
                self.try_compile_block(rip);
            }
        }

        // If compilation succeeded, execute the compiled block immediately.
        if let Some(block) = self.cache.get(rip) {
            let (next_rip, wrote_code_page) =
                exec_ir_block(&block.ir, &mut self.cpu, &mut self.mem);
            self.cpu.rip = next_rip;
            if wrote_code_page {
                self.cache.invalidate_all();
            }
            return Ok(());
        }

        let wrote_code_page = self.interpreter.step(&mut self.cpu, &mut self.mem)?;
        if wrote_code_page {
            self.cache.invalidate_all();
        }
        Ok(())
    }

    pub fn run(&mut self, max_steps: u64) -> Result<(), ExecError> {
        for _ in 0..max_steps {
            if self.cpu.is_halted() {
                break;
            }
            self.step()?;
        }
        Ok(())
    }

    pub fn code_cache_len(&self) -> usize {
        self.cache.len()
    }

    pub fn compiled_wasm(&self, rip: u64) -> Option<&[u8]> {
        self.cache.wasm(rip)
    }

    fn try_compile_block(&mut self, rip: u64) {
        if self.cache.get(rip).is_some() {
            return;
        }
        let block = match self.block_builder.build(&self.mem, rip) {
            Ok(b) => b,
            Err(BlockBuildError::UnsupportedTerminator) => {
                self.unjittable.insert(rip);
                return;
            }
            Err(_) => {
                self.unjittable.insert(rip);
                return;
            }
        };

        self.mem.mark_code_pages(&block.code_pages);

        let ir = match self.ir_builder.lower_basic_block(&block) {
            Ok(ir) => ir,
            Err(_) => {
                self.unjittable.insert(rip);
                return;
            }
        };

        let wasm = codegen_wasm(&ir);

        self.cache.insert(CompiledBlock {
            start_rip: rip,
            ir,
            wasm,
            code_pages: block.code_pages,
        });
    }
}

/// Execute an IR block in-process (Tier-1 baseline backend).
///
/// Returns `(next_rip, wrote_code_page)`, where `wrote_code_page` indicates that
/// a write hit a page previously marked as containing code (self-modifying
/// code), in which case the caller should invalidate the code cache.
fn exec_ir_block(block: &IrBlock, cpu: &mut CpuState, mem: &mut Memory) -> (u64, bool) {
    let mut wrote_code = false;
    let mut values: Vec<u64> = vec![0; block.value_types.len()];

    let mut next_value = 0u32;
    for op in &block.ops {
        let res = op.result_type().map(|_| {
            let id = ValueId(next_value);
            next_value += 1;
            id
        });

        let get = |values: &[u64], v: ValueId| values[v.as_usize()];
        let set = |values: &mut [u64], v: ValueId, val: u64| values[v.as_usize()] = val;

        match op {
            IrOp::I32Const(v) => {
                if let Some(r) = res {
                    set(&mut values, r, *v as u32 as u64);
                }
            }
            IrOp::I64Const(v) => {
                if let Some(r) = res {
                    set(&mut values, r, *v as u64);
                }
            }
            IrOp::I32Eqz { value } => {
                let v = get(&values, *value) as u32;
                if let Some(r) = res {
                    set(&mut values, r, if v == 0 { 1 } else { 0 });
                }
            }
            IrOp::I64Add { lhs, rhs } => {
                let v = get(&values, *lhs).wrapping_add(get(&values, *rhs));
                if let Some(r) = res {
                    set(&mut values, r, v);
                }
            }
            IrOp::I64Sub { lhs, rhs } => {
                let v = get(&values, *lhs).wrapping_sub(get(&values, *rhs));
                if let Some(r) = res {
                    set(&mut values, r, v);
                }
            }
            IrOp::I64ShlImm { value, shift } => {
                let v = get(&values, *value).wrapping_shl((*shift as u32) & 63);
                if let Some(r) = res {
                    set(&mut values, r, v);
                }
            }
            IrOp::I64And { lhs, rhs } => {
                let v = get(&values, *lhs) & get(&values, *rhs);
                if let Some(r) = res {
                    set(&mut values, r, v);
                }
            }
            IrOp::LoadReg64 { reg } => {
                if let Some(r) = res {
                    set(&mut values, r, cpu.reg(*reg));
                }
            }
            IrOp::StoreReg64 { reg, value } => cpu.set_reg(*reg, get(&values, *value)),
            IrOp::LoadMem { size, addr } => match size {
                MemSize::U64 => {
                    let a = get(&values, *addr);
                    let v = mem.read_u64(a);
                    if let Some(r) = res {
                        set(&mut values, r, v);
                    }
                }
                _ => {
                    if let Some(r) = res {
                        set(&mut values, r, 0);
                    }
                }
            },
            IrOp::StoreMem { size, addr, value } => match size {
                MemSize::U64 => {
                    let a = get(&values, *addr);
                    let v = get(&values, *value);
                    wrote_code |= mem.write_u64(a, v);
                }
                _ => {}
            },
            IrOp::SetPendingFlags {
                op,
                width_bits,
                lhs,
                rhs,
                result,
            } => cpu.set_pending_flags(
                *op,
                *width_bits,
                get(&values, *lhs),
                get(&values, *rhs),
                get(&values, *result),
            ),
            IrOp::GetFlag { flag } => {
                let v = cpu.read_flag(*flag);
                if let Some(r) = res {
                    set(&mut values, r, if v { 1 } else { 0 });
                }
            }
            IrOp::SelectI64 {
                cond,
                if_true,
                if_false,
            } => {
                let c = get(&values, *cond) as u32;
                let v = if c != 0 {
                    get(&values, *if_true)
                } else {
                    get(&values, *if_false)
                };
                if let Some(r) = res {
                    set(&mut values, r, v);
                }
            }
            IrOp::SetHalted => cpu.set_halted(),
            IrOp::Return { next_rip } => return (get(&values, *next_rip), wrote_code),
        }
    }

    (cpu.rip, wrote_code)
}
