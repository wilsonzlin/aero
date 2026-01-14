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
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitRuntime, PageVersionTracker, PAGE_SHIFT,
};

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

    /// Configure whether inline-TLB mode is allowed to take the cross-page (4KiB boundary-crossing)
    /// RAM fast-path for wide unaligned loads/stores.
    #[must_use]
    pub fn with_inline_tlb_cross_page_fastpath(
        mut self,
        inline_tlb_cross_page_fastpath: bool,
    ) -> Self {
        self.wasm_options.inline_tlb_cross_page_fastpath = inline_tlb_cross_page_fastpath;
        self
    }

    /// Configure whether inline-TLB mode treats non-RAM translations as MMIO exits (`jit_exit_mmio`)
    /// or falls back to the imported slow memory helpers.
    #[must_use]
    pub fn with_inline_tlb_mmio_exit(mut self, inline_tlb_mmio_exit: bool) -> Self {
        self.wasm_options.inline_tlb_mmio_exit = inline_tlb_mmio_exit;
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
    #[track_caller]
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
        let ip_mask = ip_mask_for_bitness(bitness);
        let pre_meta: CompiledBlockMeta = if ip_mask == u64::MAX {
            jit.snapshot_meta(code_paddr, snapshot_len)
        } else {
            CompiledBlockMeta {
                code_paddr,
                byte_len: snapshot_len,
                page_versions_generation: jit.page_versions().generation(),
                page_versions: snapshot_page_versions_wrapping(
                    jit.page_versions(),
                    code_paddr,
                    snapshot_len,
                    ip_mask,
                ),
                instruction_count: 0,
                inhibit_interrupts_after_block: false,
            }
        };

        let compilation = compile_tier1_block_with_options(
            &self.provider,
            entry_rip,
            bitness,
            self.limits,
            self.wasm_options,
        )?;
        let mut meta = shrink_meta(pre_meta, compilation.byte_len, ip_mask);
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

    #[track_caller]
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

fn shrink_meta(mut meta: CompiledBlockMeta, byte_len: u32, ip_mask: u64) -> CompiledBlockMeta {
    meta.byte_len = byte_len;
    meta.page_versions = shrink_page_versions(meta.code_paddr, byte_len, meta.page_versions, ip_mask);
    meta
}

fn shrink_page_versions(
    code_paddr: u64,
    byte_len: u32,
    mut page_versions: Vec<PageVersionSnapshot>,
    ip_mask: u64,
) -> Vec<PageVersionSnapshot> {
    if byte_len == 0 {
        page_versions.clear();
        return page_versions;
    }

    let start = code_paddr & ip_mask;
    let start_page = start >> PAGE_SHIFT;
    if ip_mask == u64::MAX {
        let end = start.saturating_add(byte_len as u64 - 1);
        let end_page = end >> PAGE_SHIFT;
        page_versions.retain(|snap| snap.page >= start_page && snap.page <= end_page);
        return page_versions;
    }

    let max_page = ip_mask >> PAGE_SHIFT;
    let end = start.saturating_add(byte_len as u64 - 1) & ip_mask;
    let end_page = end >> PAGE_SHIFT;
    if start_page <= end_page {
        page_versions.retain(|snap| snap.page >= start_page && snap.page <= end_page);
    } else {
        page_versions.retain(|snap| {
            (snap.page >= start_page && snap.page <= max_page) || snap.page <= end_page
        });
    }
    page_versions
}

#[track_caller]
fn ip_mask_for_bitness(bitness: u32) -> u64 {
    match bitness {
        32 => 0xffff_ffff,
        64 => u64::MAX,
        // Tier-1 only partially models 16-bit mode, but keep metadata consistent with the
        // architectural IP width for callers that use `bitness=16`.
        16 => 0xffff,
        other => panic!("invalid x86 bitness {other}"),
    }
}

fn snapshot_page_versions_wrapping(
    tracker: &PageVersionTracker,
    code_paddr: u64,
    byte_len: u32,
    ip_mask: u64,
) -> Vec<PageVersionSnapshot> {
    if byte_len == 0 {
        return Vec::new();
    }

    let pages = spanned_pages_wrapping(
        code_paddr,
        byte_len,
        ip_mask,
        PageVersionTracker::MAX_SNAPSHOT_PAGES,
    );

    pages
        .into_iter()
        .map(|page| PageVersionSnapshot {
            page,
            version: tracker.version(page),
        })
        .collect()
}

fn spanned_pages_wrapping(
    code_paddr: u64,
    byte_len: u32,
    ip_mask: u64,
    max_pages: usize,
) -> Vec<u64> {
    if byte_len == 0 || max_pages == 0 {
        return Vec::new();
    }
    let len = u64::from(byte_len);

    let start = code_paddr & ip_mask;
    let start_page = start >> PAGE_SHIFT;

    if ip_mask == u64::MAX {
        let end = start.saturating_add(len.saturating_sub(1));
        let end_page = end >> PAGE_SHIFT;
        let page_count = end_page.saturating_sub(start_page).saturating_add(1);
        let clamped_pages = page_count.min(max_pages as u64);
        if clamped_pages == 0 {
            return Vec::new();
        }
        let clamped_end_page = start_page.saturating_add(clamped_pages - 1);
        return (start_page..=clamped_end_page).collect();
    }

    let max_page = ip_mask >> PAGE_SHIFT;
    let end_unmasked = start.saturating_add(len.saturating_sub(1));
    let end = end_unmasked & ip_mask;
    let end_page = end >> PAGE_SHIFT;

    let mut out = Vec::new();
    if start_page <= end_page {
        for page in start_page..=end_page {
            out.push(page);
            if out.len() >= max_pages {
                break;
            }
        }
    } else {
        for page in start_page..=max_page {
            out.push(page);
            if out.len() >= max_pages {
                return out;
            }
        }
        for page in 0..=end_page {
            out.push(page);
            if out.len() >= max_pages {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::capture_panic_location;

    use aero_cpu_core::jit::runtime::{JitBlockExit, JitConfig};

    #[derive(Default)]
    struct DummyCompileSink;

    impl CompileRequestSink for DummyCompileSink {
        fn request_compile(&mut self, _entry_rip: u64) {}
    }

    #[derive(Default)]
    struct DummyBackend;

    impl JitBackend for DummyBackend {
        type Cpu = ();

        fn execute(&mut self, _table_index: u32, _cpu: &mut Self::Cpu) -> JitBlockExit {
            JitBlockExit {
                next_rip: 0,
                exit_to_interpreter: true,
                committed: false,
            }
        }
    }

    #[derive(Default)]
    struct DummyBus;

    impl crate::Tier1Bus for DummyBus {
        fn read_u8(&self, _addr: u64) -> u8 {
            0
        }

        fn write_u8(&mut self, _addr: u64, _value: u8) {}
    }

    #[derive(Default)]
    struct DummyRegistry;

    impl Tier1WasmRegistry for DummyRegistry {
        fn register_tier1_block(&mut self, _wasm: Vec<u8>, _exit_to_interpreter: bool) -> u32 {
            0
        }
    }

    #[test]
    fn tier1_compiler_compile_handle_panics_at_call_site_on_invalid_bitness() {
        let jit = JitRuntime::new(JitConfig::default(), DummyBackend, DummyCompileSink);
        let mut compiler = Tier1Compiler::new(DummyBus, DummyRegistry);

        let expected_file = file!();
        let expected_line = line!() + 2;
        let (file, line) = capture_panic_location(|| {
            let _ = compiler.compile_handle(&jit, 0, 0);
        });
        assert_eq!(file, expected_file);
        assert_eq!(line, expected_line);
    }
}
