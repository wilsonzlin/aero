#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::{SnapshotReader, codec::Decoder};
use aero_usb::hid::webhid::HidCollectionInfo;
use aero_usb::passthrough::UsbHostAction;
use aero_wasm::UhciRuntime;
use core::fmt::Write as _;
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

const SNAPSHOT_HEADER_BYTES: usize = 16; // `aero_io_snapshot::io::state` header length

// UHCI register offsets / bits (mirrors `crates/aero-usb/src/uhci.rs` tests).
const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

const USBCMD_RUN: u16 = 1 << 0;
const USBSTS_USBINT: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;
const PORTSC_CCS: u16 = 1 << 0;
const PORTSC_PED: u16 = 1 << 2;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/token fields.
const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

#[derive(Debug)]
struct SnapshotField<'a> {
    tag: u16,
    data: &'a [u8],
}

fn parse_snapshot_fields(bytes: &[u8]) -> Result<Vec<SnapshotField<'_>>, String> {
    if bytes.len() < SNAPSHOT_HEADER_BYTES {
        return Err(format!(
            "snapshot too short ({} bytes, expected >= {SNAPSHOT_HEADER_BYTES})",
            bytes.len()
        ));
    }
    let mut offset = SNAPSHOT_HEADER_BYTES;
    let mut fields = Vec::new();
    while offset < bytes.len() {
        if offset + 6 > bytes.len() {
            return Err(format!("truncated TLV header at offset {offset}"));
        }
        let tag = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let len = u32::from_le_bytes([
            bytes[offset + 2],
            bytes[offset + 3],
            bytes[offset + 4],
            bytes[offset + 5],
        ]) as usize;
        offset += 6;
        if offset + len > bytes.len() {
            return Err(format!(
                "truncated TLV payload for tag {tag} at offset {offset} (len={len})"
            ));
        }
        fields.push(SnapshotField {
            tag,
            data: &bytes[offset..offset + len],
        });
        offset += len;
    }
    Ok(fields)
}

fn first_diff_idx(a: &[u8], b: &[u8]) -> usize {
    let limit = a.len().min(b.len());
    for idx in 0..limit {
        if a[idx] != b[idx] {
            return idx;
        }
    }
    limit
}

fn hex_preview(bytes: &[u8]) -> String {
    const MAX: usize = 32;
    let shown = bytes.len().min(MAX);
    let mut out = String::new();
    for (i, b) in bytes[..shown].iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let _ = write!(&mut out, "{b:02x}");
    }
    if bytes.len() > shown {
        out.push_str(" …");
    }
    out
}

fn snapshot_diff_message(a: &[u8], b: &[u8], context: &str) -> String {
    if a == b {
        return String::new();
    }

    let fields_a = match parse_snapshot_fields(a) {
        Ok(fields) => fields,
        Err(e) => {
            return format!(
                "{context}: failed to parse first snapshot: {e}\nlen={}\nhead={}",
                a.len(),
                hex_preview(&a[..a.len().min(64)])
            );
        }
    };
    let fields_b = match parse_snapshot_fields(b) {
        Ok(fields) => fields,
        Err(e) => {
            return format!(
                "{context}: failed to parse second snapshot: {e}\nlen={}\nhead={}",
                b.len(),
                hex_preview(&b[..b.len().min(64)])
            );
        }
    };

    if fields_a.len() != fields_b.len() {
        return format!(
            "{context}: snapshot field count mismatch ({} vs {})",
            fields_a.len(),
            fields_b.len()
        );
    }

    for (fa, fb) in fields_a.iter().zip(fields_b.iter()) {
        if fa.tag != fb.tag {
            return format!(
                "{context}: snapshot tag mismatch ({} vs {})",
                fa.tag, fb.tag
            );
        }
        if fa.data == fb.data {
            continue;
        }
        let diff = first_diff_idx(fa.data, fb.data);
        let start = diff.saturating_sub(16);
        let end = (diff + 16).min(fa.data.len().min(fb.data.len()));
        return format!(
            "{context}: snapshot differs at tag {} (len {} vs {}, first diff @ {})\nA[..]={}\nB[..]={}",
            fa.tag,
            fa.data.len(),
            fb.data.len(),
            diff,
            hex_preview(&fa.data[start..end]),
            hex_preview(&fb.data[start..end])
        );
    }

    let diff = first_diff_idx(a, b);
    let start = diff.saturating_sub(16);
    let end = (diff + 16).min(a.len().min(b.len()));
    format!(
        "{context}: snapshots have identical parsed fields but bytes differ (len {} vs {}, first diff @ {})\nA[..]={}\nB[..]={}",
        a.len(),
        b.len(),
        diff,
        hex_preview(&a[start..end]),
        hex_preview(&b[start..end])
    )
}

