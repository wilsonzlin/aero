use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta};
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitRuntime};
use aero_x86::tier1::{decode_one, InstKind};
use thiserror::Error;

use crate::tier1_ir::{IrInst, IrTerminator};
use crate::wasm::tier1::{Tier1WasmCodegen, Tier1WasmOptions};
use crate::{translate_block, BasicBlock, BlockEndKind, BlockLimits};

/// Source of guest code bytes for the Tier-1 compiler.
pub trait CodeProvider {
    fn fetch(&self, rip: u64, max: usize) -> Vec<u8>;
}

impl<T: aero_cpu::CpuBus> CodeProvider for T {
    fn fetch(&self, rip: u64, max: usize) -> Vec<u8> {
        aero_cpu::CpuBus::fetch(self, rip, max)
    }
}

/// Queue-based Tier-1 compilation sink.
///
/// This implements [`CompileRequestSink`] so it can be installed directly into
/// [`aero_cpu_core::jit::runtime::JitRuntime`]. Requests are de-duplicated on
/// entry RIP.
#[derive(Debug, Default, Clone)]
pub struct Tier1CompileQueue {
    inner: Arc<Mutex<QueueInner>>,
}

#[derive(Debug, Default)]
struct QueueInner {
    queue: VecDeque<u64>,
    pending: HashSet<u64>,
}

impl Tier1CompileQueue {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("Tier1CompileQueue mutex poisoned")
            .queue
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Pop a single pending compilation request.
    pub fn pop(&self) -> Option<u64> {
        let mut inner = self
            .inner
            .lock()
            .expect("Tier1CompileQueue mutex poisoned");
        let rip = inner.queue.pop_front()?;
        inner.pending.remove(&rip);
        Some(rip)
    }

    /// Drain all pending compilation requests.
    pub fn drain(&self) -> Vec<u64> {
        let mut inner = self
            .inner
            .lock()
            .expect("Tier1CompileQueue mutex poisoned");
        let drained: Vec<u64> = inner.queue.drain(..).collect();
        inner.pending.clear();
        drained
    }
}

impl CompileRequestSink for Tier1CompileQueue {
    fn request_compile(&mut self, entry_rip: u64) {
        let mut inner = self
            .inner
            .lock()
            .expect("Tier1CompileQueue mutex poisoned");
        if inner.pending.insert(entry_rip) {
            inner.queue.push_back(entry_rip);
        }
    }
}

/// Backend hook used by the Tier-1 compiler to install newly compiled WASM.
///
/// The returned `u32` is treated as a stable table index used by
/// [`aero_cpu_core::jit::runtime::JitBackend::execute`].
pub trait Tier1WasmRegistry {
    fn register_tier1_block(&mut self, wasm: Vec<u8>, exit_to_interpreter: bool) -> u32;
}

#[derive(Debug, Error)]
pub enum Tier1CompileError {
    #[error("Tier-1 IR contains unsupported helper call: {helper}")]
    UnsupportedHelper { helper: String },
}

/// Tier-1 compilation pipeline for a single basic block.
pub struct Tier1Compiler<P, R> {
    provider: P,
    registry: R,
    limits: BlockLimits,
    codegen: Tier1WasmCodegen,
    wasm_options: Tier1WasmOptions,
}

impl<P, R> Tier1Compiler<P, R> {
    pub fn new(provider: P, registry: R) -> Self {
        Self {
            provider,
            registry,
            limits: BlockLimits::default(),
            codegen: Tier1WasmCodegen::new(),
            wasm_options: Tier1WasmOptions::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: BlockLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_wasm_options(mut self, options: Tier1WasmOptions) -> Self {
        self.wasm_options = options;
        self
    }

    #[must_use]
    pub fn with_inline_tlb(mut self, inline_tlb: bool) -> Self {
        self.wasm_options.inline_tlb = inline_tlb;
        self
    }
}

impl<P, R> Tier1Compiler<P, R>
where
    P: CodeProvider,
    R: Tier1WasmRegistry,
{
    /// Compile a block to a [`CompiledBlockHandle`].
    ///
    /// The returned handle embeds a snapshot of the runtime's page-version state at the time of
    /// compilation. Installing this handle after the guest modifies the underlying code bytes will
    /// cause the runtime to reject it and request recompilation.
    pub fn compile_handle<B, C>(
        &mut self,
        jit: &JitRuntime<B, C>,
        entry_rip: u64,
    ) -> Result<CompiledBlockHandle, Tier1CompileError>
    where
        B: JitBackend,
        C: CompileRequestSink,
    {
        let block = discover_block_from_provider(&self.provider, entry_rip, self.limits);
        let byte_len: u32 = block.insts.iter().map(|inst| inst.len as u32).sum();

        // For Tier-1 bring-up we treat code_paddr=rip. Higher layers can replace this once a real
        // RIP→PADDR mapping exists.
        let code_paddr = entry_rip;
        let meta: CompiledBlockMeta = jit.snapshot_meta(code_paddr, byte_len);

        let ir = translate_block(&block);
        if let Some(helper) = ir.insts.iter().find_map(|inst| match inst {
            IrInst::CallHelper { helper, .. } => Some(*helper),
            _ => None,
        }) {
            return Err(Tier1CompileError::UnsupportedHelper {
                helper: helper.to_string(),
            });
        }

        let exit_to_interpreter = matches!(ir.terminator, IrTerminator::ExitToInterpreter { .. });
        let wasm = self
            .codegen
            .compile_block_with_options(&ir, self.wasm_options);
        let table_index = self
            .registry
            .register_tier1_block(wasm, exit_to_interpreter);

        Ok(CompiledBlockHandle {
            entry_rip,
            table_index,
            meta,
        })
    }

    pub fn compile_and_install<B, C>(
        &mut self,
        jit: &mut JitRuntime<B, C>,
        entry_rip: u64,
    ) -> Result<Vec<u64>, Tier1CompileError>
    where
        B: JitBackend,
        C: CompileRequestSink,
    {
        let handle = self.compile_handle(jit, entry_rip)?;
        Ok(jit.install_handle(handle))
    }
}

fn discover_block_from_provider<P: CodeProvider>(
    provider: &P,
    entry_rip: u64,
    limits: BlockLimits,
) -> BasicBlock {
    let mut insts = Vec::new();
    let mut rip = entry_rip;
    let mut total_bytes = 0usize;

    loop {
        if insts.len() >= limits.max_insts || total_bytes >= limits.max_bytes {
            return BasicBlock {
                entry_rip,
                insts,
                end_kind: BlockEndKind::Limit { next_rip: rip },
            };
        }

        let bytes = provider.fetch(rip, 15);
        let inst = decode_one(rip, &bytes);
        total_bytes += inst.len as usize;
        rip = inst.next_rip();

        let end_kind = match inst.kind {
            InstKind::JmpRel { .. } => Some(BlockEndKind::Jmp),
            InstKind::JccRel { .. } => Some(BlockEndKind::Jcc),
            InstKind::CallRel { .. } => Some(BlockEndKind::Call),
            InstKind::Ret => Some(BlockEndKind::Ret),
            InstKind::Invalid => Some(BlockEndKind::ExitToInterpreter {
                next_rip: inst.next_rip(),
            }),
            _ => None,
        };

        insts.push(inst);

        if let Some(kind) = end_kind {
            return BasicBlock {
                entry_rip,
                insts,
                end_kind: kind,
            };
        }
    }
}
