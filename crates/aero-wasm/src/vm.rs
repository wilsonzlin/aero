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

use js_sys::{Object, Reflect, Uint8Array};

use aero_cpu_core::{
    CpuBus, CpuCore, Exception, PagingBus,
    assist::AssistContext,
    interp::tier0::{
        Tier0Config,
        exec::{BatchExit, run_batch_cpu_core_with_assists},
    },
    state::{CpuMode, Segment},
};
use aero_mmu::{MemoryBus, Mmu};

use crate::{RunExit, RunExitKind};

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
// Tiered (Tier-0 + Tier-1) execution lives in `crate::tiered_vm`.