fn write_u32(mem: &mut [u8], addr: u32, value: u32) {
    let addr = addr as usize;
    mem[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_bytes(mem: &mut [u8], addr: u32, bytes: &[u8]) {
    let addr = addr as usize;
    mem[addr..addr + bytes.len()].copy_from_slice(bytes);
}

fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
    let max_len_field = if max_len == 0 {
        0x7FFu32
    } else {
        (max_len as u32).saturating_sub(1)
    };
    (pid as u32)
        | ((addr as u32) << TD_TOKEN_DEVADDR_SHIFT)
        | ((ep as u32) << TD_TOKEN_ENDPT_SHIFT)
        | (if toggle { TD_TOKEN_D } else { 0 })
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT)
}

fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

fn setup_webusb_control_in_frame_list(mem: &mut [u8]) -> u32 {
    // Layout (all 16-byte aligned).
    let fl_base = 0x1000;
    let qh_addr = 0x2000;
    let setup_td = qh_addr + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let _data_buf = setup_buf + 0x10;

    for i in 0..1024u32 {
        write_u32(mem, fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }

    // QH: head=terminate, element=SETUP TD.
    write_u32(mem, qh_addr + 0x00, LINK_PTR_T);
    write_u32(mem, qh_addr + 0x04, setup_td);

    // Setup packet: GET_DESCRIPTOR (device), 8 bytes.
    let setup_packet = [
        0x80, // bmRequestType: device-to-host | standard | device
        0x06, // bRequest: GET_DESCRIPTOR
        0x00, 0x01, // wValue: (DEVICE=1)<<8 | index 0
        0x00, 0x00, // wIndex
        0x08, 0x00, // wLength: 8
    ];
    write_bytes(mem, setup_buf, &setup_packet);

    // SETUP TD.
    write_u32(mem, setup_td + 0x00, data_td);
    write_u32(mem, setup_td + 0x04, td_ctrl(true, false));
    write_u32(mem, setup_td + 0x08, td_token(0x2D, 0, 0, false, 8));
    write_u32(mem, setup_td + 0x0C, setup_buf);

    // DATA IN TD (will NAK until host completion is pushed).
    write_u32(mem, data_td + 0x00, status_td);
    write_u32(mem, data_td + 0x04, td_ctrl(true, false));
    write_u32(mem, data_td + 0x08, td_token(0x69, 0, 0, true, 8));
    write_u32(mem, data_td + 0x0C, setup_buf + 0x10);

    // STATUS OUT TD (0-length, IOC).
    write_u32(mem, status_td + 0x00, LINK_PTR_T);
    write_u32(mem, status_td + 0x04, td_ctrl(true, true));
    write_u32(mem, status_td + 0x08, td_token(0xE1, 0, 0, true, 0));
    write_u32(mem, status_td + 0x0C, 0);

    fl_base
}

fn setup_webhid_set_report_control_out_frame_list(mem: &mut [u8], payload: &[u8]) -> u32 {
    let fl_base = 0x1000;
    let qh_addr = 0x2000;
    let setup_td = qh_addr + 0x20;
    let data_td = setup_td + 0x20;
    let status_td = data_td + 0x20;
    let setup_buf = status_td + 0x20;
    let data_buf = setup_buf + 0x10;

    for i in 0..1024u32 {
        write_u32(mem, fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }

    write_u32(mem, qh_addr + 0x00, LINK_PTR_T);
    write_u32(mem, qh_addr + 0x04, setup_td);

    // HID SET_REPORT(Output, reportId=0), wLength = payload length.
    let w_length = u16::try_from(payload.len()).expect("payload len fits in u16");
    let setup_packet = [
        0x21, // bmRequestType: host-to-device | class | interface
        0x09, // bRequest: SET_REPORT
        0x00,
        0x02, // wValue: reportId=0, reportType=2 (Output)
        0x00,
        0x00, // wIndex: interface 0
        (w_length & 0xff) as u8,
        (w_length >> 8) as u8,
    ];
    write_bytes(mem, setup_buf, &setup_packet);
    write_bytes(mem, data_buf, payload);

    // SETUP TD.
    write_u32(mem, setup_td + 0x00, data_td);
    write_u32(mem, setup_td + 0x04, td_ctrl(true, false));
    write_u32(mem, setup_td + 0x08, td_token(0x2D, 0, 0, false, 8));
    write_u32(mem, setup_td + 0x0C, setup_buf);

    // DATA OUT TD.
    write_u32(mem, data_td + 0x00, status_td);
    write_u32(mem, data_td + 0x04, td_ctrl(true, false));
    write_u32(
        mem,
        data_td + 0x08,
        td_token(0xE1, 0, 0, true, payload.len()),
    );
    write_u32(mem, data_td + 0x0C, data_buf);

    // STATUS IN TD (ZLP, IOC).
    write_u32(mem, status_td + 0x00, LINK_PTR_T);
    write_u32(mem, status_td + 0x04, td_ctrl(true, true));
    write_u32(mem, status_td + 0x08, td_token(0x69, 0, 0, true, 0));
    write_u32(mem, status_td + 0x0C, 0);

    fl_base
}

fn load_mouse_collections_json() -> JsValue {
    let collections: Vec<HidCollectionInfo> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/hid/webhid_normalized_mouse.json"
    )))
    .expect("deserialize webhid_normalized_mouse.json fixture");
    serde_wasm_bindgen::to_value(&collections).expect("collections to_value")
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_is_deterministic() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20_000);

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    // Attach an external hub plus a couple devices so the snapshot contains nested state.
    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to_value");
    // Hub ports 1..=3 are reserved for synthetic HID devices, so allocate WebHID passthrough on
    // ports 4+ (and ensure the hub has enough downstream ports).
    rt.webhid_attach_hub(hub_path, Some(5))
        .expect("attach external hub");

    let collections_json = load_mouse_collections_json();
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

    rt.webusb_attach(Some(1)).expect("attach WebUSB");

    let a = rt.save_state();
    let b = rt.save_state();
    if a != b {
        let msg = snapshot_diff_message(&a, &b, "save_state must be deterministic");
        drop(a);
        drop(b);
        drop(rt);
        panic!("{msg}");
    }
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_truncates_webhid_product_string() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20_000);

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    let long_name = "😀".repeat(1000);
    let collections_json = load_mouse_collections_json();
    rt.webhid_attach(
        1,
        0x1234,
        0x0001,
        Some(long_name),
        collections_json,
        Some(0),
    )
    .expect("webhid_attach ok");

    let snapshot = rt.save_state();
    let r = SnapshotReader::parse(&snapshot, *b"UHRT").expect("parse UhciRuntime snapshot");
    let webhid_bytes = r.bytes(6).expect("expected WebHID list tag");

    let mut d = Decoder::new(webhid_bytes);
    let count = d.u32().expect("decode webhid count") as usize;
    assert_eq!(count, 1);
    let rec_len = d.u32().expect("decode record len") as usize;
    let rec = d.bytes(rec_len).expect("decode record bytes");
    d.finish().expect("decode webhid list");

    let mut rd = Decoder::new(rec);
    let _device_id = rd.u32().expect("device id");
    let _loc_kind = rd.u8().expect("loc kind");
    let _loc_port = rd.u8().expect("loc port");
    let _vendor_id = rd.u16().expect("vendor id");
    let _product_id = rd.u16().expect("product id");
    let name_len = rd.u32().expect("product name len") as usize;
    let name_bytes = rd.bytes(name_len).expect("product name bytes");
    let name = std::str::from_utf8(name_bytes).expect("product name utf8");

    let expected = "😀".repeat(63);
    assert_eq!(name_len, expected.as_bytes().len());
    assert_eq!(name, expected.as_str());
    assert_eq!(name.encode_utf16().count(), 126);

    let report_descriptor_len = rd.u32().expect("report descriptor len") as usize;
    rd.bytes(report_descriptor_len)
        .expect("report descriptor bytes");
    rd.bool().expect("has interrupt out");
    let dev_state_len = rd.u32().expect("device state len") as usize;
    rd.bytes(dev_state_len).expect("device state bytes");
    rd.finish().expect("finish record decoder");
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_roundtrip_preserves_irq_and_registers() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x50_000);
    let fl_base = {
        // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting
        // at `guest_base`.
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };
        setup_webusb_control_in_frame_list(guest)
    };

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");
    rt.webusb_attach(Some(1)).expect("attach WebUSB");

    rt.port_write(REG_FRBASEADD, 4, fl_base);
    rt.port_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    rt.port_write(REG_PORTSC2, 2, PORTSC_PED as u32);
    rt.port_write(REG_USBCMD, 2, USBCMD_RUN as u32);

    rt.step_frame();

    let drained = rt.webusb_drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let id = match actions.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action, got {other:?}"),
    };

    // `UhciRuntime` expects WebUSB completions to follow the canonical TypeScript wire contract
    // (binary payloads as `Uint8Array`, not `number[]`). Build the completion object manually
    // instead of relying on `serde_wasm_bindgen`'s `Vec<u8>` encoding.
    let completion = js_sys::Object::new();
    Reflect::set(
        &completion,
        &JsValue::from_str("kind"),
        &JsValue::from_str("controlIn"),
    )
    .expect("set completion kind");
    Reflect::set(
        &completion,
        &JsValue::from_str("id"),
        &JsValue::from_f64(f64::from(id)),
    )
    .expect("set completion id");
    Reflect::set(
        &completion,
        &JsValue::from_str("status"),
        &JsValue::from_str("success"),
    )
    .expect("set completion status");
    let data = Uint8Array::from(vec![0u8; 8].as_slice());
    Reflect::set(&completion, &JsValue::from_str("data"), data.as_ref())
        .expect("set completion data");
    rt.webusb_push_completion(completion.into()).unwrap();
    rt.step_frame();

    assert!(rt.irq_level());

    let usbcmd = rt.port_read(REG_USBCMD, 2);
    let usbsts = rt.port_read(REG_USBSTS, 2);
    let usbintr = rt.port_read(REG_USBINTR, 2);
    let frnum = rt.port_read(REG_FRNUM, 2);
    let frbaseadd = rt.port_read(REG_FRBASEADD, 4);

    let snapshot = rt.save_state();

    let mut rt2 = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime #2");
    if let Err(err) = rt2.load_state(&snapshot) {
        drop(snapshot);
        drop(rt2);
        panic!("load_state ok: {err:?}");
    }

    assert_eq!(rt2.port_read(REG_USBCMD, 2), usbcmd);
    assert_eq!(rt2.port_read(REG_USBSTS, 2), usbsts);
    assert_eq!(rt2.port_read(REG_USBINTR, 2), usbintr);
    assert_eq!(rt2.port_read(REG_FRNUM, 2), frnum);
    assert_eq!(rt2.port_read(REG_FRBASEADD, 4), frbaseadd);
    assert!(rt2.irq_level());

    // Clearing USBSTS.USBINT should deassert the IRQ line.
    rt2.port_write(REG_USBSTS, 2, USBSTS_USBINT as u32);
    assert!(!rt2.irq_level());
}

