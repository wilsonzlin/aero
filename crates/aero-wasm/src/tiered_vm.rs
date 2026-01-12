//! Legacy Tier-0 + Tier-1 tiered execution VM loop used by the browser CPU worker runtime.
//!
//! `WasmTieredVm` is the “tiered” sibling of [`crate::WasmVm`]: it executes CPU in WASM but is still
//! **CPU-only** (not a full-system machine).
//!
//! This module wires up:
//! - Tier-0 interpreter blocks (`aero_cpu_core::exec::Tier0Interpreter`)
//! - Tier-1 JIT cache + tiering logic (`aero_cpu_core::jit::runtime::JitRuntime`)
//! - Tiered dispatcher (`aero_cpu_core::exec::ExecDispatcher`)
//!
//! Integration boundaries:
//! - Compiled Tier-1 blocks are executed by calling out to JS via
//!   `globalThis.__aero_jit_call(table_index, cpu_ptr, jit_ctx_ptr)`.
//! - Port I/O is forwarded back to JS via `globalThis.__aero_io_port_*`.
//!
//! This path exists primarily to iterate on CPU/JIT behavior in the worker runtime. For new
//! full-system work (devices/networking), prefer the canonical `Machine` WASM export
//! (`crates/aero-wasm::Machine`, backed by `crates/aero-machine::Machine`).

#![cfg(target_arch = "wasm32")]

use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use js_sys::{Array, BigInt, Object, Reflect};

use aero_cpu_core::exec::{ExecDispatcher, ExecutedTier, StepOutcome, Tier0Interpreter, Vcpu};
use aero_cpu_core::jit::cache::{CompiledBlockHandle, CompiledBlockMeta, PageVersionSnapshot};
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime, PAGE_SHIFT,
};
use aero_cpu_core::state::{
    CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF, CPU_STATE_ALIGN, CPU_STATE_SIZE, CpuMode, Segment,
};
use aero_cpu_core::{CpuBus, CpuCore, Exception};

use aero_jit_x86::jit_ctx::{JitContext, TIER2_CTX_OFFSET, TIER2_CTX_SIZE};
use aero_jit_x86::{
    BlockLimits, JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, Tier1Bus, discover_block_mode,
};

use crate::RunExitKind;
use crate::guest_phys::{GuestRamRange, guest_ram_phys_end_exclusive, translate_guest_paddr_range};
use crate::jit_write_log::GuestWriteLog;

fn js_error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

const MAX_SAFE_INTEGER_U64: u64 = 9_007_199_254_740_991; // 2^53 - 1

fn u64_to_js_number(value: u64, label: &str) -> Result<JsValue, JsValue> {
    if value > MAX_SAFE_INTEGER_U64 {
        return Err(js_error(format!(
            "{label} exceeds JS safe integer range: {value} > {MAX_SAFE_INTEGER_U64}"
        )));
    }
    Ok(JsValue::from_f64(value as f64))
}

fn js_number_to_u64(value: JsValue, label: &str) -> Result<u64, JsValue> {
    let Some(n) = value.as_f64() else {
        return Err(js_error(format!("{label} must be a number")));
    };
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
        return Err(js_error(format!("{label} must be a non-negative integer")));
    }
    if n > MAX_SAFE_INTEGER_U64 as f64 {
        return Err(js_error(format!(
            "{label} exceeds JS safe integer range: {n}"
        )));
    }
    Ok(n as u64)
}

fn js_number_to_u32(value: JsValue, label: &str) -> Result<u32, JsValue> {
    let Some(n) = value.as_f64() else {
        return Err(js_error(format!("{label} must be a number")));
    };
    if !n.is_finite() || n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
        return Err(js_error(format!(
            "{label} must be a non-negative u32 integer"
        )));
    }
    Ok(n as u32)
}

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

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_jit_call)]
    fn js_jit_call(table_index: u32, cpu_ptr: u32, jit_ctx_ptr: u32) -> i64;
}

const JIT_EXIT_SENTINEL_I64: i64 = -1;
const DEFAULT_TLB_SALT: u64 = 0x1234_5678_9abc_def0;

/// Offset (relative to `cpu_ptr`) of a `u32` flag written by the JS host to indicate whether
/// the just-executed Tier-1 block committed architectural side effects.
///
/// The CPU worker can speculatively execute a Tier-1 block and roll back the `CpuState` + guest
/// RAM writes on runtime exits (`jit_exit`/`jit_exit_mmio`/`page_fault`). When rollback happens,
/// the block did *not* retire guest instructions, so the tiered dispatcher must not advance
/// time/TSC or interrupt shadow state.
///
/// This in-memory slot is the single source of truth for commit/rollback status; the host must
/// clear it when it rolls back guest state.
///
/// Placing this flag after the Tier-2 context avoids collisions with `TRACE_EXIT_REASON` and
/// other Tier-2 metadata slots.
const COMMIT_FLAG_OFFSET: u32 = TIER2_CTX_OFFSET + TIER2_CTX_SIZE;
const COMMIT_FLAG_BYTES: u32 = 4;
const _: () = {
    // Commit flag is a `u32` slot at a stable offset after the Tier-2 context region.
    assert!(COMMIT_FLAG_BYTES == 4);
    assert!(COMMIT_FLAG_OFFSET == TIER2_CTX_OFFSET + TIER2_CTX_SIZE);
    assert!(COMMIT_FLAG_OFFSET % 4 == 0);
};

/// Exported Tier-1 JIT ABI layout constants.
///
/// This exists so JS host code (CPU worker) can discover the exact layout of the Tier-1 JIT
/// context and Tier-2 metadata regions inside the linear-memory JIT ABI buffer, without
/// duplicating Rust-side constants that may drift over time.
#[wasm_bindgen]
pub struct TieredVmJitAbiLayout {
    jit_ctx_header_bytes: u32,
    jit_tlb_entries: u32,
    jit_tlb_entry_bytes: u32,
    tier2_ctx_bytes: u32,
    /// Offset (relative to `cpu_ptr`) of the `u32` commit flag written by the JS host.
    commit_flag_offset: u32,
    /// Offset (relative to `cpu_ptr`) of the Tier-1 JIT context pointer (`jit_ctx_ptr`).
    jit_ctx_ptr_offset: u32,
    /// Offset (relative to `cpu_ptr`) of the Tier-2 context region.
    tier2_ctx_offset: u32,
}

#[wasm_bindgen]
impl TieredVmJitAbiLayout {
    #[wasm_bindgen(getter)]
    pub fn jit_ctx_header_bytes(&self) -> u32 {
        self.jit_ctx_header_bytes
    }

    #[wasm_bindgen(getter)]
    pub fn jit_tlb_entries(&self) -> u32 {
        self.jit_tlb_entries
    }

    #[wasm_bindgen(getter)]
    pub fn jit_tlb_entry_bytes(&self) -> u32 {
        self.jit_tlb_entry_bytes
    }

    #[wasm_bindgen(getter)]
    pub fn tier2_ctx_bytes(&self) -> u32 {
        self.tier2_ctx_bytes
    }

    #[wasm_bindgen(getter)]
    pub fn commit_flag_offset(&self) -> u32 {
        self.commit_flag_offset
    }

    #[wasm_bindgen(getter)]
    pub fn jit_ctx_ptr_offset(&self) -> u32 {
        self.jit_ctx_ptr_offset
    }

    #[wasm_bindgen(getter)]
    pub fn tier2_ctx_offset(&self) -> u32 {
        self.tier2_ctx_offset
    }
}

/// Return the current Tier-1 JIT ABI layout constants for the Tiered VM.
#[wasm_bindgen]
pub fn tiered_vm_jit_abi_layout() -> TieredVmJitAbiLayout {
    TieredVmJitAbiLayout {
        // The Tier-1 JIT context header is the fixed portion before the inline TLB array.
        jit_ctx_header_bytes: JitContext::BYTE_SIZE as u32,
        jit_tlb_entries: JIT_TLB_ENTRIES as u32,
        jit_tlb_entry_bytes: JIT_TLB_ENTRY_SIZE,
        tier2_ctx_bytes: TIER2_CTX_SIZE,
        commit_flag_offset: COMMIT_FLAG_OFFSET,
        jit_ctx_ptr_offset: CPU_STATE_SIZE as u32,
        tier2_ctx_offset: TIER2_CTX_OFFSET,
    }
}

