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
    // Add multiple devices (including networking IDs) and ensure snapshot bytes are canonical and
    // deterministic.
    vm.add_device_state(aero_snapshot::DeviceId::NET_STACK.0, 1, 0, &[0x01])
        .expect("add_device_state net.stack");
    vm.add_device_state(aero_snapshot::DeviceId::USB.0, 3, 7, &[0xAA, 0xBB, 0xCC])
        .expect("add_device_state usb.uhci");
    vm.add_device_state(aero_snapshot::DeviceId::E1000.0, 2, 5, &[0x42, 0x43])
        .expect("add_device_state net.e1000");
    vm.add_device_state(0x1234, 9, 9, &[0x99])
        .expect("add_device_state unknown");

    let a = vm.snapshot_full().expect("snapshot_full #1");
    let b = vm.snapshot_full().expect("snapshot_full #2");
    assert_eq!(a, b, "snapshot bytes must be deterministic");

    // The builder should also sort devices into a canonical order, so insertion order should not
    // affect snapshot bytes.
    let mut vm2 = WorkerVmSnapshot::new(guest_base, guest_size).expect("new WorkerVmSnapshot #2");
    vm2.set_cpu_state_v2(&cpu_bytes, &mmu_bytes)
        .expect("set_cpu_state_v2 #2");
    // Insert in a different order.
    vm2.add_device_state(0x1234, 9, 9, &[0x99])
        .expect("add_device_state unknown #2");
    vm2.add_device_state(aero_snapshot::DeviceId::E1000.0, 2, 5, &[0x42, 0x43])
        .expect("add_device_state net.e1000 #2");
    vm2.add_device_state(aero_snapshot::DeviceId::USB.0, 3, 7, &[0xAA, 0xBB, 0xCC])
        .expect("add_device_state usb.uhci #2");
    vm2.add_device_state(aero_snapshot::DeviceId::NET_STACK.0, 1, 0, &[0x01])
        .expect("add_device_state net.stack #2");

    let c = vm2.snapshot_full().expect("snapshot_full #3");
    assert_eq!(
        a, c,
        "snapshot bytes must be independent of device insertion order"
    );
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

    let device_data_unknown: Vec<u8> = (0..1024u32).map(|i| (i & 0xFF) as u8).collect();
    vm.add_device_state(0xDEAD_BEEF, 2, 3, &device_data_unknown)
        .expect("add_device_state unknown");

    let device_data_e1000 = vec![0xDE, 0xAD, 0xBE, 0xEF];
    vm.add_device_state(aero_snapshot::DeviceId::E1000.0, 7, 1, &device_data_e1000)
        .expect("add_device_state net.e1000");

    let device_data_net_stack = vec![0x11, 0x22, 0x33, 0x44, 0x55];
    vm.add_device_state(
        aero_snapshot::DeviceId::NET_STACK.0,
        9,
        4,
        &device_data_net_stack,
    )
    .expect("add_device_state net.stack");

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
    assert_eq!(devices.length(), 3, "expected three device entries");

    let mut restore_ids_in_order = Vec::with_capacity(devices.length() as usize);
    let mut restored_by_id = std::collections::HashMap::<u32, (u16, u16, Vec<u8>)>::new();
    for (idx, dev) in devices.iter().enumerate() {
        let id = Reflect::get(&dev, &JsValue::from_str("id"))
            .unwrap_or_else(|_| panic!("devices[{idx}].id missing"))
            .as_f64()
            .unwrap_or_else(|| panic!("devices[{idx}].id must be a number"))
            as u32;
        restore_ids_in_order.push(id);
        let version = Reflect::get(&dev, &JsValue::from_str("version"))
            .unwrap_or_else(|_| panic!("devices[{idx}].version missing"))
            .as_f64()
            .unwrap_or_else(|| panic!("devices[{idx}].version must be a number"))
            as u16;
        let flags = Reflect::get(&dev, &JsValue::from_str("flags"))
            .unwrap_or_else(|_| panic!("devices[{idx}].flags missing"))
            .as_f64()
            .unwrap_or_else(|| panic!("devices[{idx}].flags must be a number"))
            as u16;

        let data_val = Reflect::get(&dev, &JsValue::from_str("data"))
            .unwrap_or_else(|_| panic!("devices[{idx}].data missing"));
        let data_u8 = Uint8Array::new(&data_val);
        let mut data = vec![0u8; data_u8.length() as usize];
        data_u8.copy_to(&mut data);

        assert!(
            restored_by_id.insert(id, (version, flags, data)).is_none(),
            "duplicate device id in restore output: {id}"
        );
    }

    assert_eq!(
        restore_ids_in_order.as_slice(),
        &[
            aero_snapshot::DeviceId::E1000.0,
            aero_snapshot::DeviceId::NET_STACK.0,
            0xDEAD_BEEF
        ],
        "restore output should be in canonical (sorted) device id order"
    );

    let (version, flags, data) = restored_by_id
        .get(&0xDEAD_BEEF)
        .expect("missing unknown device");
    assert_eq!((*version, *flags), (2, 3));
    assert_eq!(
        data, &device_data_unknown,
        "restored unknown device data mismatch"
    );

    let (version, flags, data) = restored_by_id
        .get(&aero_snapshot::DeviceId::E1000.0)
        .expect("missing net.e1000 device");
    assert_eq!((*version, *flags), (7, 1));
    assert_eq!(data, &device_data_e1000, "restored net.e1000 data mismatch");

    let (version, flags, data) = restored_by_id
        .get(&aero_snapshot::DeviceId::NET_STACK.0)
        .expect("missing net.stack device");
    assert_eq!((*version, *flags), (9, 4));
    assert_eq!(
        data, &device_data_net_stack,
        "restored net.stack data mismatch"
    );

    // Sanity: the restored state should be snapshot-able again deterministically.
    let snapshot2 = vm2.snapshot_full().expect("snapshot_full after restore");
    assert_eq!(
        snapshot2, snapshot,
        "snapshot bytes must roundtrip identically"
    );
}
