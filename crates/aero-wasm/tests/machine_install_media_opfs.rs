#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

use aero_wasm::Machine;

wasm_bindgen_test_configure!(run_in_worker);

fn unique_path(prefix: &str) -> String {
    let now = js_sys::Date::now() as u64;
    format!("tests/{prefix}-{now}.iso")
}

fn js_error_message(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if err.is_instance_of::<js_sys::Error>() {
        return err
            .clone()
            .dyn_into::<js_sys::Error>()
            .unwrap()
            .message()
            .into();
    }
    js_sys::Reflect::get(err, &JsValue::from_str("message"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| format!("{err:?}"))
}

#[wasm_bindgen_test(async)]
async fn attach_install_media_iso_opfs_works_or_is_not_supported() {
    let path = unique_path("install-media");
    let size_bytes = 2 * 2048u64;

    let mut machine = Machine::new(64 * 1024 * 1024).expect("Machine::new");

    let opfs_supported = aero_opfs::platform::storage::opfs::is_opfs_supported();
    let opfs_sync_supported = aero_opfs::platform::storage::opfs::opfs_sync_access_supported();
    if !opfs_supported || !opfs_sync_supported {
        let err = machine
            .attach_install_media_iso_opfs(path.clone())
            .await
            .expect_err("expected NotSupported-style error when OPFS sync handles are unavailable");
        assert!(
            err.is_instance_of::<js_sys::Error>(),
            "expected a JS Error; got {err:?}"
        );
        let msg = js_error_message(&err).to_lowercase();
        assert!(
            msg.contains("opfs") || msg.contains("not supported") || msg.contains("unavailable"),
            "unexpected error message: {msg}"
        );

        let _ = machine
            .attach_install_media_iso_opfs_for_restore(path)
            .await
            .expect_err("expected NotSupported-style error for restore variant too");
        return;
    }

    // OPFS sync access handles appear to be available. Try to create a tiny ISO-sized file in OPFS
    // so we can exercise the happy path.
    match aero_opfs::OpfsBackend::open(&path, true, size_bytes).await {
        Ok(mut backend) => {
            // Ensure the file exists and has the desired size.
            backend.flush().unwrap();
        }
        Err(aero_opfs::DiskError::QuotaExceeded)
        | Err(aero_opfs::DiskError::BackendUnavailable) => {
            return;
        }
        Err(aero_opfs::DiskError::NotSupported(_)) => return,
        Err(e) => panic!("unexpected OPFS open error: {e:?}"),
    }

    machine
        .attach_install_media_iso_opfs(path.clone())
        .await
        .expect("attach_install_media_iso_opfs");
    machine
        .attach_install_media_iso_opfs_for_restore(path)
        .await
        .expect("attach_install_media_iso_opfs_for_restore");
}

#[wasm_bindgen_test(async)]
async fn attach_install_media_iso_opfs_existing_works_or_is_not_supported() {
    let path = unique_path("install-media-existing");
    let size_bytes = 2 * 2048u64;

    let mut machine = Machine::new(64 * 1024 * 1024).expect("Machine::new");

    let opfs_supported = aero_opfs::platform::storage::opfs::is_opfs_supported();
    let opfs_sync_supported = aero_opfs::platform::storage::opfs::opfs_sync_access_supported();
    if !opfs_supported || !opfs_sync_supported {
        let err = machine
            .attach_install_media_iso_opfs_existing(path.clone())
            .await
            .expect_err("expected NotSupported-style error when OPFS sync handles are unavailable");
        assert!(
            err.is_instance_of::<js_sys::Error>(),
            "expected a JS Error; got {err:?}"
        );
        let msg = js_error_message(&err).to_lowercase();
        assert!(
            msg.contains("opfs") || msg.contains("not supported") || msg.contains("unavailable"),
            "unexpected error message: {msg}"
        );
        return;
    }

    // OPFS sync access handles appear to be available. Try to create a tiny ISO-sized file in OPFS
    // so we can exercise the happy path.
    match aero_opfs::OpfsBackend::open(&path, true, size_bytes).await {
        Ok(mut backend) => {
            // Ensure the file exists and has the desired size.
            backend.flush().unwrap();
        }
        Err(aero_opfs::DiskError::QuotaExceeded)
        | Err(aero_opfs::DiskError::BackendUnavailable) => {
            return;
        }
        Err(aero_opfs::DiskError::NotSupported(_)) => return,
        Err(e) => panic!("unexpected OPFS open error: {e:?}"),
    }

    machine
        .attach_install_media_iso_opfs_existing(path)
        .await
        .expect("attach_install_media_iso_opfs_existing");
}
