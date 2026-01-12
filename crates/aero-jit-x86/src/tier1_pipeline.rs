//! Tier-1 compilation pipeline glue for [`aero_cpu_core::jit::runtime::JitRuntime`].
//!
//! This module defines the "canonical" Tier-1 plumbing used by embedders:
//! - [`Tier1CompileQueue`]: a de-duplicating [`CompileRequestSink`] implementation that can be
//!   drained by a driver/worker thread.
//! - [`Tier1Compiler`]: compiles a single x86 basic block into WASM, registers it into a
//!   [`Tier1WasmRegistry`], and returns a [`CompiledBlockHandle`] suitable for
//!   [`JitRuntime::install_handle`].
//!
//! `Tier1Compiler` snapshots page-version metadata via [`JitRuntime::snapshot_meta`] *before* it
//! reads guest code bytes, and then shrinks the snapshot to the actual discovered block length.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitRuntime, PAGE_SHIFT};

use crate::compiler::tier1::compile_tier1_block_with_options;
use crate::tier1::{BlockLimits, Tier1WasmOptions};

pub use crate::compiler::tier1::Tier1CompileError;

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
        let mut inner = self.inner.lock().expect("Tier1CompileQueue mutex poisoned");
        let rip = inner.queue.pop_front()?;
        inner.pending.remove(&rip);
        Some(rip)
    }

    /// Drain all pending compilation requests.
    pub fn drain(&self) -> Vec<u64> {
        let mut inner = self.inner.lock().expect("Tier1CompileQueue mutex poisoned");
        let drained: Vec<u64> = inner.queue.drain(..).collect();
        inner.pending.clear();
        drained
    }
}

impl CompileRequestSink for Tier1CompileQueue {
    fn request_compile(&mut self, entry_rip: u64) {
        let mut inner = self.inner.lock().expect("Tier1CompileQueue mutex poisoned");
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

/// Tier-1 compilation pipeline for a single basic block.
pub struct Tier1Compiler<P, R> {
    provider: P,
    registry: R,
    limits: BlockLimits,
    wasm_options: Tier1WasmOptions,
}

impl<P, R> Tier1Compiler<P, R> {
    pub fn new(provider: P, registry: R) -> Self {
        Self {
            provider,
            registry,
            limits: BlockLimits::default(),
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

    /// Configure whether inline-TLB mode is allowed to emit direct guest RAM stores.
    ///
    /// When disabled, Tier-1 stores always use the imported slow helpers (`env.mem_write_*`), which
    /// allows the embedder/runtime to observe writes (MMIO classification, self-modifying code
    /// invalidation, etc).
    #[must_use]
    pub fn with_inline_tlb_stores(mut self, inline_tlb_stores: bool) -> Self {
        self.wasm_options.inline_tlb_stores = inline_tlb_stores;
        self
    }
}

impl<P, R> Tier1Compiler<P, R>
where
    P: crate::Tier1Bus,
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
        bitness: u32,
    ) -> Result<CompiledBlockHandle, Tier1CompileError>
    where
        B: JitBackend,
        C: CompileRequestSink,
    {
        // For Tier-1 bring-up we treat code_paddr=rip. Higher layers can replace this once a real
        // RIP→PADDR mapping exists.
        let code_paddr = entry_rip;
        let snapshot_len = snapshot_len_for_limits(self.limits);
        let pre_meta: CompiledBlockMeta = jit.snapshot_meta(code_paddr, snapshot_len);

        let compilation = compile_tier1_block_with_options(
            &self.provider,
            entry_rip,
            bitness,
            self.limits,
            self.wasm_options,
        )?;
        let mut meta = shrink_meta(pre_meta, compilation.byte_len);
        meta.instruction_count = compilation.instruction_count;
        meta.inhibit_interrupts_after_block = false;

        let table_index = self
            .registry
            .register_tier1_block(compilation.wasm_bytes, compilation.exit_to_interpreter);

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
        bitness: u32,
    ) -> Result<Vec<u64>, Tier1CompileError>
    where
        B: JitBackend,
        C: CompileRequestSink,
    {
        let handle = self.compile_handle(jit, entry_rip, bitness)?;
        Ok(jit.install_handle(handle))
    }
}

fn snapshot_len_for_limits(limits: BlockLimits) -> u32 {
    // `discover_block` fetches 15 bytes per instruction; when close to `max_bytes` we can read a
    // little past the limit for decoder lookahead.
    let max_bytes = u32::try_from(limits.max_bytes).unwrap_or(u32::MAX);
    max_bytes.saturating_add(15)
}

fn shrink_meta(mut meta: CompiledBlockMeta, byte_len: u32) -> CompiledBlockMeta {
    meta.byte_len = byte_len;
    meta.page_versions = shrink_page_versions(meta.code_paddr, byte_len, meta.page_versions);
    meta
}

fn shrink_page_versions(
    code_paddr: u64,
    byte_len: u32,
    mut page_versions: Vec<PageVersionSnapshot>,
) -> Vec<PageVersionSnapshot> {
    if byte_len == 0 {
        page_versions.clear();
        return page_versions;
    }

    let start_page = code_paddr >> PAGE_SHIFT;
    let end = code_paddr.saturating_add(byte_len as u64 - 1);
    let end_page = end >> PAGE_SHIFT;

    page_versions.retain(|snap| snap.page >= start_page && snap.page <= end_page);
    page_versions
}
