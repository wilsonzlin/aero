#![cfg(target_arch = "wasm32")]

use aero_wasm::WasmVm;
use aero_snapshot::CpuInternalState;
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen(inline_js = r#"
export function installAeroIoShims() {
  if (typeof globalThis.__aero_io_port_read !== "function") {
    globalThis.__aero_io_port_read = function (_port, _size) { return 0; };
  }
  if (typeof globalThis.__aero_io_port_write !== "function") {
    globalThis.__aero_io_port_write = function (_port, _size, _value) { };
  }
}
"#)]
extern "C" {
    fn installAeroIoShims();
}

fn snapshot_bytes(vm: &WasmVm) -> (Vec<u8>, Vec<u8>) {
    let val = vm.save_state_v2().expect("save_state_v2 ok");
    let obj: js_sys::Object = val.dyn_into().expect("snapshot object");

    let cpu_val = Reflect::get(&obj, &JsValue::from_str("cpu")).expect("cpu property");
    let mmu_val = Reflect::get(&obj, &JsValue::from_str("mmu")).expect("mmu property");

    let cpu = cpu_val.dyn_into::<Uint8Array>().expect("cpu Uint8Array");
    let mmu = mmu_val.dyn_into::<Uint8Array>().expect("mmu Uint8Array");

    (cpu.to_vec(), mmu.to_vec())
}

fn snapshot_cpu_internal_bytes(vm: &WasmVm) -> Vec<u8> {
    let val = vm.save_state_v2().expect("save_state_v2 ok");
    let obj: js_sys::Object = val.dyn_into().expect("snapshot object");

    let internal_val =
        Reflect::get(&obj, &JsValue::from_str("cpu_internal")).expect("cpu_internal property");
    let internal = internal_val
        .dyn_into::<Uint8Array>()
        .expect("cpu_internal Uint8Array");

    internal.to_vec()
}

#[wasm_bindgen_test]
fn save_state_v2_is_deterministic_without_execution() {
    installAeroIoShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x1000);
    // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting at
    // `guest_base` and the region is private to this test.
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    // Simple real-mode program: `mov ax, 0x1234; nop`.
    guest[0..4].copy_from_slice(&[0xB8, 0x34, 0x12, 0x90]);

    let mut vm = WasmVm::new(guest_base, guest_size).expect("new WasmVm");
    vm.reset_real_mode(0);

    let (cpu_a, mmu_a) = snapshot_bytes(&vm);
    let (cpu_b, mmu_b) = snapshot_bytes(&vm);

    assert_eq!(cpu_a, cpu_b, "CPU snapshot bytes should be deterministic");
    assert_eq!(mmu_a, mmu_b, "MMU snapshot bytes should be deterministic");
}

#[wasm_bindgen_test]
fn save_load_state_v2_roundtrips() {
    installAeroIoShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x1000);
    // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting at
    // `guest_base` and the region is private to this test.
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    // Simple real-mode program: `mov ax, 0x1234; nop`.
    guest[0..4].copy_from_slice(&[0xB8, 0x34, 0x12, 0x90]);

    let mut vm = WasmVm::new(guest_base, guest_size).expect("new WasmVm");
    vm.reset_real_mode(0);

    // Execute at least one slice so state changes (RIP/TSC/etc).
    let exit = vm.run_slice(1);
    assert_eq!(exit.executed(), 1);

    let (cpu_before, mmu_before) = snapshot_bytes(&vm);

    let mut restored = WasmVm::new(guest_base, guest_size).expect("new WasmVm (restored)");
    restored
        .load_state_v2(&cpu_before, &mmu_before)
        .expect("load_state_v2 ok");

    let (cpu_after, mmu_after) = snapshot_bytes(&restored);

    assert_eq!(
        cpu_before, cpu_after,
        "CPU snapshot mismatch after roundtrip"
    );
    assert_eq!(
        mmu_before, mmu_after,
        "MMU snapshot mismatch after roundtrip"
    );
}

#[wasm_bindgen_test]
fn save_state_v2_roundtrips_interrupt_shadow_via_cpu_internal() {
    installAeroIoShims();

    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x1000);
    // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting at
    // `guest_base` and the region is private to this test.
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    // Real-mode program:
    //   sti
    //   nop
    //   hlt
    guest[0..3].copy_from_slice(&[0xFB, 0x90, 0xF4]);

    let mut vm = WasmVm::new(guest_base, guest_size).expect("new WasmVm");
    vm.reset_real_mode(0);

    // Execute STI so the interrupt shadow is set for exactly one subsequent instruction.
    let exit = vm.run_slice(1);
    assert_eq!(exit.executed(), 1);

    let (cpu_before, mmu_before) = snapshot_bytes(&vm);
    let internal_before = snapshot_cpu_internal_bytes(&vm);

    let decoded =
        CpuInternalState::decode(&mut std::io::Cursor::new(&internal_before)).expect("decode ok");
    assert!(
        decoded.interrupt_inhibit > 0,
        "expected STI interrupt shadow to be active after STI executes"
    );

    let mut restored = WasmVm::new(guest_base, guest_size).expect("new WasmVm (restored)");
    restored
        .load_state_v2(&cpu_before, &mmu_before)
        .expect("load_state_v2 ok");
    restored
        .load_cpu_internal_state_v2(&internal_before)
        .expect("load_cpu_internal_state_v2 ok");

    let internal_after = snapshot_cpu_internal_bytes(&restored);
    assert_eq!(
        internal_after, internal_before,
        "CPU_INTERNAL bytes must roundtrip identically"
    );

    // Execute the next instruction (`nop`); the interrupt shadow should age out to zero.
    let exit = restored.run_slice(1);
    assert_eq!(exit.executed(), 1);
    let internal_post = snapshot_cpu_internal_bytes(&restored);
    let decoded_post =
        CpuInternalState::decode(&mut std::io::Cursor::new(&internal_post)).expect("decode ok");
    assert_eq!(
        decoded_post.interrupt_inhibit, 0,
        "interrupt shadow must decrement to 0 after one retired instruction"
    );
}
