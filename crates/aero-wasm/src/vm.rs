//! Legacy CPU-only x86 VM loop used by the browser CPU worker runtime.
//!
//! `WasmVm` is intentionally small: it wires the Tier-0 interpreter (`aero_cpu_core`) to the
//! injected shared guest RAM inside the module's linear memory.
//!
//! This is **not** a full-system machine: port I/O and MMIO are forwarded back to JS via shims
//! (`globalThis.__aero_io_port_*`, `globalThis.__aero_mmio_*`). The CPU worker installs these shims
//! and typically forwards them to the I/O worker via AIPC rings.
//!
//! For new full-system work (PCI/devices/networking), prefer the canonical `Machine` WASM export
//! (`crates/aero-wasm::Machine`, backed by `crates/aero-machine::Machine`).

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

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_mmio_read)]
    fn js_mmio_read(addr: u64, size: u32) -> u32;

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_mmio_write)]
    fn js_mmio_write(addr: u64, size: u32, value: u32);
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
    fn ram_offset(&self, paddr: u64, len: usize) -> Option<u64> {
        match crate::guest_phys::translate_guest_paddr_range(self.guest_size, paddr, len) {
            crate::guest_phys::GuestRamRange::Ram { ram_offset } => Some(ram_offset),
            crate::guest_phys::GuestRamRange::Hole
            | crate::guest_phys::GuestRamRange::OutOfBounds => None,
        }
    }

    #[inline]
    fn ptr(&self, paddr: u64, len: usize) -> Option<*const u8> {
        let ram_offset = self.ram_offset(paddr, len)?;
        let linear = (self.guest_base as u64).checked_add(ram_offset)?;
        let linear_u32 = u32::try_from(linear).ok()?;
        Some(linear_u32 as *const u8)
    }

    #[inline]
    fn ptr_mut(&self, paddr: u64, len: usize) -> Option<*mut u8> {
        Some(self.ptr(paddr, len)? as *mut u8)
    }

    #[inline]
    fn any_ram_byte(&self, paddr: u64, len: usize) -> bool {
        for i in 0..len {
            let Some(addr) = paddr.checked_add(i as u64) else {
                return false;
            };
            if self.ram_offset(addr, 1).is_some() {
                return true;
            }
        }
        false
    }

    #[inline]
    fn read_scalar<const N: usize>(&self, paddr: u64) -> Option<[u8; N]> {
        let ptr = self.ptr(paddr, N)?;
        // Safety: `ptr()` bounds-checks against the configured guest region.
        unsafe {
            let mut out = [0u8; N];
            core::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), N);
            Some(out)
        }
    }

    #[inline]
    fn write_scalar<const N: usize>(&self, paddr: u64, bytes: [u8; N]) -> bool {
        let Some(ptr) = self.ptr_mut(paddr, N) else {
            return false;
        };
        // Safety: `ptr_mut()` bounds-checks against the configured guest region.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, N);
        }
        true
    }
}

impl MemoryBus for WasmPhysBus {
    #[inline]
    fn read_u8(&mut self, paddr: u64) -> u8 {
        match self.read_scalar::<1>(paddr) {
            Some(bytes) => bytes[0],
            None => js_mmio_read(paddr, 1) as u8,
        }
    }

    #[inline]
    fn read_u16(&mut self, paddr: u64) -> u16 {
        match self.read_scalar::<2>(paddr) {
            Some(bytes) => u16::from_le_bytes(bytes),
            None => {
                if !self.any_ram_byte(paddr, 2) {
                    return js_mmio_read(paddr, 2) as u16;
                }
                let mut bytes = [0u8; 2];
                for (i, slot) in bytes.iter_mut().enumerate() {
                    let Some(addr) = paddr.checked_add(i as u64) else {
                        *slot = 0xFF;
                        continue;
                    };
                    *slot = self.read_u8(addr);
                }
                u16::from_le_bytes(bytes)
            }
        }
    }