#[wasm_bindgen_test]
fn uhci_runtime_restore_clears_webusb_host_state_and_allows_retry() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x50_000);
    let fl_base = {
        // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting
        // at `guest_base`.
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };
        setup_webusb_control_in_frame_list(guest)
    };

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");
    rt.webusb_attach(Some(1)).expect("attach WebUSB");

    rt.port_write(REG_FRBASEADD, 4, fl_base);
    rt.port_write(REG_PORTSC2, 2, PORTSC_PED as u32);
    rt.port_write(REG_USBCMD, 2, USBCMD_RUN as u32);

    // First attempt should emit a host action.
    rt.step_frame();
    let drained = rt.webusb_drain_actions().expect("drain_actions ok");
    let actions: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained).expect("deserialize UsbHostAction[]");
    let first_id = match actions.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action, got {other:?}"),
    };

    // With the action drained but no completion, the device is still inflight and should not emit
    // duplicates while the TD retries.
    rt.step_frame();
    let drained_again = rt.webusb_drain_actions().expect("drain_actions ok");
    let actions_again: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_again).expect("deserialize UsbHostAction[]");
    assert!(
        actions_again.is_empty(),
        "expected inflight WebUSB transfer to suppress duplicate actions"
    );

    let snapshot = rt.save_state();

    let mut rt2 = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime #2");
    if let Err(err) = rt2.load_state(&snapshot) {
        drop(snapshot);
        drop(rt2);
        panic!("load_state ok: {err:?}");
    }

    let drained_after_restore = rt2.webusb_drain_actions().expect("drain_actions ok");
    let actions_after_restore: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_after_restore).expect("deserialize UsbHostAction[]");
    assert!(
        actions_after_restore.is_empty(),
        "expected WebUSB host queues to be cleared on restore"
    );

    // The guest TD remains active and is retried; with host state cleared, the next retry should
    // re-emit host actions.
    rt2.step_frame();
    let drained_retry = rt2.webusb_drain_actions().expect("drain_actions ok");
    let actions_retry: Vec<UsbHostAction> =
        serde_wasm_bindgen::from_value(drained_retry).expect("deserialize UsbHostAction[]");
    let retry_id = match actions_retry.first() {
        Some(UsbHostAction::ControlIn { id, .. }) => *id,
        other => panic!("expected ControlIn action after restore, got {other:?}"),
    };
    assert_ne!(
        retry_id, first_id,
        "expected re-emitted host action to allocate a new id after restore"
    );
}