fn wasm_memory_byte_len() -> u64 {
    // `memory_size(0)` returns the number of 64KiB wasm pages.
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

#[derive(Debug)]
struct WasmBus {
    guest_base: u32,
    guest_size: u64,
    write_log: GuestWriteLog,
}

impl WasmBus {
    fn new(guest_base: u32, guest_size: u64) -> Self {
        Self {
            guest_base,
            guest_size,
            write_log: GuestWriteLog::new(),
        }
    }

    #[inline]
    fn ram_offset(&self, paddr: u64, len: usize) -> Option<u64> {
        match translate_guest_paddr_range(self.guest_size, paddr, len) {
            GuestRamRange::Ram { ram_offset } => Some(ram_offset),
            GuestRamRange::Hole | GuestRamRange::OutOfBounds => None,
        }
    }

    #[inline]
    fn log_write(&mut self, paddr: u64, len: usize) {
        self.write_log.record(paddr, len);
    }

    #[inline]
    fn drain_write_log_to(&mut self, f: impl FnMut(u64, usize)) {
        self.write_log
            .drain_to(guest_ram_phys_end_exclusive(self.guest_size), f);
    }

    fn clear_write_log(&mut self) {
        self.drain_write_log_to(|_, _| {});
    }

    #[inline]
    fn range_in_ram(&self, paddr: u64, len: usize) -> bool {
        self.ram_offset(paddr, len).is_some()
    }

    #[inline]
    fn ptr(&self, paddr: u64, len: usize) -> Result<*const u8, Exception> {
        let ram_offset = self.ram_offset(paddr, len).ok_or(Exception::MemoryFault)?;
        let linear = (self.guest_base as u64)
            .checked_add(ram_offset)
            .ok_or(Exception::MemoryFault)?;
        let linear_u32 = u32::try_from(linear).map_err(|_| Exception::MemoryFault)?;
        Ok(linear_u32 as *const u8)
    }

    #[inline]
    fn ptr_mut(&self, paddr: u64, len: usize) -> Result<*mut u8, Exception> {
        Ok(self.ptr(paddr, len)? as *mut u8)
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
    fn write_scalar<const N: usize>(
        &mut self,
        vaddr: u64,
        bytes: [u8; N],
    ) -> Result<(), Exception> {
        let ptr = self.ptr_mut(vaddr, N)?;
        self.log_write(vaddr, N);
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
        if self.range_in_ram(vaddr, 1) {
            Ok(self.read_scalar::<1>(vaddr)?[0])
        } else {
            Ok(js_mmio_read(vaddr, 1) as u8)
        }
    }

    #[inline]
    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        if self.range_in_ram(vaddr, 2) {
            return Ok(u16::from_le_bytes(self.read_scalar::<2>(vaddr)?));
        }

        if !self.any_ram_byte(vaddr, 2) {
            return Ok(js_mmio_read(vaddr, 2) as u16);
        }

        let mut bytes = [0u8; 2];
        for (i, slot) in bytes.iter_mut().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            *slot = self.read_u8(addr)?;
        }
        Ok(u16::from_le_bytes(bytes))
    }

    #[inline]
    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        if self.range_in_ram(vaddr, 4) {
            return Ok(u32::from_le_bytes(self.read_scalar::<4>(vaddr)?));
        }

        if !self.any_ram_byte(vaddr, 4) {
            return Ok(js_mmio_read(vaddr, 4));
        }

        let mut bytes = [0u8; 4];
        for (i, slot) in bytes.iter_mut().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            *slot = self.read_u8(addr)?;
        }
        Ok(u32::from_le_bytes(bytes))
    }

    #[inline]
    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        if self.range_in_ram(vaddr, 8) {
            return Ok(u64::from_le_bytes(self.read_scalar::<8>(vaddr)?));
        }

        if self.any_ram_byte(vaddr, 8) {
            let mut bytes = [0u8; 8];
            for (i, slot) in bytes.iter_mut().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                *slot = self.read_u8(addr)?;
            }
            return Ok(u64::from_le_bytes(bytes));
        }

        let lo = js_mmio_read(vaddr, 4) as u64;
        let hi_addr = vaddr.checked_add(4).ok_or(Exception::MemoryFault)?;
        let hi = js_mmio_read(hi_addr, 4) as u64;
        Ok(lo | (hi << 32))
    }

    #[inline]
    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        if self.range_in_ram(vaddr, 16) {
            return Ok(u128::from_le_bytes(self.read_scalar::<16>(vaddr)?));
        }

        if self.any_ram_byte(vaddr, 16) {
            let mut bytes = [0u8; 16];
            for (i, slot) in bytes.iter_mut().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                *slot = self.read_u8(addr)?;
            }
            return Ok(u128::from_le_bytes(bytes));
        }

        let mut out = 0u128;
        for i in 0..4u64 {
            let addr = vaddr.checked_add(i * 4).ok_or(Exception::MemoryFault)?;
            let part = js_mmio_read(addr, 4) as u128;
            out |= part << ((i * 32) as u32);
        }
        Ok(out)
    }

    #[inline]
    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        if self.range_in_ram(vaddr, 1) {
            self.write_scalar::<1>(vaddr, [val])
        } else {
            js_mmio_write(vaddr, 1, u32::from(val));
            Ok(())
        }
    }

    #[inline]
    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        if self.range_in_ram(vaddr, 2) {
            return self.write_scalar::<2>(vaddr, val.to_le_bytes());
        }

        if !self.any_ram_byte(vaddr, 2) {
            js_mmio_write(vaddr, 2, u32::from(val));
            return Ok(());
        }

        let bytes = val.to_le_bytes();
        for (i, byte) in bytes.into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            CpuBus::write_u8(self, addr, byte)?;
        }
        Ok(())
    }

    #[inline]
    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        if self.range_in_ram(vaddr, 4) {
            return self.write_scalar::<4>(vaddr, val.to_le_bytes());
        }

        if !self.any_ram_byte(vaddr, 4) {
            js_mmio_write(vaddr, 4, val);
            return Ok(());
        }

        let bytes = val.to_le_bytes();
        for (i, byte) in bytes.into_iter().enumerate() {
            let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
            CpuBus::write_u8(self, addr, byte)?;
        }
        Ok(())
    }

    #[inline]
    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        if self.range_in_ram(vaddr, 8) {
            return self.write_scalar::<8>(vaddr, val.to_le_bytes());
        }

        if self.any_ram_byte(vaddr, 8) {
            let bytes = val.to_le_bytes();
            for (i, byte) in bytes.into_iter().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                CpuBus::write_u8(self, addr, byte)?;
            }
            return Ok(());
        }

        let lo = val as u32;
        let hi = (val >> 32) as u32;
        js_mmio_write(vaddr, 4, lo);
        let hi_addr = vaddr.checked_add(4).ok_or(Exception::MemoryFault)?;
        js_mmio_write(hi_addr, 4, hi);
        Ok(())
    }

    #[inline]
    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        if self.range_in_ram(vaddr, 16) {
            return self.write_scalar::<16>(vaddr, val.to_le_bytes());
        }

        if self.any_ram_byte(vaddr, 16) {
            let bytes = val.to_le_bytes();
            for (i, byte) in bytes.into_iter().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                CpuBus::write_u8(self, addr, byte)?;
            }
            return Ok(());
        }

        for i in 0..4u64 {
            let addr = vaddr.checked_add(i * 4).ok_or(Exception::MemoryFault)?;
            let part = (val >> ((i * 32) as u32)) as u32;
            js_mmio_write(addr, 4, part);
        }
        Ok(())
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        if dst.is_empty() {
            return Ok(());
        }

        if self.range_in_ram(vaddr, dst.len()) {
            let ptr = self.ptr(vaddr, dst.len())?;
            // Safety: `ptr()` bounds-checks.
            unsafe {
                core::ptr::copy_nonoverlapping(ptr, dst.as_mut_ptr(), dst.len());
            }
            Ok(())
        } else {
            for (i, slot) in dst.iter_mut().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                *slot = CpuBus::read_u8(self, addr)?;
            }
            Ok(())
        }
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        if src.is_empty() {
            return Ok(());
        }

        if self.range_in_ram(vaddr, src.len()) {
            let ptr = self.ptr_mut(vaddr, src.len())?;
            self.log_write(vaddr, src.len());
            // Safety: `ptr_mut()` bounds-checks.
            unsafe {
                core::ptr::copy_nonoverlapping(src.as_ptr(), ptr, src.len());
            }
            Ok(())
        } else {
            self.preflight_write_bytes(vaddr, src.len())?;
            for (i, byte) in src.iter().copied().enumerate() {
                let addr = vaddr.checked_add(i as u64).ok_or(Exception::MemoryFault)?;
                CpuBus::write_u8(self, addr, byte)?;
            }
            Ok(())
        }
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        // Validate address arithmetic so callers get a deterministic exception instead of wraparound.
        let _ = vaddr
            .checked_add(len as u64)
            .ok_or(Exception::MemoryFault)?;

        // Only preflight in-RAM pointer ranges. Out-of-RAM ranges are treated as MMIO and cannot be
        // validated without performing the access.
        if self.range_in_ram(vaddr, len) {
            let _ = self.ptr_mut(vaddr, len)?;
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
        if !self.range_in_ram(dst, len) || !self.range_in_ram(src, len) {
            return Ok(false);
        }
        let dst_ptr = self.ptr_mut(dst, len)?;
        let src_ptr = self.ptr(src, len)?;
        self.log_write(dst, len);
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
        if !self.range_in_ram(dst, total) {
            return Ok(false);
        }
        let dst_ptr = self.ptr_mut(dst, total)?;
        self.log_write(dst, total);
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

impl Tier1Bus for WasmBus {
    fn read_u8(&self, addr: u64) -> u8 {
        if self.range_in_ram(addr, 1) {
            self.read_scalar::<1>(addr).map(|b| b[0]).unwrap_or(0)
        } else {
            js_mmio_read(addr, 1) as u8
        }
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        if self.range_in_ram(addr, 1) {
            let _ = self.write_scalar::<1>(addr, [value]);
        } else {
            js_mmio_write(addr, 1, u32::from(value));
        }
    }
}

fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

#[derive(Debug, Default)]
struct CompileQueueInner {
    queue: VecDeque<u64>,
    pending: HashSet<u64>,
}

/// De-duping FIFO compile request sink.
#[derive(Debug, Default, Clone)]
struct CompileQueue {
    inner: Rc<std::cell::RefCell<CompileQueueInner>>,
}

impl CompileQueue {
    fn new() -> Self {
        Self::default()
    }

    fn drain(&self) -> Vec<u64> {
        let mut inner = self.inner.borrow_mut();
        let drained: Vec<u64> = inner.queue.drain(..).collect();
        inner.pending.clear();
        drained
    }

    fn clear(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.queue.clear();
        inner.pending.clear();
    }
}

impl CompileRequestSink for CompileQueue {
    fn request_compile(&mut self, entry_rip: u64) {
        let mut inner = self.inner.borrow_mut();
        if inner.pending.insert(entry_rip) {
            inner.queue.push_back(entry_rip);
        }
    }
}

/// Linear-memory buffer that satisfies the Tier-1 JIT ABI layout contract:
/// `CpuState` followed by a trailing JIT context and Tier-2 context region.
#[derive(Debug)]
struct JitAbiBuffer {
    backing: Vec<u8>,
    cpu_off: usize,
    len: usize,
}

impl JitAbiBuffer {
    fn new(len: usize, align: usize) -> Self {
        let alloc_len = len
            .checked_add(align.saturating_sub(1))
            .expect("jit abi buffer alloc overflow");
        let backing = vec![0u8; alloc_len];
        let base = backing.as_ptr() as usize;
        let rem = base % align;
        let cpu_off = if rem == 0 { 0 } else { align - rem };
        assert!(
            cpu_off + len <= backing.len(),
            "jit abi buffer alignment overflow"
        );
        Self {
            backing,
            cpu_off,
            len,
        }
    }

    #[inline]
    fn cpu_ptr(&self) -> u32 {
        (self.backing.as_ptr() as usize + self.cpu_off) as u32
    }

    #[inline]
    fn jit_ctx_ptr(&self) -> u32 {
        self.cpu_ptr()
            .checked_add(CPU_STATE_SIZE as u32)
            .expect("jit_ctx_ptr overflow")
    }

    #[inline]
    fn slice_mut(&mut self) -> &mut [u8] {
        let start = self.cpu_off;
        let end = start + self.len;
        &mut self.backing[start..end]
    }

    fn clear_tlb(&mut self) {
        let cpu_ptr = self.cpu_ptr() as usize;
        let jit_ctx_ptr = self.jit_ctx_ptr() as usize;
        // Clear just the direct-mapped TLB entries (leave header intact).
        let tlb_off = jit_ctx_ptr + (JitContext::TLB_OFFSET as usize) - cpu_ptr;
        let tlb_end = tlb_off + JitContext::TLB_BYTES;
        self.slice_mut()[tlb_off..tlb_end].fill(0);
    }

    fn init_jit_ctx_header(&mut self, guest_base: u64, tlb_salt: u64) {
        let cpu_ptr = self.cpu_ptr() as usize;
        let jit_ctx_ptr = self.jit_ctx_ptr() as usize;
        let base = jit_ctx_ptr - cpu_ptr;
        let ctx = JitContext {
            ram_base: guest_base,
            tlb_salt,
        };
        ctx.write_header_to_mem(self.slice_mut(), base);
    }
}

#[derive(Debug)]
struct WasmJitBackend {
    cpu_ptr: u32,
    jit_ctx_ptr: u32,
    guest_base: u32,
    tlb_salt: u64,
}

impl WasmJitBackend {
    fn new(cpu_ptr: u32, jit_ctx_ptr: u32, guest_base: u32, tlb_salt: u64) -> Self {
        Self {
            cpu_ptr,
            jit_ctx_ptr,
            guest_base,
            tlb_salt,
        }
    }

    #[inline]
    fn write_u64_at(&self, addr: u32, value: u64) {
        // Safety: `addr` is a wasm linear-memory pointer into the dedicated JIT ABI buffer.
        unsafe {
            core::ptr::write_unaligned(addr as *mut u64, value);
        }
    }

    #[inline]
    fn write_u32_at(&self, addr: u32, value: u32) {
        // Safety: `addr` is a wasm linear-memory pointer into the dedicated JIT ABI buffer.
        unsafe {
            core::ptr::write_unaligned(addr as *mut u32, value);
        }
    }

    #[inline]
    fn read_u64_at(&self, addr: u32) -> u64 {
        // Safety: `addr` is a wasm linear-memory pointer into the dedicated JIT ABI buffer.
        unsafe { core::ptr::read_unaligned(addr as *const u64) }
    }

    #[inline]
    fn read_u32_at(&self, addr: u32) -> u32 {
        // Safety: `addr` is a wasm linear-memory pointer into the dedicated JIT ABI buffer.
        unsafe { core::ptr::read_unaligned(addr as *const u32) }
    }

    fn sync_cpu_to_abi(&self, state: &aero_cpu_core::state::CpuState) {
        for i in 0..16 {
            let off = CPU_GPR_OFF[i] as u32;
            self.write_u64_at(self.cpu_ptr + off, state.gpr[i]);
        }
        self.write_u64_at(self.cpu_ptr + (CPU_RIP_OFF as u32), state.rip);
        self.write_u64_at(
            self.cpu_ptr + (CPU_RFLAGS_OFF as u32),
            state.rflags_snapshot(),
        );

        // Ensure the JIT context header remains initialized (in case a previous block or host
        // reset clobbered it).
        self.write_u64_at(
            self.jit_ctx_ptr + JitContext::RAM_BASE_OFFSET,
            self.guest_base as u64,
        );
        self.write_u64_at(
            self.jit_ctx_ptr + JitContext::TLB_SALT_OFFSET,
            self.tlb_salt,
        );
    }

    fn sync_cpu_from_abi(&self, state: &mut aero_cpu_core::state::CpuState) {
        for i in 0..16 {
            let off = CPU_GPR_OFF[i] as u32;
            state.gpr[i] = self.read_u64_at(self.cpu_ptr + off);
        }
        state.rip = self.read_u64_at(self.cpu_ptr + (CPU_RIP_OFF as u32));
        let rflags = self.read_u64_at(self.cpu_ptr + (CPU_RFLAGS_OFF as u32));
        state.set_rflags(rflags);
    }
}

impl JitBackend for WasmJitBackend {
    type Cpu = Vcpu<WasmBus>;

    fn execute(&mut self, table_index: u32, cpu: &mut Self::Cpu) -> JitBlockExit {
        self.sync_cpu_to_abi(&cpu.cpu.state);

        let commit_flag_ptr = self
            .cpu_ptr
            .checked_add(COMMIT_FLAG_OFFSET)
            .expect("commit_flag_ptr overflow");
        // Default to "committed": JS will clear this flag if it rolled back guest state.
        self.write_u32_at(commit_flag_ptr, 1);

        let ret = js_jit_call(table_index, self.cpu_ptr, self.jit_ctx_ptr);

        self.sync_cpu_from_abi(&mut cpu.cpu.state);

        let exit_to_interpreter = ret == JIT_EXIT_SENTINEL_I64;
        let committed = self.read_u32_at(commit_flag_ptr) != 0;
        let next_rip = if exit_to_interpreter {
            cpu.cpu.state.rip()
        } else {
            ret as u64
        };

        JitBlockExit {
            next_rip,
            exit_to_interpreter,
            committed,
        }
    }
}

type WasmTieredDispatcher = ExecDispatcher<Tier0Interpreter, WasmJitBackend, CompileQueue>;

/// Tiered (Tier-0 + Tier-1) VM wrapper intended for the browser CPU worker.
#[wasm_bindgen]
pub struct WasmTieredVm {
    guest_base: u32,
    guest_size: u64,

    vcpu: Vcpu<WasmBus>,
    dispatcher: WasmTieredDispatcher,
    compile_queue: CompileQueue,

    jit_abi: JitAbiBuffer,
    jit_config: JitConfig,
    tlb_salt: u64,

    total_interp_blocks: u64,
    total_jit_blocks: u64,
}

impl WasmTieredVm {
    fn flush_guest_writes(&mut self) {
        let jit = self.dispatcher.jit_mut();
        self.vcpu
            .bus
            .drain_write_log_to(|paddr, len| jit.on_guest_write(paddr, len));
    }
}

#[wasm_bindgen]
impl WasmTieredVm {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(JsValue::from_str("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();

        // Accept `guest_size == 0` as "use the remainder of linear memory".
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };
        // Keep guest RAM below the PCI MMIO BAR window (see `guest_ram_layout` contract).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| JsValue::from_str("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(JsValue::from_str(&format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        let bus = WasmBus::new(guest_base, guest_size_u64);

        let cpu = CpuCore::new(CpuMode::Real);
        let vcpu = Vcpu::new(cpu, bus);

        let compile_queue = CompileQueue::new();

        // JIT ABI buffer: CpuState + jit_ctx + tier2_ctx + commit flag.
        let jit_abi_len = (COMMIT_FLAG_OFFSET + COMMIT_FLAG_BYTES) as usize;
        let mut jit_abi = JitAbiBuffer::new(jit_abi_len, CPU_STATE_ALIGN);
        let tlb_salt = DEFAULT_TLB_SALT;
        jit_abi.init_jit_ctx_header(guest_base as u64, tlb_salt);

        let cpu_ptr = jit_abi.cpu_ptr();
        let jit_ctx_ptr = jit_abi.jit_ctx_ptr();

        let backend = WasmJitBackend::new(cpu_ptr, jit_ctx_ptr, guest_base, tlb_salt);

        let jit_config = JitConfig {
            enabled: true,
            // Low threshold for browser bring-up: we want hot blocks to quickly transition to
            // Tier-1 so the JS wiring can observe JIT execution.
            hot_threshold: 3,
            cache_max_blocks: 1024,
            cache_max_bytes: 0,
        };

        let jit = JitRuntime::new(jit_config.clone(), backend, compile_queue.clone());

        let interp = Tier0Interpreter::new(10_000);
        let dispatcher = ExecDispatcher::new(interp, jit);

        Ok(Self {
            guest_base,
            guest_size: guest_size_u64,
            vcpu,
            dispatcher,
            compile_queue,
            jit_abi,
            jit_config,
            tlb_salt,
            total_interp_blocks: 0,
            total_jit_blocks: 0,
        })
    }

    #[wasm_bindgen(getter)]
    pub fn guest_base(&self) -> u32 {
        self.guest_base
    }

    #[wasm_bindgen(getter)]
    pub fn guest_size(&self) -> u32 {
        self.guest_size.min(u64::from(u32::MAX)) as u32
    }

    /// Total number of interpreted basic blocks executed since the last reset.
    #[wasm_bindgen(getter)]
    pub fn interp_blocks_total(&self) -> u64 {
        self.total_interp_blocks
    }

    /// Total number of JIT basic blocks executed since the last reset.
    #[wasm_bindgen(getter)]
    pub fn jit_blocks_total(&self) -> u64 {
        self.total_jit_blocks
    }

    /// Reset CPU state to 16-bit real mode and set `CS:IP = 0x0000:entry_ip`.
    pub fn reset_real_mode(&mut self, entry_ip: u32) {
        self.vcpu.cpu = CpuCore::new(CpuMode::Real);
        self.vcpu.cpu.state.halted = false;
        self.vcpu.cpu.state.clear_pending_bios_int();
        self.vcpu.exit = None;

        // Real-mode: base = selector<<4, 64KiB limit.
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.cs, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.ds, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.es, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.ss, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.fs, 0);
        set_real_mode_seg(&mut self.vcpu.cpu.state.segments.gs, 0);

        self.vcpu.cpu.state.set_rip(entry_ip as u64);

        // Reset tiering state.
        self.compile_queue.clear();
        self.total_interp_blocks = 0;
        self.total_jit_blocks = 0;
        self.vcpu.bus.clear_write_log();

        // Reset JIT ABI context (especially the inline-TLB fast-path state).
        self.jit_abi.clear_tlb();
        self.jit_abi
            .init_jit_ctx_header(self.guest_base as u64, self.tlb_salt);

        // Reset the tiering engine itself (code cache + hotness profile). Resetting just the CPU
        // core is not enough: older compiled blocks may no longer be valid after a boot sector is
        // reloaded or a snapshot restore changes guest RAM.
        let cpu_ptr = self.jit_abi.cpu_ptr();
        let jit_ctx_ptr = self.jit_abi.jit_ctx_ptr();
        let backend = WasmJitBackend::new(cpu_ptr, jit_ctx_ptr, self.guest_base, self.tlb_salt);
        let jit = JitRuntime::new(self.jit_config.clone(), backend, self.compile_queue.clone());
        let interp = Tier0Interpreter::new(10_000);
        self.dispatcher = ExecDispatcher::new(interp, jit);
    }

    /// Return the linear-memory pointer to the `CpuState.a20_enabled` flag.
    ///
    /// The browser runtime updates the A20 gate state in response to i8042/system-control events.
    /// As with [`crate::WasmVm::a20_enabled_ptr`], those events can arrive while the VM is executing
    /// (e.g. during a port I/O exit), so the host should avoid re-entering WASM to mutate
    /// `WasmTieredVm` and instead write `0`/`1` directly to the returned address.
    pub fn a20_enabled_ptr(&self) -> u32 {
        // Safety: the pointer is into this module's linear memory (wasm32); JS can treat it as an
        // absolute byte offset into `WebAssembly.Memory.buffer`.
        core::ptr::addr_of!(self.vcpu.cpu.state.a20_enabled) as u32
    }

    /// Notify the JIT runtime that guest code bytes were modified in-place (e.g. via DMA or host
    /// I/O worker writes).
    ///
    /// This bumps the internal page-version tracker so any compiled blocks covering the modified
    /// pages are rejected or recompiled.
    pub fn jit_on_guest_write(&mut self, paddr: u64, len: u32) {
        self.dispatcher
            .jit_mut()
            .on_guest_write(paddr, len as usize);
    }

    /// Drain de-duplicated Tier-1 compilation requests (entry RIPs) as `BigInt` values.
    pub fn drain_compile_requests(&mut self) -> Array {
        let drained = self.compile_queue.drain();
        let arr = Array::new();
        for rip in drained {
            arr.push(&BigInt::from(rip).into());
        }
        arr
    }

    /// Snapshot page-version metadata for a range of guest code bytes.
    ///
    /// JS should capture this as close as possible to when a background JIT worker reads the
    /// guest code bytes. Installing a compiled block with a stale snapshot (e.g. due to
    /// self-modifying code) will be rejected and recompilation requested.
    pub fn snapshot_meta(&mut self, code_paddr: u64, byte_len: u32) -> Result<JsValue, JsValue> {
        let jit = self.dispatcher.jit_mut();
        let meta = jit.snapshot_meta(code_paddr, byte_len);
        meta_to_js(&meta)
    }

    /// Install a compiled Tier-1 block into the JIT cache using a pre-snapshotted metadata
    /// object (from [`Self::snapshot_meta`]).
    ///
    /// Returns an array of evicted entry RIPs (`BigInt`) so JS can free/reuse table slots.
    pub fn install_handle(
        &mut self,
        entry_rip: u64,
        table_index: u32,
        meta: JsValue,
    ) -> Result<Array, JsValue> {
        let mut meta = meta_from_js(meta)?;

        // Fill in the execution bookkeeping fields that are not part of the JS meta payload.
        // See `install_tier1_block` for more context.
        let instruction_count = {
            let max_bytes = usize::try_from(meta.byte_len).unwrap_or(usize::MAX).max(1);
            let limits = BlockLimits {
                max_insts: 64,
                max_bytes,
            };
            let bitness = self.vcpu.cpu.state.bitness();
            let block = discover_block_mode(&self.vcpu.bus, entry_rip, limits, bitness);
            let mut count = u32::try_from(block.insts.len()).unwrap_or(u32::MAX);
            if matches!(
                block.end_kind,
                aero_jit_x86::BlockEndKind::ExitToInterpreter { .. }
            ) {
                count = count.saturating_sub(1);
            }
            count
        };
        meta.instruction_count = instruction_count;
        meta.inhibit_interrupts_after_block = false;

        let evicted = self
            .dispatcher
            .jit_mut()
            .install_handle(CompiledBlockHandle {
                entry_rip,
                table_index,
                meta,
            });

        let arr = Array::new();
        for rip in evicted {
            arr.push(&BigInt::from(rip).into());
        }
        Ok(arr)
    }

    pub fn is_compiled(&mut self, entry_rip: u64) -> bool {
        self.dispatcher.jit_mut().is_compiled(entry_rip)
    }

    pub fn cache_len(&mut self) -> u32 {
        self.dispatcher.jit_mut().cache_len().min(u32::MAX as usize) as u32
    }

    /// Notify the tiered runtime that the guest wrote to physical memory.
    ///
    /// This is intended for browser JIT integrations where Tier-1 blocks route stores through a
    /// host-provided helper (`env.mem_write_*`). The helper can call this method to keep the
    /// embedded [`JitRuntime`] page-version tracker in sync so cached blocks are invalidated when
    /// the guest modifies code pages.
    pub fn on_guest_write(&mut self, paddr: u64, byte_len: u32) {
        self.dispatcher
            .jit_mut()
            .on_guest_write(paddr, byte_len as usize);
    }

    /// Install a compiled Tier-1 block into the JIT cache.
    ///
    /// Returns an array of evicted entry RIPs (`BigInt`) so JS can free/reuse table slots.
    pub fn install_tier1_block(
        &mut self,
        entry_rip: u64,
        table_index: u32,
        code_paddr: u64,
        byte_len: u32,
    ) -> Array {
        // `JitRuntime::install_block` uses `snapshot_meta` which does not know how many guest
        // instructions the block will retire. The tiered dispatcher uses that instruction count to
        // advance time/TSC and maintain interrupt bookkeeping after a committed JIT exit.
        //
        // Compute a conservative instruction count from the current guest bytes.
        let instruction_count = {
            let max_bytes = usize::try_from(byte_len).unwrap_or(usize::MAX).max(1);
            let limits = BlockLimits {
                max_insts: 64,
                max_bytes,
            };
            let bitness = self.vcpu.cpu.state.bitness();
            let block = discover_block_mode(&self.vcpu.bus, entry_rip, limits, bitness);
            let mut count = u32::try_from(block.insts.len()).unwrap_or(u32::MAX);
            if matches!(
                block.end_kind,
                aero_jit_x86::BlockEndKind::ExitToInterpreter { .. }
            ) {
                count = count.saturating_sub(1);
            }
            count
        };

        let jit = self.dispatcher.jit_mut();
        let mut meta = jit.snapshot_meta(code_paddr, byte_len);
        meta.instruction_count = instruction_count;
        meta.inhibit_interrupts_after_block = false;

        let evicted = jit.install_handle(CompiledBlockHandle {
            entry_rip,
            table_index,
            meta,
        });
        let arr = Array::new();
        for rip in evicted {
            arr.push(&BigInt::from(rip).into());
        }
        arr
    }

    /// Execute up to `max_blocks` basic blocks.
    ///
    /// Returns a JS object:
    /// `{ kind, detail, executed_blocks, interp_blocks, jit_blocks }`.
    pub fn run_blocks(&mut self, max_blocks: u32) -> Result<JsValue, JsValue> {
        let max_blocks_u64 = u64::from(max_blocks);
        let mut executed_blocks = 0u64;
        let mut interp_blocks = 0u64;
        let mut jit_blocks = 0u64;

        let mut kind = RunExitKind::Completed;
        let mut detail = String::new();

        if max_blocks_u64 == 0 {
            let obj = Object::new();
            Reflect::set(
                &obj,
                &JsValue::from_str("kind"),
                &JsValue::from(kind as u32),
            )
            .map_err(|_| js_error("Failed to build run_blocks result (kind)"))?;
            Reflect::set(
                &obj,
                &JsValue::from_str("detail"),
                &JsValue::from(detail.clone()),
            )
            .map_err(|_| js_error("Failed to build run_blocks result (detail)"))?;
            Reflect::set(
                &obj,
                &JsValue::from_str("executed_blocks"),
                &JsValue::from(0u32),
            )
            .map_err(|_| js_error("Failed to build run_blocks result (executed_blocks)"))?;
            Reflect::set(
                &obj,
                &JsValue::from_str("interp_blocks"),
                &JsValue::from(0u32),
            )
            .map_err(|_| js_error("Failed to build run_blocks result (interp_blocks)"))?;
            Reflect::set(&obj, &JsValue::from_str("jit_blocks"), &JsValue::from(0u32))
                .map_err(|_| js_error("Failed to build run_blocks result (jit_blocks)"))?;
            return Ok(obj.into());
        }

        // Fast-path: if we're already halted or exited, surface that immediately.
        if self.vcpu.cpu.state.halted {
            kind = RunExitKind::Halted;
        } else if let Some(exit) = self.vcpu.exit {
            kind = match exit {
                aero_cpu_core::interrupts::CpuExit::TripleFault => RunExitKind::ResetRequested,
                _ => RunExitKind::Exception,
            };
            detail = format!("{exit:?}");
        } else if let Some(vector) = self.vcpu.cpu.state.take_pending_bios_int() {
            kind = RunExitKind::Assist;
            detail = format!("bios_int(0x{vector:02x})");
        } else {
            while executed_blocks < max_blocks_u64 {
                // Stop if the CPU is no longer runnable.
                if self.vcpu.cpu.state.halted {
                    kind = RunExitKind::Halted;
                    break;
                }
                if let Some(exit) = self.vcpu.exit {
                    kind = match exit {
                        aero_cpu_core::interrupts::CpuExit::TripleFault => {
                            RunExitKind::ResetRequested
                        }
                        _ => RunExitKind::Exception,
                    };
                    detail = format!("{exit:?}");
                    break;
                }

                let outcome = self.dispatcher.step(&mut self.vcpu);
                // Propagate Tier-0 writes (and interrupt-frame pushes) into the JIT page-version
                // tracker so self-modifying code invalidation stays correct.
                self.flush_guest_writes();
                match outcome {
                    StepOutcome::InterruptDelivered => {
                        // Interrupt delivery does not count as a block execution for the
                        // cooperative slice budget.
                        continue;
                    }
                    StepOutcome::Block { tier, .. } => {
                        executed_blocks += 1;
                        match tier {
                            ExecutedTier::Interpreter => {
                                interp_blocks += 1;
                                self.total_interp_blocks =
                                    self.total_interp_blocks.saturating_add(1);
                            }
                            ExecutedTier::Jit => {
                                jit_blocks += 1;
                                self.total_jit_blocks = self.total_jit_blocks.saturating_add(1);
                            }
                        }
                    }
                }

                if let Some(vector) = self.vcpu.cpu.state.take_pending_bios_int() {
                    kind = RunExitKind::Assist;
                    detail = format!("bios_int(0x{vector:02x})");
                    break;
                }
            }
        }

        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("kind"),
            &JsValue::from(kind as u32),
        )
        .map_err(|_| js_error("Failed to build run_blocks result (kind)"))?;
        Reflect::set(&obj, &JsValue::from_str("detail"), &JsValue::from(detail))
            .map_err(|_| js_error("Failed to build run_blocks result (detail)"))?;
        Reflect::set(
            &obj,
            &JsValue::from_str("executed_blocks"),
            &JsValue::from(executed_blocks.min(u64::from(u32::MAX)) as u32),
        )
        .map_err(|_| js_error("Failed to build run_blocks result (executed_blocks)"))?;
        Reflect::set(
            &obj,
            &JsValue::from_str("interp_blocks"),
            &JsValue::from(interp_blocks.min(u64::from(u32::MAX)) as u32),
        )
        .map_err(|_| js_error("Failed to build run_blocks result (interp_blocks)"))?;
        Reflect::set(
            &obj,
            &JsValue::from_str("jit_blocks"),
            &JsValue::from(jit_blocks.min(u64::from(u32::MAX)) as u32),
        )
        .map_err(|_| js_error("Failed to build run_blocks result (jit_blocks)"))?;

        Ok(obj.into())
    }
}