    #[inline]
    fn read_u32(&mut self, paddr: u64) -> u32 {
        match self.read_scalar::<4>(paddr) {
            Some(bytes) => u32::from_le_bytes(bytes),
            None => {
                if !self.any_ram_byte(paddr, 4) {
                    return js_mmio_read(paddr, 4);
                }
                let mut bytes = [0u8; 4];
                for (i, slot) in bytes.iter_mut().enumerate() {
                    let Some(addr) = paddr.checked_add(i as u64) else {
                        *slot = 0xFF;
                        continue;
                    };
                    *slot = self.read_u8(addr);
                }
                u32::from_le_bytes(bytes)
            }
        }
    }

    #[inline]
    fn read_u64(&mut self, paddr: u64) -> u64 {
        match self.read_scalar::<8>(paddr) {
            Some(bytes) => u64::from_le_bytes(bytes),
            None => {
                if self.any_ram_byte(paddr, 8) {
                    let mut bytes = [0u8; 8];
                    for (i, slot) in bytes.iter_mut().enumerate() {
                        let Some(addr) = paddr.checked_add(i as u64) else {
                            *slot = 0xFF;
                            continue;
                        };
                        *slot = self.read_u8(addr);
                    }
                    return u64::from_le_bytes(bytes);
                }
                let lo = js_mmio_read(paddr, 4) as u64;
                let hi_addr = match paddr.checked_add(4) {
                    Some(v) => v,
                    None => return 0xFFFF_FFFF_FFFF_FFFF,
                };
                let hi = js_mmio_read(hi_addr, 4) as u64;
                lo | (hi << 32)
            }
        }
    }

    #[inline]
    fn write_u8(&mut self, paddr: u64, value: u8) {
        if self.write_scalar::<1>(paddr, [value]) {
            return;
        }
        js_mmio_write(paddr, 1, u32::from(value));
    }

    #[inline]
    fn write_u16(&mut self, paddr: u64, value: u16) {
        if self.write_scalar::<2>(paddr, value.to_le_bytes()) {
            return;
        }
        if !self.any_ram_byte(paddr, 2) {
            js_mmio_write(paddr, 2, u32::from(value));
            return;
        }
        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.into_iter().enumerate() {
            let Some(addr) = paddr.checked_add(i as u64) else {
                continue;
            };
            self.write_u8(addr, byte);
        }
    }

    #[inline]
    fn write_u32(&mut self, paddr: u64, value: u32) {
        if self.write_scalar::<4>(paddr, value.to_le_bytes()) {
            return;
        }
        if !self.any_ram_byte(paddr, 4) {
            js_mmio_write(paddr, 4, value);
            return;
        }
        let bytes = value.to_le_bytes();
        for (i, byte) in bytes.into_iter().enumerate() {
            let Some(addr) = paddr.checked_add(i as u64) else {
                continue;
            };
            self.write_u8(addr, byte);
        }
    }

