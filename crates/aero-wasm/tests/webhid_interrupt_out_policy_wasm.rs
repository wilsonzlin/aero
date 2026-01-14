#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::SnapshotReader;
use aero_io_snapshot::io::state::codec::Decoder;
use aero_usb::hid::webhid;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use aero_wasm::{UhciRuntime, WebHidPassthroughBridge};
use wasm_bindgen_test::wasm_bindgen_test;

fn make_minimal_item(usage_page: u32, usage: u32) -> webhid::HidReportItem {
    webhid::HidReportItem {
        usage_page,
        usages: vec![usage],
        usage_minimum: 0,
        usage_maximum: 0,
        report_size: 8,
        report_count: 1,
        unit_exponent: 0,
        unit: 0,
        logical_minimum: 0,
        logical_maximum: 255,
        physical_minimum: 0,
        physical_maximum: 0,
        strings: vec![],
        string_minimum: 0,
        string_maximum: 0,
        designators: vec![],
        designator_minimum: 0,
        designator_maximum: 0,
        is_absolute: true,
        is_array: false,
        is_buffered_bytes: false,
        is_constant: false,
        is_linear: true,
        is_range: false,
        is_relative: false,
        is_volatile: false,
        has_null: false,
        has_preferred_state: true,
        is_wrapped: false,
    }
}

fn large_output_collections() -> Vec<webhid::HidCollectionInfo> {
    let mut large_item = make_minimal_item(0x01, 0x00); // Generic Desktop / Undefined
    // Report size = 8 bits, report count = 65 => 65-byte Output report (on-wire).
    large_item.report_count = 65;

    vec![webhid::HidCollectionInfo {
        usage_page: 0x01, // Generic Desktop
        usage: 0x00,      // Undefined
        collection_type: webhid::HidCollectionType::Application,
        children: vec![],
        input_reports: vec![webhid::HidReportInfo {
            report_id: 0,
            items: vec![make_minimal_item(0x01, 0x00)],
        }],
        output_reports: vec![webhid::HidReportInfo {
            report_id: 0,
            items: vec![large_item],
        }],
        feature_reports: vec![],
    }]
}

fn large_output_collections_with_report_id_prefix() -> Vec<webhid::HidCollectionInfo> {
    // When report IDs are in use (reportId != 0), the on-wire report includes a 1-byte prefix.
    // Use a 64-byte payload so the on-wire size is 65 bytes (which must disable interrupt OUT).
    let mut large_item = make_minimal_item(0x01, 0x00);
    large_item.report_count = 64;

    vec![webhid::HidCollectionInfo {
        usage_page: 0x01,
        usage: 0x00,
        collection_type: webhid::HidCollectionType::Application,
        children: vec![],
        input_reports: vec![webhid::HidReportInfo {
            report_id: 1,
            items: vec![make_minimal_item(0x01, 0x00)],
        }],
        output_reports: vec![webhid::HidReportInfo {
            report_id: 1,
            items: vec![large_item],
        }],
        feature_reports: vec![],
    }]
}

fn max_sized_output_collections_with_report_id_prefix() -> Vec<webhid::HidCollectionInfo> {
    // 63-byte payload + 1-byte reportId prefix => 64 bytes on wire (boundary case that should
    // still allow interrupt OUT).
    let mut item = make_minimal_item(0x01, 0x00);
    item.report_count = 63;

    vec![webhid::HidCollectionInfo {
        usage_page: 0x01,
        usage: 0x00,
        collection_type: webhid::HidCollectionType::Application,
        children: vec![],
        input_reports: vec![webhid::HidReportInfo {
            report_id: 1,
            items: vec![make_minimal_item(0x01, 0x00)],
        }],
        output_reports: vec![webhid::HidReportInfo {
            report_id: 1,
            items: vec![item],
        }],
        feature_reports: vec![],
    }]
}

