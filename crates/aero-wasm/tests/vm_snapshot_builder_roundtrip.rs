#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::{SnapshotVersion, SnapshotWriter};
use aero_snapshot::{CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, SnapshotTarget};
use aero_wasm::{guest_ram_layout, vm_snapshot_restore, vm_snapshot_save};
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn ram_pattern_byte(i: usize) -> u8 {
    // Cheap deterministic non-constant pattern that still compresses reasonably.
    let v = (i as u32)
        .wrapping_mul(0x9E37_79B1)
        .wrapping_add(0x7F4A_7C15);
    (v ^ (v >> 16)) as u8
}

fn build_cpu_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    CpuState::default()
        .encode_v2(&mut out)
        .expect("CpuState::encode_v2 ok");
    out
}

fn build_mmu_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    MmuState::default()
        .encode_v2(&mut out)
        .expect("MmuState::encode_v2 ok");
    out
}

fn build_usb_blob(device_version: SnapshotVersion) -> Vec<u8> {
    let mut w = SnapshotWriter::new(*b"UHRT", device_version);
    w.field_u32(1, 0x1234_5678);
    w.field_u8(2, 0xAB);
    w.finish()
}

fn build_net_stack_blob(device_version: SnapshotVersion) -> Vec<u8> {
    // Match the `aero-io-snapshot` device id used by `NetworkStackState`.
    let mut w = SnapshotWriter::new(*b"NETS", device_version);
    w.field_u32(1, 0xDEAD_BEEF);
    w.finish()
}

fn build_net_e1000_blob(device_version: SnapshotVersion) -> Vec<u8> {
    // Dummy `aero-io-snapshot` blob for the E1000 NIC model.
    let mut w = SnapshotWriter::new(*b"E1K0", device_version);
    w.field_u8(1, 0x42);
    w.finish()
}

fn build_devices_js(entries: &[(&str, &[u8])]) -> JsValue {
    let arr = Array::new();
    for (kind, bytes) in entries {
        let obj = Object::new();
        Reflect::set(&obj, &JsValue::from_str("kind"), &JsValue::from_str(kind)).expect("set kind");
        let bytes_js = Uint8Array::from(*bytes);
        Reflect::set(&obj, &JsValue::from_str("bytes"), bytes_js.as_ref()).expect("set bytes");
        arr.push(&obj);
    }
    arr.into()
}

