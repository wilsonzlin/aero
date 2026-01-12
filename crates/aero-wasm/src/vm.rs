//! Minimal x86 VM loop for the browser worker runtime.
//!
//! This module is intentionally small: it wires the Tier-0 interpreter
//! (`aero_cpu_core`) to the injected shared guest RAM inside the module's linear
//! memory and routes port I/O back to JS.
//!
//! The browser CPU worker installs `globalThis.__aero_io_port_read` /
//! `globalThis.__aero_io_port_write` shims which forward to the I/O worker via
//! the existing AIPC rings.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::{Object, Reflect, Uint32Array, Uint8Array};

use aero_cpu_core::{
    CpuBus, CpuCore, Exception, PagingBus,
    assist::AssistContext,
    exec::{ExecCpu as _, Interpreter as _, Tier0Interpreter, Vcpu},
    interp::tier0::{
        Tier0Config,
        exec::{BatchExit, run_batch_cpu_core_with_assists},
    },
    jit::runtime::{CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime},
    state::{CpuMode, Segment},
};
use aero_mmu::{MemoryBus, Mmu};

use crate::{RunExit, RunExitKind};

use std::cell::RefCell;
use std::rc::Rc;

fn js_error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

/// Upper bound for snapshot blobs passed into `load_state_v2`.
///
/// CPU/MMU state is typically <2KiB, but the v2 CPU encoding has a length-prefixed extension
/// mechanism that allows for forward-compatible growth. We keep the limit reasonably generous so
/// future additions do not immediately break older runtimes, while still guarding against
/// accidental OOMs from absurd inputs.
const MAX_STATE_BLOB_LEN: usize = 4 * 1024 * 1024;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_io_port_read)]
    fn js_io_port_read(port: u32, size: u32) -> u32;

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_io_port_write)]
    fn js_io_port_write(port: u32, size: u32, value: u32);

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_jit_call)]
    fn js_jit_call(table_index: u32, cpu_ptr: u32, jit_ctx_ptr: u32) -> i64;
}

fn wasm_memory_byte_len() -> u64 {
    // `memory_size(0)` returns the number of 64KiB wasm pages.
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

#[derive(Clone, Copy)]
struct WasmPhysBus {
    guest_base: u32,
    guest_size: u64,
}

impl WasmPhysBus {
    #[inline]
    fn ptr(&self, paddr: u64, len: usize) -> Option<*const u8> {
        let len_u64 = len as u64;
        let end = paddr.checked_add(len_u64)?;
        if end > self.guest_size {
            return None;
        }

        let linear = (self.guest_base as u64)
            .checked_add(paddr)?;
        Some(linear as *const u8)
    }

    #[inline]
    fn ptr_mut(&self, paddr: u64, len: usize) -> Option<*mut u8> {
        Some(self.ptr(paddr, len)? as *mut u8)
    }

    #[inline]
    fn read_scalar<const N: usize>(&self, paddr: u64) -> [u8; N] {
        let Some(ptr) = self.ptr(paddr, N) else {
            return [0xFFu8; N];
        };
        // Safety: `ptr()` bounds-checks against the configured guest region.
        unsafe {
            let mut out = [0u8; N];
            core::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), N);
            out
        }
    }

    #[inline]
    fn write_scalar<const N: usize>(&self, paddr: u64, bytes: [u8; N]) {
        let Some(ptr) = self.ptr_mut(paddr, N) else {
            return;
        };
        // Safety: `ptr_mut()` bounds-checks against the configured guest region.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, N);
        }
    }
}

impl MemoryBus for WasmPhysBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.read_scalar::<1>(paddr)[0]
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        u16::from_le_bytes(self.read_scalar::<2>(paddr))
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        u32::from_le_bytes(self.read_scalar::<4>(paddr))
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        u64::from_le_bytes(self.read_scalar::<8>(paddr))
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.write_scalar::<1>(paddr, [value]);
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        self.write_scalar::<2>(paddr, value.to_le_bytes());
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        self.write_scalar::<4>(paddr, value.to_le_bytes());
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        self.write_scalar::<8>(paddr, value.to_le_bytes());
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct JsIoBus;

