#![cfg(target_arch = "wasm32")]

use aero_usb::hid::webhid::HidCollectionInfo;
use aero_wasm::UhciRuntime;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

#[wasm_bindgen_test]
fn uhci_runtime_supports_external_hub_paths_and_webusb_on_root_port_1() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20_000);

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    // The host WebHID passthrough manager attaches a virtual external hub at root port 0 with a
    // configurable downstream port count.
    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to_value");
    // Start with a single downstream port to cover the hub grow path when we later attach devices
    // at higher hub ports.
    rt.webhid_attach_hub(hub_path, Some(1))
        .expect("attach external hub");

    let collections: Vec<HidCollectionInfo> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )))
    .expect("deserialize webhid_normalized_mouse.json fixture");
    let collections_json =
        serde_wasm_bindgen::to_value(&collections).expect("collections to_value");

    // Invalid paths should return a JS error rather than trapping.
    let invalid_root_path =
        serde_wasm_bindgen::to_value(&vec![1u32, 1u32]).expect("invalid_root_path to_value");
    assert!(
        rt.webhid_attach_at_path(
            999,
            0x1234,
            0x9999,
            Some("Invalid path".to_string()),
            collections_json.clone(),
            invalid_root_path,
        )
        .is_err()
    );
    let nested_path =
        serde_wasm_bindgen::to_value(&vec![0u32, 1u32, 1u32]).expect("nested_path to_value");
    assert!(
        rt.webhid_attach_at_path(
            1000,
            0x1234,
            0x9998,
            Some("Nested path".to_string()),
            collections_json.clone(),
            nested_path,
        )
        .is_err()
    );

    // Attach two WebHID passthrough devices behind the hub.
    //
    // Note: hub ports 1..=3 are reserved for Aero's synthetic HID devices (keyboard/mouse/gamepad),
    // so WebHID passthrough should use port 4+.
    let path1 = serde_wasm_bindgen::to_value(&vec![0u32, 4u32]).expect("path1 to_value");
    rt.webhid_attach_at_path(
        1,
        0x1234,
        0x0001,
        Some("Test HID #1".to_string()),
        collections_json.clone(),
        path1,
    )
    .expect("attach WebHID device #1");

    let path2 = serde_wasm_bindgen::to_value(&vec![0u32, 5u32]).expect("path2 to_value");
    rt.webhid_attach_at_path(
        2,
        0x1234,
        0x0002,
        Some("Test HID #2".to_string()),
        collections_json.clone(),
        path2,
    )
    .expect("attach WebHID device #2");

    // Root port 1 is reserved for the guest-visible WebUSB passthrough device. Ensure it can be
    // attached concurrently without stealing root port 0 (the hub).
    assert!(rt.webusb_attach(Some(0)).is_err());
    let webusb_port = rt.webusb_attach(Some(1)).expect("attach WebUSB");
    assert_eq!(webusb_port, 1);
}
