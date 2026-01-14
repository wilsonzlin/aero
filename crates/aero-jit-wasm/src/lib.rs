//! Tier-1 JIT compiler exposed as a standalone wasm-bindgen module.
//!
//! This module is meant to run in a browser worker and compile a single x86 basic block into a
//! standalone WASM module (returned as bytes).

// Note: This crate is built for both the single-threaded WASM variant (pinned stable toolchain)
// and the threaded/shared-memory variant (pinned nightly toolchain for `-Z build-std`). Keep the
// Rust code free of unstable language features so both builds remain viable.

#[cfg(target_arch = "wasm32")]
use core::cell::Cell;

#[cfg(target_arch = "wasm32")]
use js_sys::{Object, Reflect, Uint8Array};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use aero_jit_x86::{
    Tier1Bus,
    compiler::tier1::compile_tier1_block_with_options,
    tier1::{BlockLimits, Tier1WasmOptions},
};

// wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
// `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
// by the linker when there is at least one TLS variable. We keep a tiny TLS slot
// behind a cargo feature enabled only for the threaded build.
#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
thread_local! {
    // wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
    // `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
    // by the linker when there is at least one TLS variable.
    //
    // We use `thread_local!` instead of the unstable `#[thread_local]` attribute so
    // the threaded WASM build can compile on stable Rust.
    static TLS_DUMMY: u8 = const { 0 };
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn wasm_start() {
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    {
        // Ensure the TLS dummy is not optimized away.
        TLS_DUMMY.with(|v| core::hint::black_box(*v));
    }
}

#[cfg(target_arch = "wasm32")]
fn js_error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}

/// Hard cap to avoid accidental OOMs from absurd inputs.
#[cfg(target_arch = "wasm32")]
const MAX_CODE_BYTES: usize = 1024 * 1024;

#[cfg(target_arch = "wasm32")]
struct Tier1SliceBus<'a> {
    entry_rip: u64,
    code: &'a [u8],
    first_oob_addr: Cell<Option<u64>>,
}

#[cfg(target_arch = "wasm32")]
impl<'a> Tier1SliceBus<'a> {
    #[inline]
    fn record_oob(&self, addr: u64) {
        if self.first_oob_addr.get().is_none() {
            self.first_oob_addr.set(Some(addr));
        }
    }

    #[inline]
    fn read_oob_zero(&self, addr: u64) -> u8 {
        self.record_oob(addr);
        0
    }
}

#[cfg(target_arch = "wasm32")]
impl<'a> Tier1Bus for Tier1SliceBus<'a> {
    #[inline]
    fn read_u8(&self, addr: u64) -> u8 {
        let Some(off) = addr.checked_sub(self.entry_rip) else {
            return self.read_oob_zero(addr);
        };
        let Ok(off) = usize::try_from(off) else {
            return self.read_oob_zero(addr);
        };
        let Some(b) = self.code.get(off) else {
            return self.read_oob_zero(addr);
        };
        *b
    }

    #[inline]
    fn fetch(&self, addr: u64, len: usize) -> Vec<u8> {
        // Tier-1 decoding uses a fixed 15-byte fetch window per instruction. Override the default
        // `Tier1Bus::fetch` implementation to avoid calling `read_u8` in a tight loop.
        let Some(off) = addr.checked_sub(self.entry_rip) else {
            self.record_oob(addr);
            return vec![0u8; len];
        };
        let Ok(off) = usize::try_from(off) else {
            self.record_oob(addr);
            return vec![0u8; len];
        };
        if off >= self.code.len() {
            self.record_oob(addr);
            return vec![0u8; len];
        }

        let available = self.code.len() - off;
        let copy_len = available.min(len);
        let mut out = vec![0u8; len];
        out[..copy_len].copy_from_slice(&self.code[off..off + copy_len]);
        if copy_len < len {
            // Record the first address beyond the provided slice.
            self.record_oob(self.entry_rip + self.code.len() as u64);
        }
        out
    }

    #[inline]
    fn write_u8(&mut self, addr: u64, _value: u8) {
        // Tier-1 compilation should not write via the bus. If it does, treat it as out-of-bounds
        // (deterministic error) since we only have an immutable code slice.
        self.record_oob(addr);
    }
}