fn interface_num_endpoints(config_desc: &[u8]) -> Option<u8> {
    let mut off = 0usize;
    while off + 2 <= config_desc.len() {
        let len = config_desc[off] as usize;
        if len == 0 || off + len > config_desc.len() {
            break;
        }
        let ty = config_desc[off + 1];
        if ty == 0x04 && len >= 5 {
            // Interface descriptor: bNumEndpoints at offset 4.
            return Some(config_desc[off + 4]);
        }
        off += len;
    }
    None
}

fn endpoint_addresses(config_desc: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 2 <= config_desc.len() {
        let len = config_desc[off] as usize;
        if len == 0 || off + len > config_desc.len() {
            break;
        }
        let ty = config_desc[off + 1];
        if ty == 0x05 && len >= 3 {
            // Endpoint descriptor: bEndpointAddress at offset 2.
            out.push(config_desc[off + 2]);
        }
        off += len;
    }
    out
}

#[wasm_bindgen_test]
fn webhid_passthrough_bridge_omits_interrupt_out_for_large_output_reports() {
    let collections_json =
        serde_wasm_bindgen::to_value(&large_output_collections()).expect("collections to JsValue");
    let bridge = WebHidPassthroughBridge::new(
        0x1234,
        0x5678,
        Some("WebHID".to_string()),
        Some("Large Output Device".to_string()),
        None,
        collections_json,
    )
    .expect("WebHidPassthroughBridge::new ok");

    let mut dev = bridge.as_usb_device();
    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 256,
        },
        None,
    );
    let ControlResponse::Data(bytes) = resp else {
        panic!("expected config descriptor bytes, got {resp:?}");
    };

    assert_eq!(interface_num_endpoints(&bytes), Some(1));
    assert_eq!(
        endpoint_addresses(&bytes),
        vec![0x81],
        "expected config descriptor to omit interrupt OUT endpoint 0x01: {bytes:02x?}"
    );
}

#[wasm_bindgen_test]
fn webhid_passthrough_bridge_omits_interrupt_out_when_report_id_prefix_exceeds_max_packet_size() {
    let collections_json =
        serde_wasm_bindgen::to_value(&large_output_collections_with_report_id_prefix())
            .expect("collections to JsValue");
    let bridge = WebHidPassthroughBridge::new(
        0x1234,
        0x5678,
        Some("WebHID".to_string()),
        Some("Large Output Device (reportId)".to_string()),
        None,
        collections_json,
    )
    .expect("WebHidPassthroughBridge::new ok");

    let mut dev = bridge.as_usb_device();
    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 256,
        },
        None,
    );
    let ControlResponse::Data(bytes) = resp else {
        panic!("expected config descriptor bytes, got {resp:?}");
    };

    assert_eq!(interface_num_endpoints(&bytes), Some(1));
    assert_eq!(
        endpoint_addresses(&bytes),
        vec![0x81],
        "expected config descriptor to omit interrupt OUT endpoint 0x01: {bytes:02x?}"
    );
}