fn meta_to_js(meta: &CompiledBlockMeta) -> Result<JsValue, JsValue> {
    let obj = Object::new();

    Reflect::set(
        &obj,
        &JsValue::from_str("code_paddr"),
        &u64_to_js_number(meta.code_paddr, "meta.code_paddr")?,
    )
    .map_err(|_| js_error("Failed to set meta.code_paddr"))?;
    Reflect::set(
        &obj,
        &JsValue::from_str("byte_len"),
        &JsValue::from_f64(meta.byte_len as f64),
    )
    .map_err(|_| js_error("Failed to set meta.byte_len"))?;

    let versions = Array::new();
    for snap in &meta.page_versions {
        versions.push(&page_snapshot_to_js(snap)?);
    }

    Reflect::set(&obj, &JsValue::from_str("page_versions"), versions.as_ref())
        .map_err(|_| js_error("Failed to set meta.page_versions"))?;

    Ok(obj.into())
}

fn page_snapshot_to_js(snap: &PageVersionSnapshot) -> Result<JsValue, JsValue> {
    let obj = Object::new();
    Reflect::set(
        &obj,
        &JsValue::from_str("page"),
        &u64_to_js_number(snap.page, "snapshot.page")?,
    )
    .map_err(|_| js_error("Failed to set snapshot.page"))?;
    Reflect::set(
        &obj,
        &JsValue::from_str("version"),
        &JsValue::from_f64(snap.version as f64),
    )
    .map_err(|_| js_error("Failed to set snapshot.version"))?;
    Ok(obj.into())
}

