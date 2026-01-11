//! Native (non-wasm32) runtime backend for executing dynamically-generated WASM blocks.
//!
//! This module provides:
//! - A reference `wasmtime`-powered Tier-1 executor (`WasmtimeBackend`)
//! - A shareable wrapper (`WasmBackend`) suitable for driving
//!   `aero_cpu_core::jit::runtime::JitRuntime` from a compile worker
//! - Minimal plumbing helpers (`CompileQueue`, `compile_and_install`)

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_cpu::{CpuBus, CpuState};
use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitRuntime, PAGE_SHIFT};

use crate::compiler::tier1::compile_tier1_block;
use crate::tier1_pipeline::Tier1WasmRegistry;
use crate::BlockLimits;

/// Minimal interface a host CPU type must expose to execute Tier-1 WASM blocks.
///
/// The Tier-1 WASM ABI uses the in-memory layout of [`aero_cpu_core::state::CpuState`]. The backend
/// copies the host CPU state into the shared `WebAssembly.Memory`, calls the compiled block, and
/// then copies the updated state back into the host CPU value.
///
/// Note: the current trait is intentionally narrow (GPRs/RIP/RFLAGS only) and is used by unit tests
/// that wrap the lightweight `aero_cpu::CpuState`. Full-system integration uses the canonical
/// `aero_cpu_core::state::CpuState` layout through [`crate::abi`].
pub trait Tier1Cpu {
    fn tier1_state(&self) -> &CpuState;
    fn tier1_state_mut(&mut self) -> &mut CpuState;
}

impl Tier1Cpu for CpuState {
    fn tier1_state(&self) -> &CpuState {
        self
    }

    fn tier1_state_mut(&mut self) -> &mut CpuState {
        self
    }
}

mod wasmtime;

pub use wasmtime::WasmtimeBackend;

/// A cloneable handle around [`WasmtimeBackend`] so compilation workers can add table entries while
/// the [`JitRuntime`] owns a copy of the backend.
///
/// `WasmBackend` also implements [`CpuBus`], allowing the Tier-1 compiler pipeline to read guest
/// code bytes directly from the backend's shared linear memory.
pub struct WasmBackend<Cpu>(Rc<RefCell<WasmtimeBackend<Cpu>>>);

impl<Cpu> Clone for WasmBackend<Cpu> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<Cpu> Default for WasmBackend<Cpu> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Cpu> WasmBackend<Cpu> {
    #[must_use]
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(WasmtimeBackend::new())))
    }

    #[must_use]
    pub fn with_memory_pages(memory_pages: u32, cpu_ptr: i32) -> Self {
        Self(Rc::new(RefCell::new(
            WasmtimeBackend::new_with_memory_pages(memory_pages, cpu_ptr),
        )))
    }

    pub fn add_compiled_block(&mut self, wasm_bytes: &[u8]) -> u32 {
        self.0.borrow_mut().add_compiled_block(wasm_bytes)
    }
}

impl<Cpu> Tier1WasmRegistry for WasmBackend<Cpu> {
    fn register_tier1_block(&mut self, wasm: Vec<u8>, _exit_to_interpreter: bool) -> u32 {
        self.add_compiled_block(&wasm)
    }
}

impl<Cpu> CpuBus for WasmBackend<Cpu> {
    fn read_u8(&self, addr: u64) -> u8 {
        self.0.borrow().read_u8(addr)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.0.borrow_mut().write_u8(addr, value);
    }
}

impl<Cpu> JitBackend for WasmBackend<Cpu>
where
    Cpu: Tier1Cpu,
{
    type Cpu = Cpu;

    fn execute(
        &mut self,
        table_index: u32,
        cpu: &mut Self::Cpu,
    ) -> aero_cpu_core::jit::runtime::JitBlockExit {
        self.0.borrow_mut().execute(table_index, cpu)
    }
}

/// A simple FIFO queue of compile requests emitted by [`aero_cpu_core::jit::runtime::JitRuntime`].
///
/// The runtime owns a [`CompileRequestSink`] by value, so this uses interior mutability to allow a
/// driver thread to drain the queue out-of-band.
#[derive(Clone, Default)]
pub struct CompileQueue(Rc<RefCell<VecDeque<u64>>>);

impl CompileQueue {
    /// Drain all pending compile requests in FIFO order.
    #[must_use]
    pub fn drain(&self) -> Vec<u64> {
        self.0.borrow_mut().drain(..).collect()
    }

    /// Snapshot the currently queued RIPs without clearing them.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u64> {
        self.0.borrow().iter().copied().collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.borrow().is_empty()
    }
}

impl CompileRequestSink for CompileQueue {
    fn request_compile(&mut self, entry_rip: u64) {
        self.0.borrow_mut().push_back(entry_rip);
    }
}

/// Compile the Tier-1 block starting at `entry_rip`, instantiate it in `backend`, and return a
/// cache-installable [`CompiledBlockHandle`].
///
/// The returned handle is *not* installed into `runtime`; callers should pass it to
/// [`JitRuntime::install_handle`].
pub fn compile_and_install<Cpu, C>(
    backend: &mut WasmBackend<Cpu>,
    runtime: &JitRuntime<WasmBackend<Cpu>, C>,
    entry_rip: u64,
) -> CompiledBlockHandle
where
    Cpu: Tier1Cpu,
    C: CompileRequestSink,
{
    let limits = BlockLimits::default();

    // Capture page-version metadata *before* reading guest code. We snapshot the full discovery
    // range (plus decoder lookahead) and then shrink it to the actual byte length after block
    // formation.
    let snapshot_len = snapshot_len_for_limits(limits);
    let pre_meta = runtime.snapshot_meta(entry_rip, snapshot_len);

    let compilation = compile_tier1_block(backend, entry_rip, limits);
    let table_index = backend.add_compiled_block(&compilation.wasm_bytes);

    CompiledBlockHandle {
        entry_rip,
        table_index,
        meta: shrink_meta(pre_meta, compilation.byte_len),
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
    let end = code_paddr
        .checked_add(byte_len as u64 - 1)
        .unwrap_or(u64::MAX);
    let end_page = end >> PAGE_SHIFT;

    page_versions.retain(|snap| snap.page >= start_page && snap.page <= end_page);
    page_versions
}