/// Compile a single Tier-1 basic block starting at `entry_rip`.
///
/// `bitness` controls how x86 instructions are decoded:
/// - `16`: 16-bit decode (real mode / 16-bit protected mode)
/// - `32`: 32-bit decode (legacy/protected mode)
/// - `64`: 64-bit decode (long mode)
///
/// For backwards compatibility with older JS call sites, passing `0` (or omitting the argument)
/// defaults to `64`.
///
/// Returns a JS object:
/// - `wasm_bytes: Uint8Array`
/// - `code_byte_len: u32`
/// - `exit_to_interpreter: bool`
///
/// On failure, throws a JS `Error`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn compile_tier1_block(
    entry_rip: u64,
    code_bytes: Uint8Array,
    max_insts: u32,
    max_bytes: u32,
    inline_tlb: bool,
    memory_shared: bool,
    bitness: u32,
) -> Result<JsValue, JsValue> {
    let code_len = code_bytes.length() as usize;
    if code_len == 0 {
        return Err(js_error("compile_tier1_block: code_bytes is empty"));
    }
    if code_len > MAX_CODE_BYTES {
        return Err(js_error(format!(
            "compile_tier1_block: code_bytes is too large ({} bytes > {} bytes max)",
            code_len, MAX_CODE_BYTES
        )));
    }
    if max_insts == 0 {
        return Err(js_error("compile_tier1_block: max_insts must be > 0"));
    }
    if max_bytes == 0 {
        return Err(js_error("compile_tier1_block: max_bytes must be > 0"));
    }
    let code_end = entry_rip.checked_add(code_len as u64).ok_or_else(|| {
        js_error("compile_tier1_block: entry_rip + code_bytes.len() overflowed u64")
    })?;

    let code_vec = code_bytes.to_vec();
    let bus = Tier1SliceBus {
        entry_rip,
        code: &code_vec,
        first_oob_addr: Cell::new(None),
    };

    // Clamp limits so we cannot decode past the provided slice even if the caller passes absurd
    // values (e.g. negative numbers in JS which become huge u32s).
    //
    // Note: decoding may still require reading up to 15 bytes past the end of the slice to decode
    // the final instruction window; those reads are handled by `Tier1SliceBus` and are only
    // considered an error if the decoded instruction *length* exceeds the provided coverage.
    let limits = BlockLimits {
        max_insts: max_insts.min(code_len as u32) as usize,
        max_bytes: max_bytes.min(code_len as u32) as usize,
    };

    // Browser tiered execution relies on the host-side runtime to observe guest stores (for
    // MMIO classification + self-modifying code invalidation via `jit.on_guest_write(..)`, and
    // also requires host-side rollback on Tier-1 runtime exits (MMIO/jit_exit/page_fault).
    //
    // Inline-TLB stores emit direct `i64.store*` operations into guest RAM, bypassing those
    // hooks. When inline-TLB is enabled, disable the store fast-path for now so stores go through
    // the imported `env.mem_write_*` helpers.
    let options = Tier1WasmOptions {
        inline_tlb,
        inline_tlb_stores: !inline_tlb,
        memory_shared,
        ..Default::default()
    };

    // wasm-bindgen will coerce a missing JS argument (`undefined`) to `0` for `u32`. Treat that as
    // a backwards-compatible default to 64-bit decoding.
    let bitness = if bitness == 0 { 64 } else { bitness };
    if !matches!(bitness, 16 | 32 | 64) {
        return Err(js_error(format!(
            "compile_tier1_block: unsupported bitness {bitness} (expected 16, 32, or 64)"
        )));
    }

    let compilation = compile_tier1_block_with_options(&bus, entry_rip, bitness, limits, options)
        .map_err(|e| {
        js_error(format!(
            "compile_tier1_block: Tier-1 compilation failed: {e}"
        ))
    })?;
    let block_end = entry_rip
        .checked_add(compilation.byte_len as u64)
        .ok_or_else(|| {
            js_error("compile_tier1_block: entry_rip + compiled block length overflowed u64")
        })?;
    if block_end > code_end {
        // Note: Tier-1's decoder always reads a 15-byte window per instruction, so out-of-bounds
        // reads can occur even when the *decoded* instruction stream stays within the provided
        // slice. We only error when the decoded block's byte length actually exceeds the input
        // coverage.
        let addr = bus.first_oob_addr.get().unwrap_or(code_end);
        return Err(js_error(format!(
            "compile_tier1_block: decoded block ends at {block_end:#x}, but provided code_bytes covers [{entry_rip:#x}, {code_end:#x}) (first out-of-bounds read at {addr:#x})"
        )));
    }

    let result = Object::new();
    Reflect::set(
        &result,
        &JsValue::from_str("wasm_bytes"),
        &Uint8Array::from(compilation.wasm_bytes.as_slice()).into(),
    )?;
    Reflect::set(
        &result,
        &JsValue::from_str("code_byte_len"),
        &JsValue::from(compilation.byte_len),
    )?;
    Reflect::set(
        &result,
        &JsValue::from_str("exit_to_interpreter"),
        &JsValue::from(compilation.exit_to_interpreter),
    )?;

    Ok(result.into())
}
