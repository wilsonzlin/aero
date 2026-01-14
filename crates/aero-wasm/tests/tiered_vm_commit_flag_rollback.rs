#![cfg(target_arch = "wasm32")]

use aero_cpu_core::exec::{ExecutedTier, StepOutcome};
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
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };
    guest.write_bytes(0x1000, &[0x40, 0xEB, 0xFE]);

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);
    vm.install_tier1_block(0x1000, 0, 0x1000, 3);

    let commit_flag_offset = jit_commit_flag_offset();

    // ---------------------------------------------------------------------
    // Commit path: leave the commit flag set to 1.
    // ---------------------------------------------------------------------
    let tsc_before = vm.cpu_tsc();
    let outcome = {
        let commit_call = Closure::wrap(Box::new(
            move |_table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                let commit_flag_ptr = cpu_ptr + commit_flag_offset;
                let before = unsafe {
                    core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                        commit_flag_ptr as usize,
                    ))
                };
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
                let before = unsafe {
                    core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                        commit_flag_ptr as usize,
                    ))
                };
                assert_eq!(
                    before, 1,
                    "commit flag should be set to 1 before host hook runs"
                );
                unsafe {
                    core::ptr::write_unaligned(
                        core::ptr::with_exposed_provenance_mut::<u32>(commit_flag_ptr as usize),
                        0,
                    );
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

    // ---------------------------------------------------------------------
    // Post-rollback commit path: the backend must reset the commit flag to 1 on
    // every call, even if the previous exit cleared it to 0.
    // ---------------------------------------------------------------------
    let tsc_before = vm.cpu_tsc();
    let outcome = {
        let commit_again = Closure::wrap(Box::new(
            move |_table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
                let commit_flag_ptr = cpu_ptr + commit_flag_offset;
                let before = unsafe {
                    core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                        commit_flag_ptr as usize,
                    ))
                };
                assert_eq!(
                    before, 1,
                    "commit flag should be reset to 1 before the host hook runs"
                );
                0x1000
            },
        ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
        let _guard = GlobalThisValueGuard::set("__aero_jit_call", commit_again.as_ref());
        vm.step_raw()
    };
    let tsc_after = vm.cpu_tsc();

    let retired = match outcome {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => instructions_retired,
        other => panic!("expected JIT block outcome, got {other:?}"),
    };
    assert_eq!(
        retired, committed_retired,
        "expected instruction_count to be stable across repeated committed exits"
    );
    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        committed_retired,
        "committed JIT blocks must advance TSC by instructions_retired"
    );
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_rollback_does_not_touch_interrupt_shadow_node() {
    installAeroTieredCommitFlagTestShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");

    const ENTRY0: u64 = 0x1000;
    const ENTRY1: u64 = 0x2000;
    const ENTRY2: u64 = 0x3000;

    vm.reset_real_mode(ENTRY0 as u32);
    vm.install_test_tier1_handle(ENTRY0, 0, 1, true);
    vm.install_test_tier1_handle(ENTRY1, 1, 1, true);
    vm.install_test_tier1_handle(ENTRY2, 2, 5, false);

    let commit_flag_offset = jit_commit_flag_offset();

    assert_eq!(
        vm.cpu_interrupt_inhibit(),
        0,
        "sanity check: interrupt shadow should start inactive"
    );

    let hook = Closure::wrap(Box::new(
        move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
            let commit_flag_ptr = cpu_ptr + commit_flag_offset;
            let before = unsafe {
                core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                    commit_flag_ptr as usize,
                ))
            };
            assert_eq!(
                before, 1,
                "commit flag should be set to 1 before host hook runs"
            );

            match table_index {
                // 1) Roll back a block that *would* create an interrupt shadow if committed.
                0 => {
                    unsafe {
                        core::ptr::write_unaligned(
                            core::ptr::with_exposed_provenance_mut::<u32>(commit_flag_ptr as usize),
                            0,
                        );
                    }
                    ENTRY1 as i64
                }
                // 2) Commit a block that creates an interrupt shadow.
                1 => ENTRY2 as i64,
                // 3) Roll back a block with a non-zero instruction_count; it must not age the shadow.
                2 => {
                    unsafe {
                        core::ptr::write_unaligned(
                            core::ptr::with_exposed_provenance_mut::<u32>(commit_flag_ptr as usize),
                            0,
                        );
                    }
                    ENTRY2 as i64
                }
                other => panic!("unexpected JIT table index {other}"),
            }
        },
    ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
    let _guard = GlobalThisValueGuard::set("__aero_jit_call", hook.as_ref());

    // Step 1: rollback should not apply inhibit_interrupts_after_block.
    let tsc_before = vm.cpu_tsc();
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, 0),
        other => panic!("expected JIT block outcome, got {other:?}"),
    }
    assert_eq!(vm.cpu_tsc(), tsc_before, "rollback must not advance TSC");
    assert_eq!(
        vm.cpu_interrupt_inhibit(),
        0,
        "rollback must not apply inhibit_interrupts_after_block"
    );

    // Step 2: committed block applies inhibit_interrupts_after_block (shadow becomes active).
    let tsc_before = vm.cpu_tsc();
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, 1),
        other => panic!("expected JIT block outcome, got {other:?}"),
    }
    assert_eq!(
        vm.cpu_tsc().wrapping_sub(tsc_before),
        1,
        "committed blocks must advance TSC by instruction_count"
    );
    assert_eq!(
        vm.cpu_interrupt_inhibit(),
        1,
        "committed blocks must apply inhibit_interrupts_after_block"
    );

    // Step 3: rollback must not age an existing interrupt shadow.
    let tsc_before = vm.cpu_tsc();
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, 0),
        other => panic!("expected JIT block outcome, got {other:?}"),
    }
    assert_eq!(vm.cpu_tsc(), tsc_before, "rollback must not advance TSC");
    assert_eq!(
        vm.cpu_interrupt_inhibit(),
        1,
        "rollback must not age interrupt shadow state"
    );
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_rollback_exit_forces_one_interpreter_step_node() {
    installAeroTieredCommitFlagTestShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };
    // Single-instruction branch (`JMP -2`) so Tier0Interpreter terminates the block quickly.
    guest.write_bytes(0x1000, &[0xEB, 0xFE]);

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    vm.reset_real_mode(0x1000);

    const ENTRY_RIP: u64 = 0x1000;
    vm.install_test_tier1_handle(ENTRY_RIP, 0, 5, false);

    let commit_flag_offset = jit_commit_flag_offset();

    // Backend always rolls back and forces an interpreter exit. It should be invoked exactly once:
    // the *next* dispatcher step must run Tier-0 even though a compiled handle is present.
    let mut called = false;
    let hook = Closure::wrap(Box::new(
        move |_table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
            assert!(
                !called,
                "__aero_jit_call should only be invoked once; exit_to_interpreter must force Tier-0"
            );
            called = true;

            let commit_flag_ptr = cpu_ptr + commit_flag_offset;
            let before = unsafe {
                core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                    commit_flag_ptr as usize,
                ))
            };
            assert_eq!(
                before, 1,
                "commit flag should be set to 1 before host hook runs"
            );
            unsafe {
                core::ptr::write_unaligned(
                    core::ptr::with_exposed_provenance_mut::<u32>(commit_flag_ptr as usize),
                    0,
                );
            }

            // Exit to interpreter (Tier-0).
            -1
        },
    ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
    let _guard = GlobalThisValueGuard::set("__aero_jit_call", hook.as_ref());

    // Step 1: JIT executes and rolls back.
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, 0),
        other => panic!("expected JIT block outcome, got {other:?}"),
    }

    // Step 2: forced interpreter step, must not re-enter JIT (hook would panic).
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Interpreter,
            instructions_retired,
            ..
        } => {
            assert!(
                instructions_retired > 0,
                "sanity check: Tier-0 should execute at least one instruction"
            );
        }
        StepOutcome::InterruptDelivered => panic!("unexpected interrupt delivery"),
        other => panic!("expected Tier-0 block outcome, got {other:?}"),
    }
}

