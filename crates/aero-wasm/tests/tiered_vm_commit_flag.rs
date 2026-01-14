#![cfg(target_arch = "wasm32")]

use aero_cpu_core::state::{CpuState, MsrState};
use aero_wasm::{WasmTieredVm, jit_abi_constants};
use js_sys::{Object, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

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

fn read_tsc(vm: &WasmTieredVm) -> u64 {
    let a20_ptr = vm.a20_enabled_ptr();
    assert_ne!(a20_ptr, 0, "a20_enabled_ptr must be non-zero");

    let cpu_state_base = a20_ptr
        .checked_sub(core::mem::offset_of!(CpuState, a20_enabled) as u32)
        .expect("cpu_state_base underflow");
    let tsc_off = core::mem::offset_of!(CpuState, msr) + core::mem::offset_of!(MsrState, tsc);
    let tsc_ptr = cpu_state_base + (tsc_off as u32);

    // Safety: `a20_enabled_ptr` points into the live `CpuState` within `WasmTieredVm`.
    unsafe { core::ptr::read_unaligned(tsc_ptr as *const u64) }
}

fn run_one_jit_block(vm: &mut WasmTieredVm) {
    let res = vm.run_blocks(1).expect("run_blocks");
    let jit_blocks = Reflect::get(&res, &JsValue::from_str("jit_blocks"))
        .expect("jit_blocks")
        .as_f64()
        .expect("jit_blocks is number")
        .round() as u32;
    let interp_blocks = Reflect::get(&res, &JsValue::from_str("interp_blocks"))
        .expect("interp_blocks")
        .as_f64()
        .expect("interp_blocks is number")
        .round() as u32;
    assert_eq!(jit_blocks, 1, "expected one Tier-1 block execution");
    assert_eq!(
        interp_blocks, 0,
        "expected no Tier-0 execution in this slice"
    );
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_cleared_means_no_retirement_node() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    {
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

        // Tiny real-mode loop: INC AX; JMP -2 (2 instructions).
        guest[0x1000..0x1003].copy_from_slice(&[0x40, 0xeb, 0xfe]);
    }

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 42, 0x1000, 3);

    let commit_off = jit_commit_flag_offset();

    let tsc_before = read_tsc(&vm);
    {
        let closure = Closure::wrap(Box::new(
            move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                assert_eq!(table_index, 42);
                let flag_ptr = cpu_ptr + commit_off;

                let before = unsafe { core::ptr::read_unaligned(flag_ptr as *const u32) };
                assert_eq!(before, 1, "commit flag must be set before host hook");

                // Roll back architectural side effects.
                unsafe { core::ptr::write_unaligned(flag_ptr as *mut u32, 0) };
                -1
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", closure.as_ref());
        run_one_jit_block(&mut vm);
    }
    let tsc_after = read_tsc(&vm);

    assert_eq!(
        tsc_after, tsc_before,
        "clearing the commit flag must prevent instruction retirement / TSC advance"
    );
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_default_means_retirement_node() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    {
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

        // Tiny real-mode loop: INC AX; JMP -2 (2 instructions).
        guest[0x1000..0x1003].copy_from_slice(&[0x40, 0xeb, 0xfe]);
    }

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 42, 0x1000, 3);

    let commit_off = jit_commit_flag_offset();

    let tsc_before = read_tsc(&vm);
    {
        let closure = Closure::wrap(Box::new(
            move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                assert_eq!(table_index, 42);
                let flag_ptr = cpu_ptr + commit_off;

                let before = unsafe { core::ptr::read_unaligned(flag_ptr as *const u32) };
                assert_eq!(before, 1, "commit flag must be set before host hook");

                // Leave flag as 1 to indicate commit.
                -1
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", closure.as_ref());
        run_one_jit_block(&mut vm);
    }
    let tsc_after = read_tsc(&vm);

    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        2,
        "committed blocks must retire the decoded instruction_count"
    );
}
