#![cfg(target_arch = "wasm32")]

use aero_usb::hid::webhid::HidCollectionInfo;
use aero_wasm::UhciRuntime;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn webhid_mouse_collections_json() -> wasm_bindgen::JsValue {
    // Embed the normalized WebHID mouse fixture so wasm32 tests don't need filesystem access.
    let json = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ));
    let collections: Vec<HidCollectionInfo> =
        serde_json::from_str(json).expect("deserialize webhid_normalized_mouse.json");
    serde_wasm_bindgen::to_value(&collections).expect("collections to JsValue")
}

fn make_runtime() -> UhciRuntime {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x4000);
    UhciRuntime::new(guest_base, guest_size).expect("UhciRuntime::new")
}

#[wasm_bindgen_test]
fn uhci_runtime_can_attach_external_hub_with_configurable_port_count() {
    let mut rt = make_runtime();

    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to JsValue");
    rt.webhid_attach_hub(hub_path, Some(4))
        .expect("attach hub ok");

    // Should accept attachments to the highest configured downstream port.
    let path = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path to JsValue");
    rt.webhid_attach_at_path(
        1,
        0x1234,
        0x5678,
        Some("dev-1".to_string()),
        webhid_mouse_collections_json(),
        path,
    )
    .expect("attach WebHID behind hub ok");
}

#[wasm_bindgen_test]
fn uhci_runtime_supports_multiple_webhid_devices_behind_hub() {
    let mut rt = make_runtime();
    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to JsValue");
    rt.webhid_attach_hub(hub_path, Some(8))
        .expect("attach hub ok");

    let collections = webhid_mouse_collections_json();

    // Hub ports 1..=3 are reserved for synthetic HID devices, so use port 4+ for WebHID passthrough.
    let path1 = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path1");
    rt.webhid_attach_at_path(
        1,
        0x1234,
        0x0001,
        Some("dev-1".to_string()),
        collections.clone(),
        path1,
    )
    .expect("attach dev1 ok");

    let path2 = serde_wasm_bindgen::to_value(&vec![0u32, 5u32]).expect("path2");
    rt.webhid_attach_at_path(
        2,
        0x1234,
        0x0002,
        Some("dev-2".to_string()),
        collections,
        path2,
    )
    .expect("attach dev2 ok");
}

#[wasm_bindgen_test]
fn uhci_runtime_webusb_attach_is_strict_about_preferred_port() {
    let mut rt = make_runtime();

    // Root port 1 is free initially.
    assert_eq!(rt.webusb_attach(Some(1)).expect("webusb attach ok"), 1);

    // Occupy port 1 with a WebHID device and ensure WebUSB attach fails.
    rt.webusb_detach();
    rt.webhid_attach(
        1,
        0x1234,
        0x5678,
        Some("hid".to_string()),
        webhid_mouse_collections_json(),
        Some(1),
    )
    .expect("webhid attach root port 1 ok");

    assert!(
        rt.webusb_attach(Some(1)).is_err(),
        "expected webusb_attach(1) to error when port 1 is occupied"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_resizes_external_hub_and_reattaches_devices() {
    let mut rt = make_runtime();
    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to JsValue");
    rt.webhid_attach_hub(hub_path.clone(), Some(2))
        .expect("attach hub 2 ok");

    let collections = webhid_mouse_collections_json();

    let p1 = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("p1");
    rt.webhid_attach_at_path(
        10,
        0x1234,
        0x0001,
        Some("dev-10".to_string()),
        collections.clone(),
        p1,
    )
    .expect("attach dev10 ok");

    let p2 = serde_wasm_bindgen::to_value(&vec![0u32, 5u32]).expect("p2");
    rt.webhid_attach_at_path(
        11,
        0x1234,
        0x0002,
        Some("dev-11".to_string()),
        collections.clone(),
        p2,
    )
    .expect("attach dev11 ok");

    // Resize to a larger hub. This should disconnect+reconnect the hub and then reattach the
    // downstream devices without panicking.
    rt.webhid_attach_hub(hub_path, Some(8))
        .expect("resize hub ok");

    // After resizing we should be able to attach to the new higher port.
    let p8 = serde_wasm_bindgen::to_value(&vec![0u32, 8u32]).expect("p8");
    rt.webhid_attach_at_path(
        12,
        0x1234,
        0x0003,
        Some("dev-12".to_string()),
        collections,
        p8,
    )
    .expect("attach dev12 ok");
}