#[wasm_bindgen_test]
fn uhci_runtime_snapshot_restores_external_hub_and_allows_webhid_attach_at_path() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20_000);

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");

    let hub_path = serde_wasm_bindgen::to_value(&vec![0u32]).expect("hub path to_value");
    // Hub ports 1..=3 are reserved for synthetic HID devices, so allocate WebHID passthrough on
    // ports 4+ (and ensure the hub has enough downstream ports).
    rt.webhid_attach_hub(hub_path, Some(5))
        .expect("attach external hub");

    let collections_json = load_mouse_collections_json();
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

    let snapshot = rt.save_state();

    let mut rt2 = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime #2");
    if let Err(err) = rt2.load_state(&snapshot) {
        drop(snapshot);
        drop(rt2);
        panic!("load_state ok: {err:?}");
    }

    // Sanity: restored runtime snapshot should still include external hub state.
    let after = rt2.save_state();
    let r = SnapshotReader::parse(&after, *b"UHRT").expect("parse restored runtime snapshot");
    assert!(r.bytes(4).is_some(), "expected hub state tag to be present");

    // Root port 0 should still report "connected" because the external hub is restored.
    let portsc1 = rt2.port_read(REG_PORTSC1, 2) as u16;
    assert_ne!(portsc1 & PORTSC_CCS, 0);

    // `webhid_attach_at_path` should succeed without requiring another `webhid_attach_hub` call.
    let path2 = serde_wasm_bindgen::to_value(&vec![0u32, 5u32]).expect("path2 to_value");
    rt2.webhid_attach_at_path(
        2,
        0x1234,
        0x0002,
        Some("Test HID #2".to_string()),
        collections_json,
        path2,
    )
    .unwrap_or_else(|err| {
        drop(rt2);
        panic!("attach WebHID device #2 after restore: {err:?}");
    });
}