#[wasm_bindgen_test]
fn vm_snapshot_builder_roundtrips_guest_ram_and_device_states() {
    // Ensure we have at least one guest page above the runtime-reserved region (128MiB).
    let _ = common::alloc_guest_region_bytes(64 * 1024);

    // VM snapshot builder uses the fixed guest layout contract (`guest_base` is always the start of
    // the runtime-reserved region aligned up to 64KiB).
    let layout = guest_ram_layout(0);
    let guest_base = layout.guest_base();

    let mem_bytes = (core::arch::wasm32::memory_size(0) as u64).saturating_mul(64 * 1024);
    let guest_size = mem_bytes
        .saturating_sub(guest_base as u64)
        .try_into()
        .expect("guest_size fits in usize");
    assert!(guest_size > 0, "guest_size should be non-zero");

    // Safety: We just ensured linear memory has at least `guest_base + guest_size` bytes.
    let guest = unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size) };

    for (i, b) in guest.iter_mut().enumerate() {
        *b = ram_pattern_byte(i);
    }

    let cpu_bytes = build_cpu_bytes();
    let mmu_bytes = build_mmu_bytes();

    let cpu_js = Uint8Array::from(cpu_bytes.as_slice());
    let mmu_js = Uint8Array::from(mmu_bytes.as_slice());

    let usb_version = SnapshotVersion::new(3, 7);
    let usb_blob = build_usb_blob(usb_version);
    let i8042_version = SnapshotVersion::new(1, 2);
    let i8042_blob = SnapshotWriter::new(*b"8042", i8042_version).finish();
    let hda_version = SnapshotVersion::new(9, 1);
    let hda_blob = SnapshotWriter::new(*b"HDA0", hda_version).finish();
    let net_e1000_version = SnapshotVersion::new(2, 5);
    let net_e1000_blob = build_net_e1000_blob(net_e1000_version);
    let net_stack_version = SnapshotVersion::new(1, 0);
    let net_stack_blob = build_net_stack_blob(net_stack_version);
    let devices_js = build_devices_js(&[
        ("usb.uhci", &usb_blob),
        ("input.i8042", &i8042_blob),
        ("audio.hda", &hda_blob),
        ("net.e1000", &net_e1000_blob),
        ("net.stack", &net_stack_blob),
    ]);

    let snap_a = vm_snapshot_save(cpu_js.clone(), mmu_js.clone(), devices_js.clone())
        .expect("vm_snapshot_save ok")
        .to_vec();
    let snap_b = vm_snapshot_save(cpu_js.clone(), mmu_js.clone(), devices_js.clone())
        .expect("vm_snapshot_save ok")
        .to_vec();

    assert_eq!(snap_a, snap_b, "snapshot bytes should be deterministic");

    // Inspect device header mapping by decoding via `aero_snapshot` and capturing device states.
    #[derive(Default)]
    struct InspectTarget {
        ram_len: usize,
        devices: Vec<DeviceState>,
    }

    impl SnapshotTarget for InspectTarget {
        fn restore_cpu_state(&mut self, _state: CpuState) {}
        fn restore_mmu_state(&mut self, _state: MmuState) {}
        fn restore_device_states(&mut self, states: Vec<DeviceState>) {
            self.devices = states;
        }
        fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

        fn ram_len(&self) -> usize {
            self.ram_len
        }

        fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
            use aero_snapshot::SnapshotError;

            let offset: usize = offset
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
            let end = offset
                .checked_add(data.len())
                .ok_or(SnapshotError::Corrupt("ram write overflow"))?;
            if end > self.ram_len {
                return Err(SnapshotError::Corrupt("ram write out of bounds"));
            }
            Ok(())
        }
    }

    let mut inspect = InspectTarget {
        ram_len: guest_size,
        ..Default::default()
    };
    aero_snapshot::restore_snapshot_checked(
        &mut std::io::Cursor::new(snap_a.as_slice()),
        &mut inspect,
        aero_snapshot::RestoreOptions::default(),
    )
    .expect("restore_snapshot_checked ok");

    assert_eq!(
        inspect.devices.len(),
        5,
        "snapshot should contain exactly five device states"
    );

    let usb_state = inspect
        .devices
        .iter()
        .find(|d| d.id == DeviceId::USB)
        .expect("snapshot should contain USB device state");
    assert_eq!(
        (usb_state.version, usb_state.flags),
        (usb_version.major, usb_version.minor),
        "USB DeviceState version/flags should reflect aero-io-snapshot header"
    );
    assert_eq!(
        usb_state.data, usb_blob,
        "USB device blob should be preserved verbatim"
    );

    let i8042_state = inspect
        .devices
        .iter()
        .find(|d| d.id == DeviceId::I8042)
        .expect("snapshot should contain i8042 device state");
    assert_eq!(
        (i8042_state.version, i8042_state.flags),
        (i8042_version.major, i8042_version.minor),
        "i8042 DeviceState version/flags should reflect aero-io-snapshot header"
    );
    assert_eq!(
        i8042_state.data, i8042_blob,
        "i8042 device blob should be preserved verbatim"
    );

    let hda_state = inspect
        .devices
        .iter()
        .find(|d| d.id == DeviceId::HDA)
        .expect("snapshot should contain HDA device state");
    assert_eq!(
        (hda_state.version, hda_state.flags),
        (hda_version.major, hda_version.minor),
        "HDA DeviceState version/flags should reflect aero-io-snapshot header"
    );
    assert_eq!(
        hda_state.data, hda_blob,
        "HDA device blob should be preserved verbatim"
    );

    let net_e1000_state = inspect
        .devices
        .iter()
        .find(|d| d.id == DeviceId::E1000)
        .expect("snapshot should contain net.e1000 device state");
    assert_eq!(
        (net_e1000_state.version, net_e1000_state.flags),
        (net_e1000_version.major, net_e1000_version.minor),
        "net.e1000 DeviceState version/flags should reflect aero-io-snapshot header"
    );
    assert_eq!(
        net_e1000_state.data, net_e1000_blob,
        "net.e1000 device blob should be preserved verbatim"
    );

    let net_stack_state = inspect
        .devices
        .iter()
        .find(|d| d.id == DeviceId::NET_STACK)
        .expect("snapshot should contain net.stack device state");
    assert_eq!(
        (net_stack_state.version, net_stack_state.flags),
        (net_stack_version.major, net_stack_version.minor),
        "net.stack DeviceState version/flags should reflect aero-io-snapshot header"
    );
    assert_eq!(
        net_stack_state.data, net_stack_blob,
        "net.stack device blob should be preserved verbatim"
    );

    // Clear RAM and restore via the wasm export.
    guest.fill(0);

    let restored =
        vm_snapshot_restore(Uint8Array::from(snap_a.as_slice())).expect("vm_snapshot_restore ok");
    let obj: Object = restored.dyn_into().expect("restore result object");

    let cpu_out = Reflect::get(&obj, &JsValue::from_str("cpu"))
        .expect("cpu property")
        .dyn_into::<Uint8Array>()
        .expect("cpu Uint8Array")
        .to_vec();
    let mmu_out = Reflect::get(&obj, &JsValue::from_str("mmu"))
        .expect("mmu property")
        .dyn_into::<Uint8Array>()
        .expect("mmu Uint8Array")
        .to_vec();

    assert_eq!(cpu_out, cpu_bytes, "CPU state should roundtrip");
    assert_eq!(mmu_out, mmu_bytes, "MMU state should roundtrip");

    let devices_out_val = Reflect::get(&obj, &JsValue::from_str("devices")).expect("devices get");
    assert!(
        !devices_out_val.is_undefined() && !devices_out_val.is_null(),
        "devices should be present"
    );
    let devices_out: Array = devices_out_val.dyn_into().expect("devices array");
    assert_eq!(devices_out.length(), 5, "expected five device states");

    let mut devices_by_kind = std::collections::BTreeMap::<String, Vec<u8>>::new();
    for (idx, entry) in devices_out.iter().enumerate() {
        let dev: Object = entry.dyn_into().expect("device object");
        let kind = Reflect::get(&dev, &JsValue::from_str("kind"))
            .expect("kind property")
            .as_string()
            .unwrap_or_else(|| panic!("devices[{idx}].kind must be string"));
        let bytes = Reflect::get(&dev, &JsValue::from_str("bytes"))
            .expect("bytes property")
            .dyn_into::<Uint8Array>()
            .expect("bytes Uint8Array")
            .to_vec();
        assert!(
            devices_by_kind.insert(kind.clone(), bytes).is_none(),
            "duplicate device kind in restore output: {kind}"
        );
    }

    assert_eq!(
        devices_by_kind
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .as_slice(),
        &["audio.hda", "input.i8042", "net.e1000", "net.stack", "usb.uhci"],
        "restored device kinds should match input kinds"
    );
    assert_eq!(
        devices_by_kind.get("usb.uhci").unwrap(),
        &usb_blob,
        "USB device bytes should roundtrip"
    );
    assert_eq!(
        devices_by_kind.get("input.i8042").unwrap(),
        &i8042_blob,
        "input.i8042 device bytes should roundtrip"
    );
    assert_eq!(
        devices_by_kind.get("audio.hda").unwrap(),
        &hda_blob,
        "audio.hda device bytes should roundtrip"
    );
    assert_eq!(
        devices_by_kind.get("net.e1000").unwrap(),
        &net_e1000_blob,
        "net.e1000 device bytes should roundtrip"
    );
    assert_eq!(
        devices_by_kind.get("net.stack").unwrap(),
        &net_stack_blob,
        "net.stack device bytes should roundtrip"
    );

    for (i, &b) in guest.iter().enumerate() {
        assert_eq!(b, ram_pattern_byte(i), "RAM mismatch at offset {i}");
    }
}