    #[inline]
    fn write_u64(&mut self, paddr: u64, value: u64) {
        if self.write_scalar::<8>(paddr, value.to_le_bytes()) {
            return;
        }
        if self.any_ram_byte(paddr, 8) {
            let bytes = value.to_le_bytes();
            for (i, byte) in bytes.into_iter().enumerate() {
                let Some(addr) = paddr.checked_add(i as u64) else {
                    continue;
                };
                self.write_u8(addr, byte);
            }
            return;
        }
        let lo = value as u32;
        let hi = (value >> 32) as u32;
        js_mmio_write(paddr, 4, lo);
        if let Some(hi_addr) = paddr.checked_add(4) {
            js_mmio_write(hi_addr, 4, hi);
        }
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
        // Keep the *backing* guest RAM below the PCI MMIO BAR allocation window so the JS-side PCI
        // bus never overlaps the shared wasm linear memory guest region.
        //
        // Note: the guest physical layout may still remap "high RAM" above 4GiB when the backing
        // RAM size exceeds the PCIe ECAM base (PC/Q35 E820 layout).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::GUEST_PCI_MMIO_BASE);

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

        Ok(Self { cpu, assist, bus })
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

    /// Return the linear-memory pointer to the `CpuState.a20_enabled` flag.
    ///
    /// The browser runtime updates the A20 gate state in response to I/O worker events. Those
    /// events can arrive *while the VM is executing* (e.g. during an `IN`/`OUT` instruction that
    /// calls a JS import to perform port I/O, which synchronously drains pending device events).
    ///
    /// Calling back into WASM from that import to mutate `WasmVm` would be *re-entrant* and can
    /// violate Rust's aliasing rules (nested `&mut self` borrows). Instead, the host can write
    /// `0`/`1` into the returned address directly in the module's linear memory.
    pub fn a20_enabled_ptr(&self) -> u32 {
        // Safety: the pointer is into this module's linear memory (wasm32); JS can treat it as an
        // absolute byte offset into `WebAssembly.Memory.buffer`.
        core::ptr::addr_of!(self.cpu.state.a20_enabled) as u32
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
    /// Returns a JS object `{ cpu: Uint8Array, mmu: Uint8Array, cpu_internal: Uint8Array }` that
    /// can be persisted by the CPU worker and later restored via [`WasmVm::load_state_v2`] +
    /// [`WasmVm::load_cpu_internal_state_v2`].
    pub fn save_state_v2(&self) -> Result<JsValue, JsValue> {
        let cpu_state = aero_snapshot::cpu_state_from_cpu_core(&self.cpu.state);
        let mmu_state = aero_snapshot::mmu_state_from_cpu_core(&self.cpu.state);
        let cpu_internal_state = aero_snapshot::cpu_internal_state_from_cpu_core(&self.cpu);

        let mut cpu = Vec::new();
        cpu_state
            .encode_v2(&mut cpu)
            .map_err(|e| js_error(&format!("Failed to encode CPU state: {e}")))?;

        let mut mmu = Vec::new();
        mmu_state
            .encode_v2(&mut mmu)
            .map_err(|e| js_error(&format!("Failed to encode MMU state: {e}")))?;

        let cpu_internal = cpu_internal_state
            .to_device_state()
            .map_err(|e| js_error(&format!("Failed to encode CPU internal state: {e}")))?
            .data;

        let cpu_js = Uint8Array::from(cpu.as_slice());
        let mmu_js = Uint8Array::from(mmu.as_slice());
        let cpu_internal_js = Uint8Array::from(cpu_internal.as_slice());

        let obj = Object::new();
        let cpu_key = JsValue::from_str("cpu");
        let mmu_key = JsValue::from_str("mmu");
        let cpu_internal_key = JsValue::from_str("cpu_internal");

        Reflect::set(&obj, &cpu_key, cpu_js.as_ref())
            .map_err(|_| js_error("Failed to build snapshot object (cpu)"))?;
        Reflect::set(&obj, &mmu_key, mmu_js.as_ref())
            .map_err(|_| js_error("Failed to build snapshot object (mmu)"))?;
        Reflect::set(&obj, &cpu_internal_key, cpu_internal_js.as_ref())
            .map_err(|_| js_error("Failed to build snapshot object (cpu_internal)"))?;

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

    /// Restore non-architectural CPU bookkeeping produced by [`WasmVm::save_state_v2`].
    ///
    /// This restores the `DeviceId::CPU_INTERNAL` v2 encoding (interrupt shadow + pending external
    /// interrupts). It is safe to call immediately after [`WasmVm::load_state_v2`], which clears
    /// all pending-event state to deterministic defaults.
    pub fn load_cpu_internal_state_v2(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.len() > MAX_STATE_BLOB_LEN {
            return Err(js_error(&format!(
                "CPU_INTERNAL state blob too large: {} bytes (max {MAX_STATE_BLOB_LEN})",
                bytes.len()
            )));
        }

        let state = aero_snapshot::CpuInternalState::decode(&mut std::io::Cursor::new(bytes))
            .map_err(|e| js_error(&format!("Failed to decode CPU internal state: {e}")))?;
        aero_snapshot::apply_cpu_internal_state_to_cpu_core(&state, &mut self.cpu);
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// wasm-bindgen tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{WasmPhysBus, WasmVm};

    use aero_mmu::MemoryBus;
    use js_sys::{Array, Reflect};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen(inline_js = r#"
export function installAeroMmioTestShims() {
  // Simple call log consumed by the Rust-side assertions.
  globalThis.__aero_test_mmio_calls = [];

  globalThis.__aero_mmio_read = function (addr, size) {
    // Store addr as a Number for easier Rust access. Tests only use small addresses
    // (<2^53) so the conversion is exact.
    globalThis.__aero_test_mmio_calls.push({
      kind: "read",
      addr: Number(addr),
      size: size >>> 0,
    });
    // Return the low 32 bits of the address so split reads can be asserted.
    return Number(addr) >>> 0;
  };

  globalThis.__aero_mmio_write = function (addr, size, value) {
    globalThis.__aero_test_mmio_calls.push({
      kind: "write",
      addr: Number(addr),
      size: size >>> 0,
      value: value >>> 0,
    });
  };

  // Install no-op port shims so any incidental WasmVm instantiation from other tests
  // doesn't trap due to missing globals.
  if (typeof globalThis.__aero_io_port_read !== "function") {
    globalThis.__aero_io_port_read = function (_port, _size) { return 0; };
  }
  if (typeof globalThis.__aero_io_port_write !== "function") {
    globalThis.__aero_io_port_write = function (_port, _size, _value) { };
  }
}
"#)]
    extern "C" {
        fn installAeroMmioTestShims();
    }

    fn mmio_calls() -> Array {
        let global = js_sys::global();
        let calls = Reflect::get(&global, &JsValue::from_str("__aero_test_mmio_calls"))
            .expect("get __aero_test_mmio_calls");
        calls
            .dyn_into::<Array>()
            .expect("__aero_test_mmio_calls must be an Array")
    }

    fn call_prop_u32(call: &JsValue, key: &str) -> u32 {
        Reflect::get(call, &JsValue::from_str(key))
            .expect("prop exists")
            .as_f64()
            .expect("prop is number")
            .round() as u32
    }

    fn call_prop_str(call: &JsValue, key: &str) -> String {
        Reflect::get(call, &JsValue::from_str(key))
            .expect("prop exists")
            .as_string()
            .expect("prop is string")
    }

    #[wasm_bindgen_test]
    fn wasm_phys_bus_routes_out_of_ram_accesses_to_mmio() {
        installAeroMmioTestShims();

        let mut guest = vec![0u8; 0x10];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u64;

        let mut bus = WasmPhysBus {
            guest_base,
            guest_size,
        };

        // In-RAM access uses the RAM fast path and must not invoke MMIO shims.
        bus.write_u32(0, 0x1122_3344);
        assert_eq!(bus.read_u32(0), 0x1122_3344);
        assert_eq!(
            mmio_calls().length(),
            0,
            "unexpected MMIO calls for RAM access"
        );

        // Out-of-RAM access must invoke MMIO shims.
        assert_eq!(bus.read_u32(0x20), 0x20, "mmio read returns stubbed value");
        bus.write_u16(0x30, 0x1234);

        let calls = mmio_calls();
        assert_eq!(calls.length(), 2, "expected exactly 2 MMIO calls");

        let c0 = calls.get(0);
        assert_eq!(call_prop_str(&c0, "kind"), "read");
        assert_eq!(call_prop_u32(&c0, "addr"), 0x20);
        assert_eq!(call_prop_u32(&c0, "size"), 4);

        let c1 = calls.get(1);
        assert_eq!(call_prop_str(&c1, "kind"), "write");
        assert_eq!(call_prop_u32(&c1, "addr"), 0x30);
        assert_eq!(call_prop_u32(&c1, "size"), 2);
        assert_eq!(call_prop_u32(&c1, "value"), 0x1234);
    }

    #[wasm_bindgen_test]
    fn wasm_phys_bus_splits_u64_mmio_into_u32_ops() {
        installAeroMmioTestShims();

        let mut guest = vec![0u8; 0x10];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u64;

        let mut bus = WasmPhysBus {
            guest_base,
            guest_size,
        };

        let value = bus.read_u64(0x100);
        let expected = (0x104u64 << 32) | 0x100u64;
        assert_eq!(value, expected);

        let calls = mmio_calls();
        assert_eq!(
            calls.length(),
            2,
            "read_u64 should issue two mmio_read calls"
        );

        let c0 = calls.get(0);
        assert_eq!(call_prop_str(&c0, "kind"), "read");
        assert_eq!(call_prop_u32(&c0, "addr"), 0x100);
        assert_eq!(call_prop_u32(&c0, "size"), 4);

        let c1 = calls.get(1);
        assert_eq!(call_prop_str(&c1, "kind"), "read");
        assert_eq!(call_prop_u32(&c1, "addr"), 0x104);
        assert_eq!(call_prop_u32(&c1, "size"), 4);

        // Reset call log.
        Reflect::set(
            &js_sys::global(),
            &JsValue::from_str("__aero_test_mmio_calls"),
            &Array::new(),
        )
        .expect("clear call log");

        bus.write_u64(0x200, 0x8877_6655_4433_2211);

        let calls = mmio_calls();
        assert_eq!(
            calls.length(),
            2,
            "write_u64 should issue two mmio_write calls"
        );

        let c0 = calls.get(0);
        assert_eq!(call_prop_str(&c0, "kind"), "write");
        assert_eq!(call_prop_u32(&c0, "addr"), 0x200);
        assert_eq!(call_prop_u32(&c0, "size"), 4);
        assert_eq!(call_prop_u32(&c0, "value"), 0x4433_2211);

        let c1 = calls.get(1);
        assert_eq!(call_prop_str(&c1, "kind"), "write");
        assert_eq!(call_prop_u32(&c1, "addr"), 0x204);
        assert_eq!(call_prop_u32(&c1, "size"), 4);
        assert_eq!(call_prop_u32(&c1, "value"), 0x8877_6655);
    }

    #[wasm_bindgen_test]
    fn wasm_phys_bus_straddling_ram_and_mmio_reads_and_writes_bytewise() {
        installAeroMmioTestShims();

        let mut guest = vec![0u8; 0x10];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u64;

        let mut bus = WasmPhysBus {
            guest_base,
            guest_size,
        };

        // Read across the end of RAM: the first byte comes from RAM and the remaining bytes are
        // sourced via MMIO shims.
        guest[0x0F] = 0xAA;
        let value = bus.read_u32(0x0F);
        assert_eq!(value, 0x12_11_10_AA);

        let calls = mmio_calls();
        assert_eq!(calls.length(), 3, "expected 3 byte MMIO reads for a straddle");

        for (idx, addr) in [(0, 0x10), (1, 0x11), (2, 0x12)] {
            let call = calls.get(idx);
            assert_eq!(call_prop_str(&call, "kind"), "read");
            assert_eq!(call_prop_u32(&call, "addr"), addr);
            assert_eq!(call_prop_u32(&call, "size"), 1);
        }

        // Reset call log.
        Reflect::set(
            &js_sys::global(),
            &JsValue::from_str("__aero_test_mmio_calls"),
            &Array::new(),
        )
        .expect("clear call log");

        // Write across the end of RAM: first byte should land in RAM, remainder should be routed
        // to MMIO byte writes.
        bus.write_u32(0x0F, 0x4433_2211);
        assert_eq!(guest[0x0F], 0x11);

        let calls = mmio_calls();
        assert_eq!(calls.length(), 3, "expected 3 byte MMIO writes for a straddle");

        for (idx, addr, byte) in [(0, 0x10, 0x22), (1, 0x11, 0x33), (2, 0x12, 0x44)] {
            let call = calls.get(idx);
            assert_eq!(call_prop_str(&call, "kind"), "write");
            assert_eq!(call_prop_u32(&call, "addr"), addr);
            assert_eq!(call_prop_u32(&call, "size"), 1);
            assert_eq!(call_prop_u32(&call, "value"), byte);
        }
    }

    #[wasm_bindgen_test]
    fn wasm_vm_a20_enabled_ptr_controls_real_mode_wraparound() {
        installAeroMmioTestShims();

        // Allocate enough guest RAM to include the 1MiB alias boundary.
        let mut guest = vec![0u8; 2 * 1024 * 1024];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        // Place distinct bytes at physical 0x0 and 0x1_00000.
        guest[0x0000_0000] = 0x11;
        guest[0x0010_0000] = 0x22;

        // Write a tiny real-mode program at 0x0100:
        //   mov al, [0x0010_0000]   (addr-size override, moffs32)
        //   mov [0x0000_0200], al   (addr-size override, moffs32)
        //   hlt
        const ENTRY_IP: u32 = 0x0100;
        let code = [
            0x67, 0xA0, 0x00, 0x00, 0x10, 0x00, // mov al, [0x0010_0000]
            0x67, 0xA2, 0x00, 0x02, 0x00, 0x00, // mov [0x0000_0200], al
            0xF4, // hlt
        ];
        guest[ENTRY_IP as usize..ENTRY_IP as usize + code.len()].copy_from_slice(&code);

        let mut vm = WasmVm::new(guest_base, guest_size).expect("WasmVm::new should succeed");

        // ---------------------------------------------------------------------
        // A20 enabled: reading 0x1_00000 should see 0x22.
        // ---------------------------------------------------------------------
        guest[0x0000_0200] = 0;
        vm.reset_real_mode(ENTRY_IP);
        let a20_ptr = vm.a20_enabled_ptr();
        assert_ne!(a20_ptr, 0, "a20_enabled_ptr must return a non-zero address");

        // Safety: `a20_ptr` is provided by `WasmVm` and points into wasm linear memory.
        // Write 0/1 only (valid `bool` representation in Rust).
        unsafe {
            (a20_ptr as *mut u8).write(1);
        }

        let exit = vm.run_slice(128);
        assert_eq!(exit.kind(), crate::RunExitKind::Halted);
        assert_eq!(
            guest[0x0000_0200], 0x22,
            "A20 enabled: 0x1_00000 should be distinct from 0x0"
        );

        // ---------------------------------------------------------------------
        // A20 disabled: reading 0x1_00000 should alias to 0x0 (0x11).
        // ---------------------------------------------------------------------
        guest[0x0000_0200] = 0;
        vm.reset_real_mode(ENTRY_IP);
        let a20_ptr2 = vm.a20_enabled_ptr();
        assert_eq!(
            a20_ptr2, a20_ptr,
            "a20_enabled_ptr must be stable across reset_real_mode"
        );
        unsafe {
            (a20_ptr2 as *mut u8).write(0);
        }

        let exit = vm.run_slice(128);
        assert_eq!(exit.kind(), crate::RunExitKind::Halted);
        assert_eq!(
            guest[0x0000_0200], 0x11,
            "A20 disabled: 0x1_00000 should alias to 0x0"
        );
    }

    #[wasm_bindgen_test]
    fn guest_ram_remap_translation_maps_high_ram_above_4gib() {
        // Low RAM below ECAM base + small high-RAM region remapped above 4GiB.
        let ram_bytes = 0xB000_0000u64 + 0x2000;

        // paddr 0 -> RAM offset 0.
        assert_eq!(
            crate::guest_phys::translate_guest_paddr_range(ram_bytes, 0x0, 1),
            crate::guest_phys::GuestRamRange::Ram { ram_offset: 0 }
        );

        // ECAM base is inside the hole.
        assert_eq!(
            crate::guest_phys::translate_guest_paddr_range(ram_bytes, 0xB000_0000, 1),
            crate::guest_phys::GuestRamRange::Hole
        );

        // High RAM begins at 4GiB and maps to offset 0xB000_0000 in the contiguous backing store.
        assert_eq!(
            crate::guest_phys::translate_guest_paddr_range(ram_bytes, 0x1_0000_0000, 1),
            crate::guest_phys::GuestRamRange::Ram {
                ram_offset: 0xB000_0000
            }
        );
    }
}
// Tiered (Tier-0 + Tier-1) execution lives in `crate::tiered_vm`.
