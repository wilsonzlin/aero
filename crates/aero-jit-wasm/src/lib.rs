//! Tier-1 JIT compiler exposed as a standalone wasm-bindgen module.
//!
//! This module is meant to run in a browser worker and compile a single x86 basic block into a
//! standalone WASM module (returned as bytes).

#![cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-threaded"),
    feature(thread_local)
)]

#[cfg(target_arch = "wasm32")]
use core::cell::Cell;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use js_sys::{Object, Reflect, Uint8Array};

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
#[thread_local]
static TLS_DUMMY: u8 = 0;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn wasm_start() {
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    {
        // Ensure the TLS dummy is not optimized away.
        let _ = &TLS_DUMMY as *const u8;
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
        let Some(b) = self.code.get(off as usize) else {
            return self.read_oob_zero(addr);
        };
        *b
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
) -> Result<JsValue, JsValue> {
    let code_len = code_bytes.length() as usize;
    if code_len > MAX_CODE_BYTES {
        return Err(js_error(format!(
            "compile_tier1_block: code_bytes is too large ({} bytes > {} bytes max)",
            code_len, MAX_CODE_BYTES
        )));
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

    let limits = BlockLimits {
        max_insts: max_insts as usize,
        max_bytes: max_bytes as usize,
    };

    let mut options = Tier1WasmOptions::default();
    options.inline_tlb = inline_tlb;
    options.memory_shared = memory_shared;
    if memory_shared && options.memory_max_pages.is_none() {
        // Shared memories require a declared maximum. We pick the largest valid wasm32 page count
        // (4GiB / 64KiB) to keep the generated block modules compatible with a wide range of guest
        // memory configurations.
        options.memory_max_pages = Some(65_536);
    }

    let compilation =
        compile_tier1_block_with_options(&bus, entry_rip, limits, options).map_err(|e| {
            js_error(format!(
                "compile_tier1_block: Tier-1 compilation failed: {e}"
            ))
        })?;

    let block_end = entry_rip.checked_add(compilation.byte_len as u64).ok_or_else(|| {
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