#[wasm_bindgen_test]
fn uhci_runtime_restore_preserves_webhid_device_and_allows_set_report() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x20_000);

    let collections_json = load_mouse_collections_json();

    let mut rt = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime");
    let port = rt
        .webhid_attach(
            1,
            0x1234,
            0x0001,
            Some("Test HID".to_string()),
            collections_json.clone(),
            None,
        )
        .expect("attach WebHID device");
    assert_eq!(port, 0);

    let snapshot = rt.save_state();

    let mut rt2 = UhciRuntime::new(guest_base, guest_size).expect("new UhciRuntime #2");
    if let Err(err) = rt2.load_state(&snapshot) {
        drop(snapshot);
        drop(rt2);
        panic!("load_state ok: {err:?}");
    }

    let payload = [0x11u8, 0x22, 0x33];
    let fl_base = {
        // Safety: `alloc_guest_region_bytes` reserves `guest_size` bytes in linear memory starting
        // at `guest_base`.
        let guest =
            unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };
        setup_webhid_set_report_control_out_frame_list(guest, &payload)
    };

    rt2.port_write(REG_FRBASEADD, 4, fl_base);
    rt2.port_write(REG_PORTSC1, 2, PORTSC_PED as u32);
    rt2.port_write(REG_USBCMD, 2, USBCMD_RUN as u32);
    rt2.step_frame();

    let reports = Array::from(&rt2.webhid_drain_output_reports());
    assert_eq!(reports.length(), 1, "expected one output report");

    let report = reports.get(0);
    let device_id = Reflect::get(&report, &JsValue::from_str("deviceId"))
        .unwrap()
        .as_f64()
        .unwrap() as u32;
    assert_eq!(device_id, 1);

    let report_type = Reflect::get(&report, &JsValue::from_str("reportType"))
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(report_type, "output");

    let report_id = Reflect::get(&report, &JsValue::from_str("reportId"))
        .unwrap()
        .as_f64()
        .unwrap() as u32;
    assert_eq!(report_id, 0);

    let data_val = Reflect::get(&report, &JsValue::from_str("data")).unwrap();
    let data_u8 = Uint8Array::new(&data_val);
    let mut data = vec![0u8; data_u8.length() as usize];
    data_u8.copy_to(&mut data);
    assert_eq!(data.as_slice(), payload.as_slice());
}