fn meta_from_js(meta: JsValue) -> Result<CompiledBlockMeta, JsValue> {
    if !meta.is_object() {
        return Err(js_error("meta must be an object"));
    }

    let code_paddr_val = Reflect::get(&meta, &JsValue::from_str("code_paddr"))
        .map_err(|_| js_error("meta.code_paddr missing"))?;
    if code_paddr_val.is_undefined() || code_paddr_val.is_null() {
        return Err(js_error("meta.code_paddr missing"));
    }
    let code_paddr = js_number_to_u64(code_paddr_val, "meta.code_paddr")?;

    let byte_len_val = Reflect::get(&meta, &JsValue::from_str("byte_len"))
        .map_err(|_| js_error("meta.byte_len missing"))?;
    if byte_len_val.is_undefined() || byte_len_val.is_null() {
        return Err(js_error("meta.byte_len missing"));
    }
    let byte_len = js_number_to_u32(byte_len_val, "meta.byte_len")?;

    let page_versions_val = Reflect::get(&meta, &JsValue::from_str("page_versions"))
        .map_err(|_| js_error("meta.page_versions missing"))?;
    if page_versions_val.is_undefined() || page_versions_val.is_null() {
        return Err(js_error("meta.page_versions missing"));
    }
    let versions_arr = Array::from(&page_versions_val);

    // Enforce that `page_versions` matches the covered range.
    // This guards against malformed JS inputs and prevents a buggy host from
    // accidentally snapshotting the wrong pages (which would defeat stale-code
    // rejection).
    let (start_page, end_page, expected) = if byte_len == 0 {
        (0, 0, 0)
    } else {
        let start_page = code_paddr >> PAGE_SHIFT;
        let end = code_paddr.saturating_add(byte_len as u64 - 1);
        let end_page = end >> PAGE_SHIFT;
        (start_page, end_page, (end_page - start_page + 1) as usize)
    };
    if expected != versions_arr.length() as usize {
        return Err(js_error(format!(
            "meta.page_versions length mismatch: expected {expected} entries for code_paddr=0x{code_paddr:x} byte_len={byte_len}, got {}",
            versions_arr.length()
        )));
    }

    let mut page_versions = Vec::with_capacity(expected);
    for (idx, entry) in versions_arr.iter().enumerate() {
        let snap = page_snapshot_from_js(entry)?;

        // Require the pages to be contiguous and ordered `[start_page..=end_page]`.
        // This matches `PageVersionTracker::snapshot` and allows `JitRuntime` to
        // validate staleness without needing to sort/dedup.
        //
        // Note: we validate this while parsing so we can safely allocate based on
        // `expected` (derived from `code_paddr` + `byte_len`) rather than the JS
        // array length, which could otherwise be attacker-controlled.
        if expected != 0 {
            let expected_page = start_page + idx as u64;
            if snap.page != expected_page {
                return Err(js_error(format!(
                    "meta.page_versions[{idx}].page mismatch: expected page {expected_page} (range {start_page}..={end_page}) for code_paddr=0x{code_paddr:x} byte_len={byte_len}, got {}",
                    snap.page
                )));
            }
        }

        page_versions.push(snap);
    }

    Ok(CompiledBlockMeta {
        code_paddr,
        byte_len,
        page_versions,
        instruction_count: 0,
        inhibit_interrupts_after_block: false,
    })
}

