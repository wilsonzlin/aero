#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

use aero_wasm::Machine;

wasm_bindgen_test_configure!(run_in_browser);

fn js_error_message(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if let Ok(e) = err.clone().dyn_into::<js_sys::Error>() {
        return e.message().into();
    }
    js_sys::Reflect::get(err, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("{err:?}"))
}

#[wasm_bindgen_test(async)]
async fn install_media_iso_opfs_error_mentions_worker_hint_and_storage_capabilities() {
    // This test targets the "OPFS sync access handles are unavailable" / NotSupported error path.
    // In environments where sync access handles *are* available (e.g. Chromium DedicatedWorker),
    // `attach_install_media_iso_opfs` can succeed, so skip the assertion.
    if aero_opfs::platform::storage::opfs::opfs_sync_access_supported() {
        return;
    }

    let mut machine = Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let path = "tests/opfs-install-media-error.iso".to_string();

    let err = machine
        .attach_install_media_iso_opfs(path.clone())
        .await
        .expect_err("attach_install_media_iso_opfs should fail when OPFS sync access handles are unavailable");
    let msg = js_error_message(&err);
    assert!(
        msg.contains("DedicatedWorker"),
        "expected error to mention DedicatedWorker hint; got: {msg}"
    );
    assert!(
        msg.contains("storage_capabilities()"),
        "expected error to mention storage_capabilities() tip; got: {msg}"
    );

    let err = machine
        .attach_install_media_iso_opfs_for_restore(path)
        .await
        .expect_err(
            "restore variant should also fail when OPFS sync access handles are unavailable",
        );
    let msg = js_error_message(&err);
    assert!(
        msg.contains("DedicatedWorker"),
        "expected restore error to mention DedicatedWorker hint; got: {msg}"
    );
    assert!(
        msg.contains("storage_capabilities()"),
        "expected restore error to mention storage_capabilities() tip; got: {msg}"
    );
}
