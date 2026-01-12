#![cfg(target_arch = "wasm32")]

use aero_wasm::WorkerVmSnapshot;
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn pattern_byte(i: usize) -> u8 {
    ((i as u32).wrapping_mul(31).wrapping_add(7) & 0xFF) as u8
}

#[wasm_bindgen_test]
fn worker_vm_snapshot_is_deterministic() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(256 * 1024);

    unsafe {
        let mem = core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize);
        for (i, b) in mem.iter_mut().enumerate() {
            *b = pattern_byte(i);
        }
    }

    let mut vm = WorkerVmSnapshot::new(guest_base, guest_size).expect("new WorkerVmSnapshot");

    let cpu = aero_snapshot::CpuState::default();
    let mmu = aero_snapshot::MmuState::default();
    let mut cpu_bytes = Vec::new();
    cpu.encode_v2(&mut cpu_bytes).unwrap();
    let mut mmu_bytes = Vec::new();
    mmu.encode_v2(&mut mmu_bytes).unwrap();

    vm.set_cpu_state_v2(&cpu_bytes, &mmu_bytes)
        .expect("set_cpu_state_v2");
    vm.add_device_state(0x1234, 1, 0, &[0xAA, 0xBB, 0xCC])
        .expect("add_device_state");

    let a = vm.snapshot_full().expect("snapshot_full #1");
    let b = vm.snapshot_full().expect("snapshot_full #2");
    assert_eq!(a, b, "snapshot bytes must be deterministic");
}

#[wasm_bindgen_test]
fn worker_vm_snapshot_roundtrip_restores_ram_and_devices() {
    let (src_base, src_size) = common::alloc_guest_region_bytes(128 * 1024);

    unsafe {
        let mem = core::slice::from_raw_parts_mut(src_base as *mut u8, src_size as usize);
        for (i, b) in mem.iter_mut().enumerate() {
            *b = pattern_byte(i);
        }
    }

    let mut vm = WorkerVmSnapshot::new(src_base, src_size).expect("new WorkerVmSnapshot");

    let cpu = aero_snapshot::CpuState::default();
    let mmu = aero_snapshot::MmuState::default();
    let mut cpu_bytes = Vec::new();
    cpu.encode_v2(&mut cpu_bytes).unwrap();
    let mut mmu_bytes = Vec::new();
    mmu.encode_v2(&mut mmu_bytes).unwrap();

    vm.set_cpu_state_v2(&cpu_bytes, &mmu_bytes)
        .expect("set_cpu_state_v2");

    let device_data: Vec<u8> = (0..1024u32).map(|i| (i & 0xFF) as u8).collect();
    vm.add_device_state(0xDEAD_BEEF, 2, 3, &device_data)
        .expect("add_device_state");

    let snapshot = vm.snapshot_full().expect("snapshot_full");

    let (dst_base, dst_size) = common::alloc_guest_region_bytes(src_size);
    assert_eq!(dst_size, src_size);
    unsafe {
        let mem = core::slice::from_raw_parts_mut(dst_base as *mut u8, dst_size as usize);
        mem.fill(0xFF);
    }

    let mut vm2 = WorkerVmSnapshot::new(dst_base, dst_size).expect("new WorkerVmSnapshot #2");
    let restored = vm2.restore_snapshot(&snapshot).expect("restore_snapshot");

    unsafe {
        let mem = core::slice::from_raw_parts(dst_base as *const u8, dst_size as usize);
        for (i, b) in mem.iter().enumerate() {
            assert_eq!(*b, pattern_byte(i), "RAM mismatch at offset {i}");
        }
    }

    let restored_cpu_val = Reflect::get(&restored, &JsValue::from_str("cpu")).unwrap();
    let restored_cpu = Uint8Array::new(&restored_cpu_val);
    let mut restored_cpu_bytes = vec![0u8; restored_cpu.length() as usize];
    restored_cpu.copy_to(&mut restored_cpu_bytes);
    assert_eq!(
        restored_cpu_bytes, cpu_bytes,
        "restored CPU bytes must match input"
    );

    let restored_mmu_val = Reflect::get(&restored, &JsValue::from_str("mmu")).unwrap();
    let restored_mmu = Uint8Array::new(&restored_mmu_val);
    let mut restored_mmu_bytes = vec![0u8; restored_mmu.length() as usize];
    restored_mmu.copy_to(&mut restored_mmu_bytes);
    assert_eq!(
        restored_mmu_bytes, mmu_bytes,
        "restored MMU bytes must match input"
    );

    let devices_val = Reflect::get(&restored, &JsValue::from_str("devices")).unwrap();
    let devices = Array::from(&devices_val);
    assert_eq!(devices.length(), 1, "expected exactly one device entry");

    let dev0 = devices.get(0);
    let id = Reflect::get(&dev0, &JsValue::from_str("id"))
        .unwrap()
        .as_f64()
        .unwrap() as u32;
    let version = Reflect::get(&dev0, &JsValue::from_str("version"))
        .unwrap()
        .as_f64()
        .unwrap() as u16;
    let flags = Reflect::get(&dev0, &JsValue::from_str("flags"))
        .unwrap()
        .as_f64()
        .unwrap() as u16;
    assert_eq!(id, 0xDEAD_BEEF);
    assert_eq!(version, 2);
    assert_eq!(flags, 3);

    let data_val = Reflect::get(&dev0, &JsValue::from_str("data")).unwrap();
    let data_u8 = Uint8Array::new(&data_val);
    let mut restored_data = vec![0u8; data_u8.length() as usize];
    data_u8.copy_to(&mut restored_data);
    assert_eq!(restored_data, device_data, "restored device data mismatch");

    // Sanity: the restored state should be snapshot-able again deterministically.
    let snapshot2 = vm2.snapshot_full().expect("snapshot_full after restore");
    assert_eq!(
        snapshot2, snapshot,
        "snapshot bytes must roundtrip identically"
    );
}