#[wasm_bindgen_test]
fn vm_snapshot_builder_roundtrips_unknown_device_id_kind() {
    // Ensure we have at least one guest page above the runtime-reserved region (128MiB).
    let _ = common::alloc_guest_region_bytes(64 * 1024);

    let cpu_bytes = build_cpu_bytes();
    let mmu_bytes = build_mmu_bytes();

    let cpu_js = Uint8Array::from(cpu_bytes.as_slice());
    let mmu_js = Uint8Array::from(mmu_bytes.as_slice());

    let unknown_id: u32 = 0xCAFE_BABE;
    let unknown_kind = format!("device.{unknown_id}");
    let unknown_version = SnapshotVersion::new(9, 4);
    let unknown_blob = {
        let mut w = SnapshotWriter::new(*b"UNKN", unknown_version);
        w.field_u16(1, 0xBEEF);
        w.finish()
    };

    let devices_js = build_devices_js(&[(unknown_kind.as_str(), unknown_blob.as_slice())]);
    let snap = vm_snapshot_save(cpu_js.clone(), mmu_js.clone(), devices_js.clone())
        .expect("vm_snapshot_save ok")
        .to_vec();

    #[derive(Default)]
    struct InspectTarget {
        ram_len: usize,
        devices: Vec<DeviceState>,
    }

    impl SnapshotTarget for InspectTarget {
        fn restore_cpu_state(&mut self, _state: CpuState) {}
        fn restore_mmu_state(&mut self, _state: MmuState) {}
        fn restore_device_states(&mut self, states: Vec<DeviceState>) {
            self.devices = states;
        }
        fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

        fn ram_len(&self) -> usize {
            self.ram_len
        }

        fn write_ram(&mut self, _offset: u64, _data: &[u8]) -> aero_snapshot::Result<()> {
            Ok(())
        }
    }

    let layout = guest_ram_layout(0);
    let guest_base = layout.guest_base();
    let mem_bytes = (core::arch::wasm32::memory_size(0) as u64).saturating_mul(64 * 1024);
    let guest_size = mem_bytes
        .saturating_sub(guest_base as u64)
        .try_into()
        .expect("guest_size fits in usize");

    let mut inspect = InspectTarget {
        ram_len: guest_size,
        ..Default::default()
    };
    aero_snapshot::restore_snapshot_checked(
        &mut std::io::Cursor::new(snap.as_slice()),
        &mut inspect,
        aero_snapshot::RestoreOptions::default(),
    )
    .expect("restore_snapshot_checked ok");

    assert_eq!(inspect.devices.len(), 1, "expected exactly one device");
    let state = &inspect.devices[0];
    assert_eq!(
        state.id,
        DeviceId(unknown_id),
        "unknown device id should be preserved as numeric DeviceId"
    );
    assert_eq!(
        (state.version, state.flags),
        (unknown_version.major, unknown_version.minor),
        "unknown DeviceState version/flags should reflect aero-io-snapshot header"
    );

    let restored =
        vm_snapshot_restore(Uint8Array::from(snap.as_slice())).expect("vm_snapshot_restore ok");
    let obj: Object = restored.dyn_into().expect("restore result object");
    let devices_out_val = Reflect::get(&obj, &JsValue::from_str("devices")).expect("devices get");
    let devices_out: Array = devices_out_val.dyn_into().expect("devices array");
    assert_eq!(devices_out.length(), 1, "expected exactly one device");
    let dev0: Object = devices_out.get(0).dyn_into().expect("device object");
    let kind = Reflect::get(&dev0, &JsValue::from_str("kind"))
        .expect("kind get")
        .as_string()
        .expect("kind string");
    assert_eq!(
        kind, unknown_kind,
        "unknown device kind should roundtrip unchanged"
    );
    let bytes = Reflect::get(&dev0, &JsValue::from_str("bytes"))
        .expect("bytes get")
        .dyn_into::<Uint8Array>()
        .expect("bytes Uint8Array")
        .to_vec();
    assert_eq!(bytes, unknown_blob, "unknown device bytes should roundtrip");
}
