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

use aero_cpu_core::{
    assist::AssistContext,
    exception::Exception,
    interp::tier0::exec::{run_batch_with_assists, BatchExit},
    mem::CpuBus,
    state::{CpuMode, CpuState, Segment},
};

use crate::{RunExit, RunExitKind};

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
struct WasmBus {
    guest_base: u32,
    guest_size: u64,
}

impl WasmBus {
    #[inline]
    fn ptr(&self, vaddr: u64, len: usize) -> Result<*const u8, Exception> {
        let len_u64 = len as u64;
        let end = vaddr.checked_add(len_u64).ok_or(Exception::MemoryFault)?;
        if end > self.guest_size {
            return Err(Exception::MemoryFault);
        }

        let linear = (self.guest_base as u64)
            .checked_add(vaddr)
            .ok_or(Exception::MemoryFault)?;
        Ok(linear as *const u8)
    }

    #[inline]
    fn ptr_mut(&self, vaddr: u64, len: usize) -> Result<*mut u8, Exception> {
        Ok(self.ptr(vaddr, len)? as *mut u8)
    }

    #[inline]
    fn read_scalar<const N: usize>(&self, vaddr: u64) -> Result<[u8; N], Exception> {
        let ptr = self.ptr(vaddr, N)?;
        // Safety: `ptr()` bounds-checks against the configured guest region.
        unsafe {
            let mut out = [0u8; N];
            core::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), N);
            Ok(out)
        }
    }

    #[inline]
    fn write_scalar<const N: usize>(&self, vaddr: u64, bytes: [u8; N]) -> Result<(), Exception> {
        let ptr = self.ptr_mut(vaddr, N)?;
        // Safety: `ptr_mut()` bounds-checks against the configured guest region.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, N);
        }
        Ok(())
    }
}

impl CpuBus for WasmBus {
    #[inline]
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        Ok(self.read_scalar::<1>(vaddr)?[0])
    }

    #[inline]
    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        Ok(u16::from_le_bytes(self.read_scalar::<2>(vaddr)?))
    }

    #[inline]
    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        Ok(u32::from_le_bytes(self.read_scalar::<4>(vaddr)?))
    }

    #[inline]
    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        Ok(u64::from_le_bytes(self.read_scalar::<8>(vaddr)?))
    }

    #[inline]
    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        Ok(u128::from_le_bytes(self.read_scalar::<16>(vaddr)?))
    }

    #[inline]
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.write_scalar::<1>(vaddr, [val])
    }

    #[inline]
    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_scalar::<2>(vaddr, val.to_le_bytes())
    }

    #[inline]
    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_scalar::<4>(vaddr, val.to_le_bytes())
    }

    #[inline]
    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_scalar::<8>(vaddr, val.to_le_bytes())
    }

    #[inline]
    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_scalar::<16>(vaddr, val.to_le_bytes())
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        let ptr = self.ptr(vaddr, dst.len())?;
        // Safety: `ptr()` bounds-checks.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr, dst.as_mut_ptr(), dst.len());
        }
        Ok(())
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        let ptr = self.ptr_mut(vaddr, src.len())?;
        // Safety: `ptr_mut()` bounds-checks.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), ptr, src.len());
        }
        Ok(())
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }
        let dst_ptr = self.ptr_mut(dst, len)?;
        let src_ptr = self.ptr(src, len)?;
        // Safety: pointers are in-bounds and `copy` preserves overlap semantics.
        unsafe {
            core::ptr::copy(src_ptr, dst_ptr, len);
        }
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }
        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;
        let dst_ptr = self.ptr_mut(dst, total)?;
        // Safety: `dst_ptr` covers `total` bytes.
        unsafe {
            for i in 0..repeat {
                let off = i * pattern.len();
                core::ptr::copy_nonoverlapping(pattern.as_ptr(), dst_ptr.add(off), pattern.len());
            }
        }
        Ok(true)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        self.read_bytes(vaddr, &mut buf[..len])?;
        Ok(buf)
    }

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
    guest_base: u32,
    guest_size: u64,
    cpu: CpuState,
    assist: AssistContext,
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

        Ok(Self {
            guest_base,
            guest_size: guest_size_u64,
            cpu: CpuState::new(CpuMode::Real),
            assist: AssistContext::default(),
        })
    }

    /// Reset CPU state to 16-bit real mode and set `CS:IP = 0x0000:entry_ip`.
    pub fn reset_real_mode(&mut self, entry_ip: u32) {
        self.cpu = CpuState::new(CpuMode::Real);
        self.cpu.halted = false;
        self.cpu.clear_pending_bios_int();
        self.assist = AssistContext::default();

        // Real-mode: base = selector<<4, 64KiB limit.
        set_real_mode_seg(&mut self.cpu.segments.cs, 0);
        set_real_mode_seg(&mut self.cpu.segments.ds, 0);
        set_real_mode_seg(&mut self.cpu.segments.es, 0);
        set_real_mode_seg(&mut self.cpu.segments.ss, 0);
        set_real_mode_seg(&mut self.cpu.segments.fs, 0);
        set_real_mode_seg(&mut self.cpu.segments.gs, 0);

        self.cpu.set_rip(entry_ip as u64);
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

        let mut bus = WasmBus {
            guest_base: self.guest_base,
            guest_size: self.guest_size,
        };

        let mut executed = 0u64;
        while executed < max_insts_u64 {
            let remaining = max_insts_u64 - executed;
            let batch = run_batch_with_assists(&mut self.assist, &mut self.cpu, &mut bus, remaining);
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
            }
        }

        RunExit {
            kind: RunExitKind::Completed,
            executed: executed.min(u64::from(u32::MAX)) as u32,
            detail: String::new(),
        }
    }
}

