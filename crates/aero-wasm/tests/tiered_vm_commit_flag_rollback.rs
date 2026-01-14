#![cfg(target_arch = "wasm32")]

use aero_cpu_core::exec::{ExecutedTier, StepOutcome};
use aero_jit_x86::jit_ctx::{TIER2_CTX_OFFSET, TIER2_CTX_SIZE};
use aero_wasm::WasmTieredVm;
use js_sys::{Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

struct GlobalThisValueGuard {
    global: Object,
    key: JsValue,
    prev: JsValue,
}

impl GlobalThisValueGuard {
    fn set(key: &str, value: &JsValue) -> Self {
        let global = js_sys::global();
        let key_js = JsValue::from_str(key);
        let prev = Reflect::get(&global, &key_js).expect("get global value");
        Reflect::set(&global, &key_js, value).expect("set global value");
        Self {
            global,
            key: key_js,
            prev,
        }
    }
}

impl Drop for GlobalThisValueGuard {
    fn drop(&mut self) {
        let _ = Reflect::set(&self.global, &self.key, &self.prev);
    }
}

#[wasm_bindgen(inline_js = r#"
export function installAeroTieredCommitFlagTestShims() {
  if (typeof globalThis.__aero_mmio_read !== "function") {
    globalThis.__aero_mmio_read = function (addr, _size) { return Number(addr) >>> 0; };
  }
  if (typeof globalThis.__aero_mmio_write !== "function") {
    globalThis.__aero_mmio_write = function (_addr, _size, _value) { };
  }
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
    fn installAeroTieredCommitFlagTestShims();
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_rollback_retires_zero_instructions_node() {
    installAeroTieredCommitFlagTestShims();

    // Minimal 16-bit real-mode loop: INC AX; JMP -2.
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    {
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };
        guest[0x1000..0x1003].copy_from_slice(&[0x40, 0xEB, 0xFE]);
    }

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 0, 0x1000, 3);

    // Commit flag lives immediately after the Tier-2 context region.
    let commit_flag_offset = TIER2_CTX_OFFSET + TIER2_CTX_SIZE;

    // ---------------------------------------------------------------------
    // Commit path: leave the commit flag set to 1.
    // ---------------------------------------------------------------------
    let tsc_before = vm.cpu_tsc();
    let outcome = {
        let commit_call = Closure::wrap(Box::new(
            move |_table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                let commit_flag_ptr = cpu_ptr + commit_flag_offset;
                let before = unsafe { core::ptr::read_unaligned(commit_flag_ptr as *const u32) };
                assert_eq!(
                    before, 1,
                    "commit flag should be set to 1 before host hook runs"
                );
                0x1000
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", commit_call.as_ref());
        vm.step_raw()
    };
    let tsc_after = vm.cpu_tsc();

    let committed_retired = match outcome {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => {
            assert!(
                instructions_retired > 0,
                "sanity check: committed JIT block must retire at least one instruction"
            );
            instructions_retired
        }
        other => panic!("expected JIT block outcome, got {other:?}"),
    };
    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        committed_retired,
        "committed JIT blocks must advance TSC by instructions_retired"
    );

    // ---------------------------------------------------------------------
    // Rollback path: clear the commit flag to 0.
    // ---------------------------------------------------------------------
    let tsc_before = vm.cpu_tsc();
    let outcome = {
        let rollback_call = Closure::wrap(Box::new(
            move |_table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                let commit_flag_ptr = cpu_ptr + commit_flag_offset;
                let before = unsafe { core::ptr::read_unaligned(commit_flag_ptr as *const u32) };
                assert_eq!(
                    before, 1,
                    "commit flag should be set to 1 before host hook runs"
                );
                unsafe {
                    core::ptr::write_unaligned(commit_flag_ptr as *mut u32, 0);
                }
                0x1000
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", rollback_call.as_ref());
        vm.step_raw()
    };
    let tsc_after = vm.cpu_tsc();

    match outcome {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => {
            assert_eq!(
                instructions_retired, 0,
                "rollback exits must report zero retired instructions"
            );
        }
        other => panic!("expected JIT block outcome, got {other:?}"),
    }
    assert_eq!(tsc_after, tsc_before, "rollback exits must not advance TSC");
}