#[wasm_bindgen_test]
fn webhid_passthrough_bridge_retains_interrupt_out_when_report_id_prefix_fits_in_packet() {
    let collections_json =
        serde_wasm_bindgen::to_value(&max_sized_output_collections_with_report_id_prefix())
            .expect("collections to JsValue");
    let bridge = WebHidPassthroughBridge::new(
        0x1234,
        0x5678,
        Some("WebHID".to_string()),
        Some("Max Output Device (reportId)".to_string()),
        None,
        collections_json,
    )
    .expect("WebHidPassthroughBridge::new ok");

    let mut dev = bridge.as_usb_device();
    let resp = dev.handle_control_request(
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 256,
        },
        None,
    );
    let ControlResponse::Data(bytes) = resp else {
        panic!("expected config descriptor bytes, got {resp:?}");
    };

    assert_eq!(interface_num_endpoints(&bytes), Some(2));
    let eps = endpoint_addresses(&bytes);
    assert_eq!(
        eps,
        vec![0x81, 0x01],
        "expected config descriptor to include interrupt OUT endpoint 0x01: {bytes:02x?}"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_webhid_attach_marks_interrupt_out_unavailable_for_large_output_reports() {
    let mut rt = UhciRuntime::new(0, 0).expect("UhciRuntime::new ok");
    let collections_json =
        serde_wasm_bindgen::to_value(&large_output_collections()).expect("collections to JsValue");

    rt.webhid_attach(
        1,
        0x1234,
        0x5678,
        Some("Large Output Device".to_string()),
        collections_json,
        Some(0),
    )
    .expect("webhid_attach ok");

    let snap = rt.save_state();
    let r = SnapshotReader::parse(&snap, *b"UHRT").expect("parse UHCI runtime snapshot");
    let webhid_list = r
        .bytes(6)
        .expect("snapshot should include WebHID device list (tag 6)");

    let mut d = Decoder::new(webhid_list);
    let count = d.u32().expect("WebHID list count") as usize;
    assert_eq!(count, 1);
    let rec_len = d.u32().expect("WebHID record length") as usize;
    let rec = d.bytes(rec_len).expect("WebHID record bytes");
    d.finish().expect("WebHID list trailing bytes");

    let mut rd = Decoder::new(rec);
    assert_eq!(rd.u32().expect("device_id"), 1);
    let _loc_kind = rd.u8().expect("loc_kind");
    let _loc_port = rd.u8().expect("loc_port");
    let _vendor_id = rd.u16().expect("vendor_id");
    let _product_id = rd.u16().expect("product_id");
    let _product = rd.vec_u8().expect("product bytes");
    let _report_descriptor = rd.vec_u8().expect("report descriptor");
    let has_interrupt_out = rd.bool().expect("has_interrupt_out");
    let _dev_state = rd.vec_u8().expect("device state");
    rd.finish().expect("WebHID record trailing bytes");

    assert!(
        !has_interrupt_out,
        "expected UhciRuntime to disable interrupt OUT for >64-byte output reports"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_webhid_attach_marks_interrupt_out_unavailable_when_report_id_prefix_exceeds_max_packet_size()
 {
    let mut rt = UhciRuntime::new(0, 0).expect("UhciRuntime::new ok");
    let collections_json =
        serde_wasm_bindgen::to_value(&large_output_collections_with_report_id_prefix())
            .expect("collections to JsValue");

    rt.webhid_attach(
        1,
        0x1234,
        0x5678,
        Some("Large Output Device (reportId)".to_string()),
        collections_json,
        Some(0),
    )
    .expect("webhid_attach ok");

    let snap = rt.save_state();
    let r = SnapshotReader::parse(&snap, *b"UHRT").expect("parse UHCI runtime snapshot");
    let webhid_list = r
        .bytes(6)
        .expect("snapshot should include WebHID device list (tag 6)");

    let mut d = Decoder::new(webhid_list);
    let count = d.u32().expect("WebHID list count") as usize;
    assert_eq!(count, 1);
    let rec_len = d.u32().expect("WebHID record length") as usize;
    let rec = d.bytes(rec_len).expect("WebHID record bytes");
    d.finish().expect("WebHID list trailing bytes");

    let mut rd = Decoder::new(rec);
    assert_eq!(rd.u32().expect("device_id"), 1);
    let _loc_kind = rd.u8().expect("loc_kind");
    let _loc_port = rd.u8().expect("loc_port");
    let _vendor_id = rd.u16().expect("vendor_id");
    let _product_id = rd.u16().expect("product_id");
    let _product = rd.vec_u8().expect("product bytes");
    let _report_descriptor = rd.vec_u8().expect("report descriptor");
    let has_interrupt_out = rd.bool().expect("has_interrupt_out");
    let _dev_state = rd.vec_u8().expect("device state");
    rd.finish().expect("WebHID record trailing bytes");

    assert!(
        !has_interrupt_out,
        "expected UhciRuntime to disable interrupt OUT when reportId prefix makes output report >64 bytes"
    );
}