impl aero_cpu_core::paging_bus::IoBus for JsIoBus {
    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        match size {
            1 | 2 | 4 => Ok(js_io_port_read(u32::from(port), size) as u64),
            _ => Err(Exception::Unimplemented("io_read size")),
        }
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        match size {
            1 | 2 | 4 => {
                js_io_port_write(u32::from(port), size, val as u32);
                Ok(())
            }
            _ => Err(Exception::Unimplemented("io_write size")),
        }
    }
}

fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

/// Minimal VM wrapper around the Tier-0 interpreter.
///
/// This is intended to be driven by `web/src/workers/cpu.worker.ts`.
#[wasm_bindgen]
pub struct WasmVm {
    cpu: CpuCore,
    assist: AssistContext,
    bus: PagingBus<WasmPhysBus, JsIoBus>,
}

#[wasm_bindgen]
impl WasmVm {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(JsValue::from_str("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();

        // For parity with other wasm-bindgen APIs, accept `guest_size == 0` as a
        // "use the remainder of linear memory" sentinel.
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| JsValue::from_str("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(JsValue::from_str(&format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        let cpu = CpuCore::new(CpuMode::Real);
        let assist = AssistContext::default();
        let mut bus = PagingBus::new_with_io(
            WasmPhysBus {
                guest_base,
                guest_size: guest_size_u64,
            },
            JsIoBus,
        );
        bus.sync(&cpu.state);

        Ok(Self {
            cpu,
            assist,
            bus,
        })
    }

    /// Reset CPU state to 16-bit real mode and set `CS:IP = 0x0000:entry_ip`.
    pub fn reset_real_mode(&mut self, entry_ip: u32) {
        self.cpu = CpuCore::new(CpuMode::Real);
        self.cpu.state.halted = false;
        self.cpu.state.clear_pending_bios_int();
        self.assist = AssistContext::default();
        *self.bus.mmu_mut() = Mmu::new();

        // Real-mode: base = selector<<4, 64KiB limit.
        set_real_mode_seg(&mut self.cpu.state.segments.cs, 0);
        set_real_mode_seg(&mut self.cpu.state.segments.ds, 0);
        set_real_mode_seg(&mut self.cpu.state.segments.es, 0);
        set_real_mode_seg(&mut self.cpu.state.segments.ss, 0);
        set_real_mode_seg(&mut self.cpu.state.segments.fs, 0);
        set_real_mode_seg(&mut self.cpu.state.segments.gs, 0);

        self.cpu.state.set_rip(entry_ip as u64);
        self.bus.sync(&self.cpu.state);
    }

    /// Execute up to `max_insts` instructions.
    ///
    /// This is a cooperative slice intended to be called repeatedly by the CPU worker.
    pub fn run_slice(&mut self, max_insts: u32) -> RunExit {
        let max_insts_u64 = u64::from(max_insts);
        if max_insts_u64 == 0 {
            return RunExit {
                kind: RunExitKind::Completed,
                executed: 0,
                detail: String::new(),
            };
        }

        let cfg = Tier0Config::from_cpuid(&self.assist.features);

        let mut executed = 0u64;
        while executed < max_insts_u64 {
            let remaining = max_insts_u64 - executed;
            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut self.bus,
                remaining,
            );
            executed = executed.saturating_add(batch.executed);

            match batch.exit {
                BatchExit::Completed | BatchExit::Branch => {
                    // Branch exits are an internal "basic block boundary" signal for the
                    // tiered execution engine. For the minimal browser VM loop we just
                    // keep going until we consume the instruction budget.
                    continue;
                }
                BatchExit::Halted => {
                    return RunExit {
                        kind: RunExitKind::Halted,
                        executed: executed.min(u64::from(u32::MAX)) as u32,
                        detail: String::new(),
                    };
                }
                BatchExit::BiosInterrupt(vector) => {
                    return RunExit {
                        kind: RunExitKind::Assist,
                        executed: executed.min(u64::from(u32::MAX)) as u32,
                        detail: format!("bios_int(0x{vector:02x})"),
                    };
                }
                BatchExit::Assist(reason) => {
                    return RunExit {
                        kind: RunExitKind::Assist,
                        executed: executed.min(u64::from(u32::MAX)) as u32,
                        detail: format!("{reason:?}"),
                    };
                }
                BatchExit::Exception(exception) => {
                    return RunExit {
                        kind: RunExitKind::Exception,
                        executed: executed.min(u64::from(u32::MAX)) as u32,
                        detail: exception.to_string(),
                    };
                }
                BatchExit::CpuExit(exit) => {
                    let kind = match exit {
                        aero_cpu_core::interrupts::CpuExit::TripleFault => {
                            RunExitKind::ResetRequested
                        }
                        _ => RunExitKind::Exception,
                    };
                    return RunExit {
                        kind,
                        executed: executed.min(u64::from(u32::MAX)) as u32,
                        detail: format!("{exit:?}"),
                    };
                }
            }
        }

        RunExit {
            kind: RunExitKind::Completed,
            executed: executed.min(u64::from(u32::MAX)) as u32,
            detail: String::new(),
        }
    }

    /// Serialize the current CPU/MMU execution state (v2 encoding).
    ///
    /// Returns a JS object `{ cpu: Uint8Array, mmu: Uint8Array }` that can be persisted by the
    /// CPU worker and later restored via [`WasmVm::load_state_v2`].
    pub fn save_state_v2(&self) -> Result<JsValue, JsValue> {
        let cpu_state = aero_snapshot::cpu_state_from_cpu_core(&self.cpu.state);
        let mmu_state = aero_snapshot::mmu_state_from_cpu_core(&self.cpu.state);

        let mut cpu = Vec::new();
        cpu_state
            .encode_v2(&mut cpu)
            .map_err(|e| js_error(&format!("Failed to encode CPU state: {e}")))?;

        let mut mmu = Vec::new();
        mmu_state
            .encode_v2(&mut mmu)
            .map_err(|e| js_error(&format!("Failed to encode MMU state: {e}")))?;

        let cpu_js = Uint8Array::from(cpu.as_slice());
        let mmu_js = Uint8Array::from(mmu.as_slice());

        let obj = Object::new();
        let cpu_key = JsValue::from_str("cpu");
        let mmu_key = JsValue::from_str("mmu");

        Reflect::set(&obj, &cpu_key, cpu_js.as_ref())
            .map_err(|_| js_error("Failed to build snapshot object (cpu)"))?;
        Reflect::set(&obj, &mmu_key, mmu_js.as_ref())
            .map_err(|_| js_error("Failed to build snapshot object (mmu)"))?;

        Ok(obj.into())
    }

    /// Restore CPU/MMU execution state produced by [`WasmVm::save_state_v2`].
    ///
    /// This only restores architecturally visible CPU state. Any non-snapshotted runtime
    /// bookkeeping (pending interrupts, assist logs, etc) is cleared to safe defaults so the
    /// next [`WasmVm::run_slice`] is deterministic.
    pub fn load_state_v2(&mut self, cpu: &[u8], mmu: &[u8]) -> Result<(), JsValue> {
        if cpu.len() > MAX_STATE_BLOB_LEN {
            return Err(js_error(&format!(
                "CPU state blob too large: {} bytes (max {MAX_STATE_BLOB_LEN})",
                cpu.len()
            )));
        }
        if mmu.len() > MAX_STATE_BLOB_LEN {
            return Err(js_error(&format!(
                "MMU state blob too large: {} bytes (max {MAX_STATE_BLOB_LEN})",
                mmu.len()
            )));
        }

        let cpu_state = aero_snapshot::CpuState::decode_v2(&mut std::io::Cursor::new(cpu))
            .map_err(|e| js_error(&format!("Failed to decode CPU state: {e}")))?;
        let mmu_state = aero_snapshot::MmuState::decode_v2(&mut std::io::Cursor::new(mmu))
            .map_err(|e| js_error(&format!("Failed to decode MMU state: {e}")))?;

        aero_snapshot::apply_cpu_state_to_cpu_core(&cpu_state, &mut self.cpu.state);
        aero_snapshot::apply_mmu_state_to_cpu_core(&mmu_state, &mut self.cpu.state);

        // The paging bus caches translations in its internal TLB. Snapshots only capture the
        // architectural MMU registers, so discard any cached translations before resuming.
        *self.bus.mmu_mut() = Mmu::new();
        self.bus.sync(&self.cpu.state);

        // Reset runtime bookkeeping that is intentionally not part of the snapshot encoding.
        self.cpu.pending = Default::default();
        self.assist.invlpg_log.clear();

        // Restore deterministic TSC/time bookkeeping.
        let tsc_hz = self.cpu.time.tsc_hz();
        self.cpu.time = aero_cpu_core::time::TimeSource::new_deterministic(tsc_hz);
        self.cpu.time.set_tsc(mmu_state.tsc);
        // Keep the ABI-visible MSR state coherent with the time source (some instructions read
        // `state.msr.tsc` directly after advancing).
        self.cpu.state.msr.tsc = mmu_state.tsc;

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Tiered VM loop (Tier-0 interpreter + Tier-1 JS-managed JIT blocks)
// -----------------------------------------------------------------------------

/// Sentinel return value used by Tier-1 blocks to request a one-shot exit back to the interpreter.
///
/// See `crates/aero-jit-x86/src/wasm/abi.rs` (`JIT_EXIT_SENTINEL_I64`).
const JIT_EXIT_SENTINEL_I64: i64 = -1;

/// JIT context layout (header + direct-mapped TLB) shared with JS.
///
/// Keep in sync with `crates/aero-jit-x86/src/jit_ctx.rs` + `crates/aero-jit-x86/src/lib.rs`.
const JIT_TLB_ENTRIES: usize = 256;
const JIT_CTX_U64_WORDS: usize = 2 + (JIT_TLB_ENTRIES * 2); // header (ram_base, tlb_salt) + (tag,data)*entries

#[derive(Clone, Default)]
struct SharedCompileQueue(Rc<RefCell<Vec<u64>>>);

impl SharedCompileQueue {
    fn drain(&self) -> Vec<u64> {
        let mut borrow = self.0.borrow_mut();
        let out = borrow.clone();
        borrow.clear();
        out
    }
}

impl CompileRequestSink for SharedCompileQueue {
    fn request_compile(&mut self, entry_rip: u64) {
        self.0.borrow_mut().push(entry_rip);
    }
}

#[derive(Clone, Copy, Debug)]
struct JsTier1Backend {
    jit_ctx_ptr: u32,
}

impl JitBackend for JsTier1Backend {
    type Cpu = Vcpu<WasmBus>;

    fn execute(&mut self, table_index: u32, cpu: &mut Self::Cpu) -> JitBlockExit {
        let cpu_ptr = (&mut cpu.cpu.state as *mut aero_cpu_core::state::CpuState) as u32;
        let ret = js_jit_call(table_index, cpu_ptr, self.jit_ctx_ptr);

        let exit_to_interpreter = ret == JIT_EXIT_SENTINEL_I64;
        let next_rip = if exit_to_interpreter {
            cpu.rip()
        } else {
            ret as u64
        };

        JitBlockExit {
            next_rip,
            exit_to_interpreter,
        }
    }
}

#[wasm_bindgen]
pub struct WasmTieredVm {
    guest_base: u32,
    guest_size: u64,

    vcpu: Vcpu<WasmBus>,
    interp: Tier0Interpreter,
    jit: JitRuntime<JsTier1Backend, SharedCompileQueue>,
    jit_config: JitConfig,
    compile_queue: SharedCompileQueue,
    force_interpreter: bool,

    jit_ctx: Vec<u64>,

    interp_executions: u64,
    jit_executions: u64,
}

#[wasm_bindgen]
impl WasmTieredVm {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(JsValue::from_str("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();

        // For parity with other wasm-bindgen APIs, accept `guest_size == 0` as a
        // "use the remainder of linear memory" sentinel.
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| JsValue::from_str("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(JsValue::from_str(&format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        let bus = WasmBus {
            guest_base,
            guest_size: guest_size_u64,
        };
        let vcpu = Vcpu::new_with_mode(CpuMode::Real, bus);

        let jit_config = JitConfig {
            enabled: true,
            hot_threshold: 32,
            // Keep the tiered VM cache bounded for the browser smoke harness.
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
        };

        let compile_queue = SharedCompileQueue::default();

        let mut jit_ctx = vec![0u64; JIT_CTX_U64_WORDS];
        // JitContext header: { ram_base, tlb_salt }.
        jit_ctx[0] = guest_base as u64;
        jit_ctx[1] = 0x1234_5678_9abc_def0u64;
        let jit_ctx_ptr = jit_ctx.as_ptr() as u32;

        let backend = JsTier1Backend { jit_ctx_ptr };
        let jit = JitRuntime::new(jit_config.clone(), backend, compile_queue.clone());

        Ok(Self {
            guest_base,
            guest_size: guest_size_u64,

            vcpu,
            interp: Tier0Interpreter::new(1024),
            jit,
            jit_config,
            compile_queue,
            force_interpreter: false,

            jit_ctx,

            interp_executions: 0,
            jit_executions: 0,
        })
    }

    /// Reset CPU state to 16-bit real mode and set `CS:IP = 0x0000:entry_ip`.
    pub fn reset_real_mode(&mut self, entry_ip: u32) {
        self.vcpu.exit = None;
        self.vcpu.cpu = CpuCore::new(CpuMode::Real);
        self.vcpu.cpu.state.halted = false;
        self.vcpu.cpu.state.clear_pending_bios_int();
        self.vcpu.cpu.pending = Default::default();

        self.interp.assist = AssistContext::default();

        // Reset JIT bookkeeping (cache + hotness + pending requests).
        self.compile_queue.drain();
        let jit_ctx_ptr = self.jit_ctx.as_ptr() as u32;
        self.jit = JitRuntime::new(
            self.jit_config.clone(),
            JsTier1Backend { jit_ctx_ptr },
            self.compile_queue.clone(),
        );
        self.force_interpreter = false;

        self.interp_executions = 0;
        self.jit_executions = 0;

        // Real-mode: base = selector<<4, 64KiB limit.
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.cs, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.ds, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.es, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.ss, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.fs, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.gs, 0);

        self.vcpu.cpu.state.set_rip(entry_ip as u64);
    }

    /// Execute up to `blocks` basic blocks, choosing Tier-0 vs Tier-1 per the hotness/JIT cache.
    pub fn run_blocks(&mut self, blocks: u32) {
        let mut remaining = u64::from(blocks);
        while remaining > 0 {
            // Interrupts are delivered at instruction boundaries.
            if self.vcpu.maybe_deliver_interrupt() {
                continue;
            }

            if self.vcpu.exit.is_some() || self.vcpu.cpu.state.halted {
                break;
            }

            let entry_rip = self.vcpu.rip();
            let compiled = self.jit.prepare_block(entry_rip);

            if self.force_interpreter || compiled.is_none() {
                let next_rip = self.interp.exec_block(&mut self.vcpu);
                self.vcpu.set_rip(next_rip);
                self.force_interpreter = false;
                self.interp_executions = self.interp_executions.saturating_add(1);
            } else {
                let handle = compiled.expect("checked is_some above");
                let exit = self.jit.execute_block(&mut self.vcpu, &handle);
                self.vcpu.set_rip(exit.next_rip);
                self.force_interpreter = exit.exit_to_interpreter;
                self.jit_executions = self.jit_executions.saturating_add(1);
            }

            remaining -= 1;
        }
    }

    /// Drain queued Tier-1 compilation requests accumulated while running blocks.
    pub fn drain_compile_requests(&mut self) -> Uint32Array {
        let drained = self.compile_queue.drain();
        let out: Vec<u32> = drained
            .into_iter()
            .filter_map(|rip| u32::try_from(rip).ok())
            .collect();
        Uint32Array::from(out.as_slice())
    }

    /// Install a compiled Tier-1 block into the runtime JIT cache.
    pub fn install_tier1_block(&mut self, entry_rip: u32, table_index: u32, code_paddr: u32, byte_len: u32) {
        let _evicted = self
            .jit
            .install_block(entry_rip as u64, table_index, code_paddr as u64, byte_len);
    }

    #[wasm_bindgen(getter)]
    pub fn interp_executions(&self) -> u32 {
        self.interp_executions.min(u64::from(u32::MAX)) as u32
    }

    #[wasm_bindgen(getter)]
    pub fn jit_executions(&self) -> u32 {
        self.jit_executions.min(u64::from(u32::MAX)) as u32
    }

    #[wasm_bindgen(getter)]
    pub fn guest_base(&self) -> u32 {
        self.guest_base
    }

    #[wasm_bindgen(getter)]
    pub fn guest_size(&self) -> u32 {
        self.guest_size.min(u64::from(u32::MAX)) as u32
    }
}
