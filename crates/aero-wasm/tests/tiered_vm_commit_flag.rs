#![cfg(target_arch = "wasm32")]

use aero_cpu_core::exec::{ExecutedTier, StepOutcome};
use aero_wasm::{WasmTieredVm, jit_abi_constants};
use js_sys::{Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

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

struct GlobalThisValueGuard {
    global: Object,
    key: JsValue,
    prev: JsValue,
    had_key: bool,
}

impl GlobalThisValueGuard {
    fn set(key: &str, value: &JsValue) -> Self {
        let global = js_sys::global();
        let key_js = JsValue::from_str(key);
        let had_key = Reflect::has(&global, &key_js).expect("check global value");
        let prev = if had_key {
            Reflect::get(&global, &key_js).expect("get global value")
        } else {
            JsValue::UNDEFINED
        };
        Reflect::set(&global, &key_js, value).expect("set global value");
        Self {
            global,
            key: key_js,
            prev,
            had_key,
        }
    }
}

impl Drop for GlobalThisValueGuard {
    fn drop(&mut self) {
        if self.had_key {
            let _ = Reflect::set(&self.global, &self.key, &self.prev);
        } else {
            let _ = Reflect::delete_property(&self.global, &self.key);
        }
    }
}

fn jit_commit_flag_offset() -> u32 {
    let obj = jit_abi_constants();
    Reflect::get(&obj, &JsValue::from_str("commit_flag_offset"))
        .expect("commit_flag_offset exists")
        .as_f64()
        .expect("commit_flag_offset must be a number")
        .round() as u32
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_cleared_means_no_retirement_node() {
    installAeroTieredCommitFlagTestShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };
    // Tiny real-mode loop: INC AX; JMP -2 (2 instructions).
    guest.write_bytes(0x1000, &[0x40, 0xeb, 0xfe]);

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 42, 0x1000, 3);

    let commit_off = jit_commit_flag_offset();

    let tsc_before = vm.cpu_tsc();
    {
        let closure = Closure::wrap(Box::new(
            move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                assert_eq!(table_index, 42);
                let flag_ptr = cpu_ptr + commit_off;

                let before = unsafe {
                    core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                        flag_ptr as usize,
                    ))
                };
                assert_eq!(before, 1, "commit flag must be set before host hook");

                // Roll back architectural side effects.
                unsafe {
                    core::ptr::write_unaligned(
                        core::ptr::with_exposed_provenance_mut::<u32>(flag_ptr as usize),
                        0,
                    );
                }
                -1
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", closure.as_ref());
        let outcome = vm.step_raw();
        match outcome {
            StepOutcome::Block {
                tier: ExecutedTier::Jit,
                instructions_retired,
                ..
            } => assert_eq!(instructions_retired, 0),
            other => panic!("expected JIT block outcome, got {other:?}"),
        }
    }
    let tsc_after = vm.cpu_tsc();

    assert_eq!(
        tsc_after, tsc_before,
        "clearing the commit flag must prevent instruction retirement / TSC advance"
    );
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_default_means_retirement_node() {
    installAeroTieredCommitFlagTestShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };
    // Tiny real-mode loop: INC AX; JMP -2 (2 instructions).
    guest.write_bytes(0x1000, &[0x40, 0xeb, 0xfe]);

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 42, 0x1000, 3);

    let commit_off = jit_commit_flag_offset();

    let tsc_before = vm.cpu_tsc();
    {
        let closure = Closure::wrap(Box::new(
            move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                assert_eq!(table_index, 42);
                let flag_ptr = cpu_ptr + commit_off;

                let before = unsafe {
                    core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                        flag_ptr as usize,
                    ))
                };
                assert_eq!(before, 1, "commit flag must be set before host hook");

                // Leave flag as 1 to indicate commit.
                -1
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", closure.as_ref());
        let outcome = vm.step_raw();
        match outcome {
            StepOutcome::Block {
                tier: ExecutedTier::Jit,
                instructions_retired,
                ..
            } => assert_eq!(instructions_retired, 2),
            other => panic!("expected JIT block outcome, got {other:?}"),
        }
    }
    let tsc_after = vm.cpu_tsc();

    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        2,
        "committed blocks must retire the decoded instruction_count"
    );
}