fn page_snapshot_from_js(obj: JsValue) -> Result<PageVersionSnapshot, JsValue> {
    if !obj.is_object() {
        return Err(js_error("meta.page_versions entry must be an object"));
    }
    let page = js_number_to_u64(
        Reflect::get(&obj, &JsValue::from_str("page"))
            .map_err(|_| js_error("snapshot.page missing"))?,
        "snapshot.page",
    )?;
    let version = js_number_to_u32(
        Reflect::get(&obj, &JsValue::from_str("version"))
            .map_err(|_| js_error("snapshot.version missing"))?,
        "snapshot.version",
    )?;
    Ok(PageVersionSnapshot { page, version })
}

// -----------------------------------------------------------------------------
// wasm-bindgen tests (MMIO routing)
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{WasmBus, WasmTieredVm, meta_from_js};

    use aero_cpu_core::CpuBus;
    use js_sys::{Array, Object, Reflect};
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen(inline_js = r#"
export function installAeroTieredMmioTestShims() {
  globalThis.__aero_test_tiered_mmio_calls = [];

  globalThis.__aero_mmio_read = function (addr, size) {
    globalThis.__aero_test_tiered_mmio_calls.push({
      kind: "read",
      addr: Number(addr),
      size: size >>> 0,
    });
    return Number(addr) >>> 0;
  };

  globalThis.__aero_mmio_write = function (addr, size, value) {
    globalThis.__aero_test_tiered_mmio_calls.push({
      kind: "write",
      addr: Number(addr),
      size: size >>> 0,
      value: value >>> 0,
    });
  };

  if (typeof globalThis.__aero_io_port_read !== "function") {
    globalThis.__aero_io_port_read = function (_port, _size) { return 0; };
  }
  if (typeof globalThis.__aero_io_port_write !== "function") {
    globalThis.__aero_io_port_write = function (_port, _size, _value) { };
  }
  if (typeof globalThis.__aero_jit_call !== "function") {
    // Default Tier-1 JIT hook: force an interpreter exit.
    globalThis.__aero_jit_call = function (_tableIndex, _cpuPtr, _jitCtxPtr) {
      return -1n;
    };
  }
}
"#)]
    extern "C" {
        fn installAeroTieredMmioTestShims();
    }

    fn mmio_calls() -> Array {
        let global = js_sys::global();
        let calls = Reflect::get(&global, &JsValue::from_str("__aero_test_tiered_mmio_calls"))
            .expect("get __aero_test_tiered_mmio_calls");
        calls
            .dyn_into::<Array>()
            .expect("__aero_test_tiered_mmio_calls must be an Array")
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

    fn js_err_message(err: JsValue) -> String {
        if let Some(e) = err.dyn_ref::<js_sys::Error>() {
            return e.message().into();
        }
        if let Some(s) = err.as_string() {
            return s;
        }
        "<non-string js error>".to_string()
    }

    fn page_snapshot_obj(page: u64, version: u32) -> JsValue {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("page"),
            &JsValue::from_f64(page as f64),
        )
        .expect("set snapshot.page");
        Reflect::set(
            &obj,
            &JsValue::from_str("version"),
            &JsValue::from_f64(version as f64),
        )
        .expect("set snapshot.version");
        obj.into()
    }

    fn meta_obj(code_paddr: u64, byte_len: u32, page_versions: &[JsValue]) -> JsValue {
        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("code_paddr"),
            &JsValue::from_f64(code_paddr as f64),
        )
        .expect("set meta.code_paddr");
        Reflect::set(
            &obj,
            &JsValue::from_str("byte_len"),
            &JsValue::from_f64(byte_len as f64),
        )
        .expect("set meta.byte_len");
        let versions = Array::new();
        for v in page_versions {
            versions.push(v);
        }
        Reflect::set(&obj, &JsValue::from_str("page_versions"), versions.as_ref())
            .expect("set meta.page_versions");
        obj.into()
    }

    #[wasm_bindgen_test]
    fn wasm_tiered_bus_routes_out_of_ram_accesses_to_mmio() {
        installAeroTieredMmioTestShims();

        let mut guest = vec![0u8; 0x10];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u64;

        let mut bus = WasmBus::new(guest_base, guest_size);

        bus.write_u32(0, 0x1122_3344).expect("write_u32");
        assert_eq!(bus.read_u32(0).expect("read_u32"), 0x1122_3344);
        assert_eq!(
            mmio_calls().length(),
            0,
            "unexpected MMIO calls for RAM access"
        );

        assert_eq!(bus.read_u32(0x20).expect("mmio read_u32"), 0x20);
        bus.write_u16(0x30, 0x1234).expect("mmio write_u16");

        let calls = mmio_calls();
        assert_eq!(calls.length(), 2);

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
    fn wasm_tiered_bus_routes_ecam_hole_to_mmio_when_ram_exceeds_ecam_base() {
        installAeroTieredMmioTestShims();

        // Allocate a tiny backing buffer; we will only dereference low RAM addresses (0..len).
        //
        // Regression target: the Tiered VM must not treat the Q35 ECAM/PCI/MMIO hole
        // ([PCIE_ECAM_BASE..4GiB)) as RAM when the *RAM byte size* exceeds PCIE_ECAM_BASE.
        let mut guest = vec![0u8; 0x100];
        let guest_base = guest.as_mut_ptr() as u32;

        // Simulate a machine with low RAM up to PCIE_ECAM_BASE and 8KiB of remapped high RAM above
        // 4GiB. This does not require allocating multi-gigabyte buffers because we only touch low
        // RAM and hole addresses in the test.
        let guest_size = crate::guest_phys::PCIE_ECAM_BASE + 0x2000;
        let mut bus = WasmBus::new(guest_base, guest_size);

        // Low RAM should still be directly accessible.
        bus.write_u32(0, 0x1122_3344).expect("write_u32");
        assert_eq!(bus.read_u32(0).expect("read_u32"), 0x1122_3344);

        // ECAM hole accesses must route to MMIO (and must not attempt a direct RAM dereference).
        let hole = crate::guest_phys::PCIE_ECAM_BASE;
        assert_eq!(bus.read_u32(hole).expect("mmio read_u32"), hole as u32);
        bus.write_u16(hole + 0x10, 0x1234).expect("mmio write_u16");

        // High-RAM physical addresses >=4GiB should map back into RAM offsets starting at
        // PCIE_ECAM_BASE (without being routed to MMIO).
        assert_eq!(
            bus.ram_offset(crate::guest_phys::HIGH_RAM_BASE, 4),
            Some(crate::guest_phys::PCIE_ECAM_BASE)
        );

        let calls = mmio_calls();
        assert_eq!(calls.length(), 2);

        let c0 = calls.get(0);
        assert_eq!(call_prop_str(&c0, "kind"), "read");
        assert_eq!(call_prop_u32(&c0, "addr"), hole as u32);
        assert_eq!(call_prop_u32(&c0, "size"), 4);

        let c1 = calls.get(1);
        assert_eq!(call_prop_str(&c1, "kind"), "write");
        assert_eq!(call_prop_u32(&c1, "addr"), (hole + 0x10) as u32);
        assert_eq!(call_prop_u32(&c1, "size"), 2);
        assert_eq!(call_prop_u32(&c1, "value"), 0x1234);
    }

    #[wasm_bindgen_test]
    fn wasm_tiered_bus_write_log_overflow_invalidates_full_guest_phys_span() {
        // This is a regression test for the write-log overflow path. When the write log overflows
        // we conservatively invalidate the entire guest *physical* RAM region, which (for Q35) can
        // extend above 4GiB due to the high-RAM remap.
        //
        // The test does not require allocating multi-gigabyte guest RAM; we only exercise the
        // write-log bookkeeping and the computed invalidation bounds.
        installAeroTieredMmioTestShims();

        let mut guest = vec![0u8; 0x100];
        let guest_base = guest.as_mut_ptr() as u32;

        let guest_size = crate::guest_phys::PCIE_ECAM_BASE + 0x2000;
        let expected_end = crate::guest_phys::guest_ram_phys_end_exclusive(guest_size);
        assert!(
            expected_end > crate::guest_phys::HIGH_RAM_BASE,
            "sanity check: expected physical end should exceed 4GiB for this guest_size"
        );

        let mut bus = WasmBus::new(guest_base, guest_size);

        // Overflow the write log by recording many disjoint 1-byte writes.
        for i in 0..2048u64 {
            bus.log_write(i * 2, 1);
        }

        let mut drained: Vec<(u64, usize)> = Vec::new();
        bus.drain_write_log_to(|paddr, len| drained.push((paddr, len)));
        assert!(!drained.is_empty(), "expected at least one invalidation range");

        let mut max_end = 0u64;
        for (paddr, len) in drained {
            max_end = max_end.max(paddr.saturating_add(len as u64));
        }
        assert_eq!(
            max_end, expected_end,
            "write-log overflow invalidation must cover the full guest-physical span (including high RAM)"
        );
    }

    #[wasm_bindgen_test]
    fn wasm_tiered_bus_straddling_ram_and_mmio_reads_and_writes_bytewise() {
        installAeroTieredMmioTestShims();

        let mut guest = vec![0u8; 0x10];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u64;

        let mut bus = WasmBus::new(guest_base, guest_size);

        // Read across the end of RAM: the first byte comes from RAM and the remaining bytes are
        // sourced via MMIO shims.
        guest[0x0F] = 0xAA;
        let value = bus.read_u32(0x0F).expect("read_u32");
        assert_eq!(value, 0x12_11_10_AA);

        let calls = mmio_calls();
        assert_eq!(
            calls.length(),
            3,
            "expected 3 byte MMIO reads for a straddle"
        );

        for (idx, addr) in [(0, 0x10), (1, 0x11), (2, 0x12)] {
            let call = calls.get(idx);
            assert_eq!(call_prop_str(&call, "kind"), "read");
            assert_eq!(call_prop_u32(&call, "addr"), addr);
            assert_eq!(call_prop_u32(&call, "size"), 1);
        }

        // Reset call log.
        Reflect::set(
            &js_sys::global(),
            &JsValue::from_str("__aero_test_tiered_mmio_calls"),
            &Array::new(),
        )
        .expect("clear call log");

        // Write across the end of RAM: first byte should land in RAM, remainder should be routed
        // to MMIO byte writes.
        bus.write_u32(0x0F, 0x4433_2211).expect("write_u32");
        assert_eq!(guest[0x0F], 0x11);

        let calls = mmio_calls();
        assert_eq!(
            calls.length(),
            3,
            "expected 3 byte MMIO writes for a straddle"
        );

        for (idx, addr, byte) in [(0, 0x10, 0x22), (1, 0x11, 0x33), (2, 0x12, 0x44)] {
            let call = calls.get(idx);
            assert_eq!(call_prop_str(&call, "kind"), "write");
            assert_eq!(call_prop_u32(&call, "addr"), addr);
            assert_eq!(call_prop_u32(&call, "size"), 1);
            assert_eq!(call_prop_u32(&call, "value"), byte);
        }
    }

    #[wasm_bindgen_test]
    fn wasm_tiered_vm_instruction_count_respects_cpu_bitness() {
        // This is a regression test for `WasmTieredVm::install_tier1_block` /
        // `install_handle`: instruction_count is computed by re-decoding the guest code bytes.
        //
        // The Tier-1 minimal decoder treats 0x40..=0x4F as REX prefixes in 64-bit mode, but as
        // INC/DEC in 16/32-bit modes. Ensure we use the current CPU mode's bitness when decoding,
        // otherwise real-mode guest blocks will get incorrect instruction counts (which affects
        // retirement bookkeeping like TSC advancement).

        // The test should not require any I/O/MMIO shims since we only access in-RAM bytes.

        let mut guest = vec![0u8; 0x2000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        // Guest code at paddr 0x1000:
        //   0x40       inc ax        (in 16-bit mode)
        //   0xeb 0xfe  jmp short -2  (back to the jmp itself)
        //
        // In 64-bit decode mode, 0x40 is a REX prefix, so the same bytes decode as a single JMP
        // instruction (len=3). In 16-bit mode, they decode as two instructions.
        guest[0x1000..0x1003].copy_from_slice(&[0x40, 0xeb, 0xfe]);

        let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
        vm.reset_real_mode(0x1000);

        // Install via the direct helper (byte_len=3).
        vm.install_tier1_block(0x1000, 0, 0x1000, 3);
        let handle = vm
            .dispatcher
            .jit_mut()
            .prepare_block(0x1000)
            .expect("installed block must be present");
        assert_eq!(
            handle.meta.instruction_count, 2,
            "expected 16-bit decode to see INC+JMP (2 instructions)"
        );

        // Now install via `snapshot_meta` + `install_handle` and ensure the count is still correct.
        vm.dispatcher.jit_mut().invalidate_block(0x1000);
        let meta = vm.snapshot_meta(0x1000, 3).expect("snapshot_meta");
        vm.install_handle(0x1000, 1, meta).expect("install_handle");
        let handle2 = vm
            .dispatcher
            .jit_mut()
            .prepare_block(0x1000)
            .expect("installed block must be present after install_handle");
        assert_eq!(
            handle2.meta.instruction_count, 2,
            "expected instruction_count to match 16-bit decode after install_handle"
        );
    }

    #[wasm_bindgen_test]
    fn meta_from_js_rejects_length_mismatch() {
        let code_paddr = 0x1000u64;
        let meta = meta_obj(code_paddr, 1, &[]);
        let err = meta_from_js(meta).expect_err("expected error");
        let msg = js_err_message(err);
        assert!(
            msg.contains("length mismatch"),
            "unexpected error message: {msg}"
        );
    }

    #[wasm_bindgen_test]
    fn meta_from_js_rejects_non_contiguous_pages() {
        let code_paddr = 0x1000u64; // page 1
        let meta = meta_obj(
            code_paddr,
            0x2000, // spans pages 1..=2
            &[page_snapshot_obj(1, 0), page_snapshot_obj(3, 0)],
        );
        let err = meta_from_js(meta).expect_err("expected error");
        let msg = js_err_message(err);
        assert!(
            msg.contains("page mismatch"),
            "unexpected error message: {msg}"
        );
    }

    #[wasm_bindgen_test]
    fn wasm_tiered_vm_a20_enabled_ptr_controls_real_mode_wraparound() {
        installAeroTieredMmioTestShims();

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

        let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");

        // ---------------------------------------------------------------------
        // A20 enabled: reading 0x1_00000 should see 0x22.
        // ---------------------------------------------------------------------
        guest[0x0000_0200] = 0;
        vm.reset_real_mode(ENTRY_IP);
        let a20_ptr = vm.a20_enabled_ptr();
        assert_ne!(a20_ptr, 0, "a20_enabled_ptr must return a non-zero address");

        // Safety: `a20_ptr` is provided by `WasmTieredVm` and points into wasm linear memory.
        // Write 0/1 only (valid `bool` representation in Rust).
        unsafe {
            (a20_ptr as *mut u8).write(1);
        }

        let exit = vm.run_blocks(64).expect("run_blocks");
        assert_eq!(
            call_prop_u32(&exit, "kind"),
            crate::RunExitKind::Halted as u32
        );
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

        let exit = vm.run_blocks(64).expect("run_blocks");
        assert_eq!(
            call_prop_u32(&exit, "kind"),
            crate::RunExitKind::Halted as u32
        );
        assert_eq!(
            guest[0x0000_0200], 0x11,
            "A20 disabled: 0x1_00000 should alias to 0x0"
        );
    }
}
