//! Tier-0 + Tier-1 tiered execution VM loop for the browser worker runtime.
//!
//! This module wires up:
//! - Tier-0 interpreter blocks (`aero_cpu_core::exec::Tier0Interpreter`)
//! - Tier-1 JIT cache + tiering logic (`aero_cpu_core::jit::runtime::JitRuntime`)
//! - Tiered dispatcher (`aero_cpu_core::exec::ExecDispatcher`)
//!
//! The Tier-1 backend is intentionally minimal: compiled blocks are executed by
//! calling out to JS via `globalThis.__aero_jit_call(table_index, cpu_ptr, jit_ctx_ptr)`.
//!
//! Compilation happens out-of-band in a separate JIT worker; this VM only exports a
//! compile-request drain and an install hook for compiled blocks.

#![cfg(target_arch = "wasm32")]

use std::collections::{HashSet, VecDeque};
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use js_sys::{Array, BigInt, Object, Reflect};

use aero_cpu_core::exec::{ExecDispatcher, ExecutedTier, StepOutcome, Tier0Interpreter, Vcpu};
use aero_cpu_core::jit::cache::CompiledBlockHandle;
use aero_cpu_core::jit::runtime::{
    CompileRequestSink, JitBackend, JitBlockExit, JitConfig, JitRuntime,
};
use aero_cpu_core::state::{
    CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF, CPU_STATE_ALIGN, CPU_STATE_SIZE, CpuMode, Segment,
};
use aero_cpu_core::{CpuBus, CpuCore, Exception};

use aero_jit_x86::jit_ctx::{JitContext, TIER2_CTX_OFFSET, TIER2_CTX_SIZE};
use aero_jit_x86::{BlockLimits, Tier1Bus, discover_block};

use crate::RunExitKind;

fn js_error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_io_port_read)]
    fn js_io_port_read(port: u32, size: u32) -> u32;

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_io_port_write)]
    fn js_io_port_write(port: u32, size: u32, value: u32);

    #[wasm_bindgen(js_namespace = globalThis, js_name = __aero_jit_call)]
    fn js_jit_call(table_index: u32, cpu_ptr: u32, jit_ctx_ptr: u32) -> i64;
}

const JIT_EXIT_SENTINEL_I64: i64 = -1;
const DEFAULT_TLB_SALT: u64 = 0x1234_5678_9abc_def0;

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

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        let _ = self.ptr_mut(vaddr, len)?;
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

impl Tier1Bus for WasmBus {
    fn read_u8(&self, addr: u64) -> u8 {
        self.read_scalar::<1>(addr).map(|b| b[0]).unwrap_or(0)
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        let _ = self.write_scalar::<1>(addr, [value]);
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
    fn read_u64_at(&self, addr: u32) -> u64 {
        // Safety: `addr` is a wasm linear-memory pointer into the dedicated JIT ABI buffer.
        unsafe { core::ptr::read_unaligned(addr as *const u64) }
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

        let ret = js_jit_call(table_index, self.cpu_ptr, self.jit_ctx_ptr);

        self.sync_cpu_from_abi(&mut cpu.cpu.state);

        let exit_to_interpreter = ret == JIT_EXIT_SENTINEL_I64;
        let next_rip = if exit_to_interpreter {
            cpu.cpu.state.rip()
        } else {
            ret as u64
        };

        JitBlockExit {
            next_rip,
            exit_to_interpreter,
            committed: true,
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

    total_interp_blocks: u64,
    total_jit_blocks: u64,
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

        let cpu = CpuCore::new(CpuMode::Real);
        let vcpu = Vcpu::new(cpu, bus);

        let compile_queue = CompileQueue::new();

        // JIT ABI buffer: CpuState + jit_ctx + tier2_ctx.
        let jit_abi_len = (TIER2_CTX_OFFSET + TIER2_CTX_SIZE) as usize;
        let mut jit_abi = JitAbiBuffer::new(jit_abi_len, CPU_STATE_ALIGN);
        jit_abi.init_jit_ctx_header(guest_base as u64, DEFAULT_TLB_SALT);

        let cpu_ptr = jit_abi.cpu_ptr();
        let jit_ctx_ptr = jit_abi.jit_ctx_ptr();

        let backend = WasmJitBackend::new(cpu_ptr, jit_ctx_ptr, guest_base, DEFAULT_TLB_SALT);

        let jit = JitRuntime::new(
            JitConfig {
                enabled: true,
                // Low threshold for browser bring-up: we want hot blocks to quickly transition to
                // Tier-1 so the JS wiring can observe JIT execution.
                hot_threshold: 3,
                cache_max_blocks: 1024,
                cache_max_bytes: 0,
            },
            backend,
            compile_queue.clone(),
        );

        let interp = Tier0Interpreter::new(10_000);
        let dispatcher = ExecDispatcher::new(interp, jit);

        Ok(Self {
            guest_base,
            guest_size: guest_size_u64,
            vcpu,
            dispatcher,
            compile_queue,
            jit_abi,
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

        // Reset JIT ABI context (especially the inline-TLB fast-path state).
        self.jit_abi.clear_tlb();
        self.jit_abi
            .init_jit_ctx_header(self.guest_base as u64, DEFAULT_TLB_SALT);
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
            let block = discover_block(&self.vcpu.bus, entry_rip, limits);
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
