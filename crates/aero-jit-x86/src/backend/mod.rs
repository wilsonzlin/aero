//! Native (non-wasm32) runtime backend for executing dynamically-generated WASM blocks.
//!
//! This module provides:
//! - A reference `wasmtime`-powered Tier-1 executor (`WasmtimeBackend`)
//! - A shareable wrapper (`WasmBackend`) suitable for driving
//!   `aero_cpu_core::jit::runtime::JitRuntime` from a compile worker
//! - Convenience wrappers for Tier-1 compilation (`compile_and_install`)
//!
//! The canonical Tier-1 compilation pipeline + compile request queue live in
//! [`crate::tier1::pipeline`]. The helpers in this module are thin wrappers
//! around that API for consumers that already depend on `WasmBackend`.

use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{CompileRequestSink, JitBackend, JitRuntime};
use aero_cpu_core::state::CpuState as CoreCpuState;

use crate::tier1::Tier1WasmOptions;
use crate::tier1_pipeline::{Tier1Compiler, Tier1WasmRegistry};
use crate::Tier1Bus;

/// Minimal interface a host CPU type must expose to execute Tier-1 WASM blocks.
///
/// The Tier-1 WASM ABI uses the in-memory layout of [`aero_cpu_core::state::CpuState`] (plus
/// optional JIT-only context data appended after the struct). The backend copies the architectural
/// subset used by Tier-1 (GPRs/RIP/RFLAGS) into the shared `WebAssembly.Memory`, calls the compiled
/// block, and then copies the updated values back into the host CPU value.
pub trait Tier1Cpu {
    fn tier1_state(&self) -> &CoreCpuState;
    fn tier1_state_mut(&mut self) -> &mut CoreCpuState;
}

impl Tier1Cpu for CoreCpuState {
    fn tier1_state(&self) -> &CoreCpuState {
        self
    }

    fn tier1_state_mut(&mut self) -> &mut CoreCpuState {
        self
    }
}

mod wasmtime;

pub use wasmtime::WasmtimeBackend;

/// A cloneable handle around [`WasmtimeBackend`] so compilation workers can add table entries while
/// the [`JitRuntime`] owns a copy of the backend.
///
/// `WasmBackend` also implements [`Tier1Bus`], allowing the Tier-1 compiler pipeline to read guest
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

impl<Cpu> Tier1Bus for WasmBackend<Cpu> {
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

/// Compile the Tier-1 block starting at `entry_rip`, instantiate it in `backend`, and return a
/// cache-installable [`CompiledBlockHandle`].
///
/// The returned handle is *not* installed into `runtime`; callers should pass it to
/// [`JitRuntime::install_handle`].
pub fn compile_and_install<Cpu, C>(
    backend: &mut WasmBackend<Cpu>,
    runtime: &JitRuntime<WasmBackend<Cpu>, C>,
    entry_rip: u64,
    bitness: u32,
) -> CompiledBlockHandle
where
    Cpu: Tier1Cpu,
    C: CompileRequestSink,
{
    compile_and_install_with_options(
        backend,
        runtime,
        entry_rip,
        bitness,
        Tier1WasmOptions::default(),
    )
}

/// Same as [`compile_and_install`], but allows selecting Tier-1 WASM codegen options (e.g. enabling
/// the inline-TLB fast-path).
pub fn compile_and_install_with_options<Cpu, C>(
    backend: &mut WasmBackend<Cpu>,
    runtime: &JitRuntime<WasmBackend<Cpu>, C>,
    entry_rip: u64,
    bitness: u32,
    options: Tier1WasmOptions,
) -> CompiledBlockHandle
where
    Cpu: Tier1Cpu,
    C: CompileRequestSink,
{
    Tier1Compiler::new(backend.clone(), backend.clone())
        .with_wasm_options(options)
        .compile_handle(runtime, entry_rip, bitness)
        .expect("Tier-1 compilation failed")
}
