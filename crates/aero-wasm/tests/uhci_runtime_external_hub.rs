#![cfg(target_arch = "wasm32")]

use aero_usb::hid::webhid::HidCollectionInfo;
use aero_wasm::UhciRuntime;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn uhci_runtime_supports_external_hub_paths_and_webusb_on_root_port_1() {
    // Back the guest RAM region with a Rust Vec so we can hand its linear-memory address to the
    // runtime constructor.
    let mut guest = vec![0u8; 0x20_000];
    let guest_base = guest.as_mut_ptr() as u32;
    let guest_size = guest.len() as u32;

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    // The host WebHID passthrough manager attaches a virtual external hub at root port 0 with a
    // configurable downstream port count.
    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to_value");
    rt.webhid_attach_hub(hub_path, Some(16))
        .expect("attach external hub");

    let collections: Vec<HidCollectionInfo> = serde_json::from_str(include_str!(
        "../../tests/fixtures/hid/webhid_normalized_mouse.json"
    ))
    .expect("deserialize webhid_normalized_mouse.json fixture");
    let collections_json =
        serde_wasm_bindgen::to_value(&collections).expect("collections to_value");

    // Attach two WebHID passthrough devices behind the hub on ports 1 and 2.
    let path1 = serde_wasm_bindgen::to_value(&vec![0u32, 1u32]).expect("path1 to_value");
    rt.webhid_attach_at_path(
        1,
        0x1234,
        0x0001,
        Some("Test HID #1".to_string()),
        collections_json.clone(),
        path1,
    )
    .expect("attach WebHID device #1");

    let path2 = serde_wasm_bindgen::to_value(&vec![0u32, 2u32]).expect("path2 to_value");
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
    let webusb_port = rt.webusb_attach(Some(1)).expect("attach WebUSB");
    assert_eq!(webusb_port, 1);
}