#[wasm_bindgen_test]
fn tiered_vm_commit_flag_committed_exit_to_interpreter_retires_and_forces_tier0_node() {
    installAeroTieredCommitFlagTestShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x2000);
    let guest = common::GuestRegion {
        base: guest_base,
        size: guest_size,
    };
    // Single-instruction branch (`JMP -2`) so Tier0Interpreter terminates the block quickly.
    guest.write_bytes(0x1000, &[0xEB, 0xFE]);

    let mut vm = WasmTieredVm::new(guest_base, guest_size).expect("new WasmTieredVm");
    const ENTRY_RIP: u64 = 0x1000;
    const INSTS: u32 = 5;
    vm.reset_real_mode(ENTRY_RIP as u32);
    vm.install_test_tier1_handle(ENTRY_RIP, 0, INSTS, false);

    let commit_flag_offset = jit_commit_flag_offset();

    // Backend commits but returns the interpreter-exit sentinel. This should retire instructions and
    // advance TSC, then force exactly one Tier-0 step.
    let mut called = false;
    let hook = Closure::wrap(Box::new(
        move |table_index: u32, cpu_ptr: u32, _jit_ctx_ptr: u32| -> i64 {
            assert!(
                !called,
                "__aero_jit_call should only be invoked once; exit_to_interpreter must force Tier-0"
            );
            called = true;
            assert_eq!(table_index, 0);

            let commit_flag_ptr = cpu_ptr + commit_flag_offset;
            let before = unsafe {
                core::ptr::read_unaligned(core::ptr::with_exposed_provenance::<u32>(
                    commit_flag_ptr as usize,
                ))
            };
            assert_eq!(
                before, 1,
                "commit flag should be set to 1 before host hook runs"
            );

            // Leave the flag untouched to indicate a committed block, but exit to interpreter.
            -1
        },
    ) as Box<dyn FnMut(u32, u32, u32) -> i64>);
    let _guard = GlobalThisValueGuard::set("__aero_jit_call", hook.as_ref());

    let tsc_before = vm.cpu_tsc();
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Jit,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, u64::from(INSTS)),
        other => panic!("expected JIT block outcome, got {other:?}"),
    }
    let tsc_after = vm.cpu_tsc();
    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        u64::from(INSTS),
        "committed JIT blocks must advance TSC by instruction_count even when exiting to interpreter"
    );

    let tsc_before = vm.cpu_tsc();
    match vm.step_raw() {
        StepOutcome::Block {
            tier: ExecutedTier::Interpreter,
            instructions_retired,
            ..
        } => assert_eq!(instructions_retired, 1),
        StepOutcome::InterruptDelivered => panic!("unexpected interrupt delivery"),
        other => panic!("expected Tier-0 block outcome, got {other:?}"),
    }
    let tsc_after = vm.cpu_tsc();
    assert_eq!(
        tsc_after.wrapping_sub(tsc_before),
        1,
        "expected Tier-0 branch instruction to retire as one instruction"
    );
}
