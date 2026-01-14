#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

fn unique_path(prefix: &str, ext: &str) -> String {
    let now = js_sys::Date::now() as u64;
    format!("tests/{prefix}-{now}.{ext}")
}

fn js_error_message(err: JsValue) -> String {
    if let Ok(e) = err.clone().dyn_into::<js_sys::Error>() {
        return e.message().into();
    }
    err.as_string().unwrap_or_else(|| format!("{err:?}"))
}

fn is_opfs_unavailable_message(msg: &str) -> bool {
    msg.contains("backend not supported") || msg.contains("backend unavailable")
}

fn should_skip_opfs_message(msg: &str) -> bool {
    // Keep in sync with other OPFS-skipping tests; these are `aero_storage::DiskError` `Display`
    // strings (which can appear inside our OPFS error wrappers).
    msg.contains("backend not supported")
        || msg.contains("backend unavailable")
        || msg.contains("storage quota exceeded")
}

#[wasm_bindgen_test(async)]
async fn aerospar_opfs_create_unavailable_error_includes_operation_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        // This test only checks the "OPFS unavailable" path (e.g. Node, main thread, or browsers
        // without sync access handles).
        return;
    }

    let path = unique_path("opfs-error-aerospar-create", "aerospar");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .set_disk_aerospar_opfs_create(path.clone(), 1024 * 1024, 32 * 1024)
        .await
        .expect_err("expected OPFS-backed disk create to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.set_disk_aerospar_opfs_create"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&path),
        "expected OPFS path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn aerospar_opfs_open_unavailable_error_includes_operation_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let path = unique_path("opfs-error-aerospar-open", "aerospar");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .set_disk_aerospar_opfs_open(path.clone())
        .await
        .expect_err("expected OPFS-backed disk open to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.set_disk_aerospar_opfs_open"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&path),
        "expected OPFS path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn cow_opfs_create_unavailable_error_includes_operation_and_both_paths_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let base_path = unique_path("opfs-error-cow-base", "img");
    let overlay_path = unique_path("opfs-error-cow-overlay", "aerospar");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .set_disk_cow_opfs_create(base_path.clone(), overlay_path.clone(), 32 * 1024)
        .await
        .expect_err("expected OPFS-backed COW create to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.set_disk_cow_opfs_create"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&base_path),
        "expected base_path in message, got: {msg}"
    );
    assert!(
        msg.contains(&overlay_path),
        "expected overlay_path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn cow_opfs_open_unavailable_error_includes_operation_and_both_paths_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let base_path = unique_path("opfs-error-cow-open-base", "img");
    let overlay_path = unique_path("opfs-error-cow-open-overlay", "aerospar");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .set_disk_cow_opfs_open(base_path.clone(), overlay_path.clone())
        .await
        .expect_err("expected OPFS-backed COW open to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.set_disk_cow_opfs_open"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&base_path),
        "expected base_path in message, got: {msg}"
    );
    assert!(
        msg.contains(&overlay_path),
        "expected overlay_path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn install_media_iso_opfs_existing_unavailable_error_includes_operation_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let path = unique_path("opfs-error-install-media-existing", "iso");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .attach_install_media_iso_opfs_existing(path.clone())
        .await
        .expect_err("expected OPFS-backed ISO attach to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.attach_install_media_iso_opfs_existing"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&path),
        "expected OPFS path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
    assert!(
        msg.contains("storage_capabilities()"),
        "expected storage_capabilities() tip in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn ide_secondary_master_iso_opfs_existing_unavailable_error_includes_operation_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let path = unique_path("opfs-error-ide-secondary-master-existing", "iso");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .attach_ide_secondary_master_iso_opfs_existing(path.clone())
        .await
        .expect_err("expected OPFS-backed ISO attach to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.attach_ide_secondary_master_iso_opfs_existing"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&path),
        "expected OPFS path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
    assert!(
        msg.contains("storage_capabilities()"),
        "expected storage_capabilities() tip in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn snapshot_full_to_opfs_unavailable_error_includes_operation_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let path = unique_path("opfs-error-snapshot-full", "aerosnap");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .snapshot_full_to_opfs(path.clone())
        .await
        .expect_err("expected OPFS-backed snapshot save to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    assert!(
        msg.contains("Machine.snapshot_full_to_opfs"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&path),
        "expected OPFS path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}

#[wasm_bindgen_test(async)]
async fn primary_hdd_opfs_cow_unavailable_error_includes_operation_and_both_paths_and_hint() {
    if aero_opfs::opfs_sync_access_supported() {
        return;
    }

    let base_path = unique_path("opfs-error-primary-hdd-base", "img");
    let overlay_path = unique_path("opfs-error-primary-hdd-overlay", "aerospar");
    let mut m = aero_wasm::Machine::new(2 * 1024 * 1024).expect("Machine::new");
    let err = m
        .set_primary_hdd_opfs_cow(base_path.clone(), overlay_path.clone(), 32 * 1024)
        .await
        .expect_err("expected OPFS-backed primary HDD COW attach to fail when OPFS sync access unavailable");

    let msg = js_error_message(err);
    if !is_opfs_unavailable_message(&msg) {
        if should_skip_opfs_message(&msg) {
            return;
        }
        panic!("unexpected error (expected NotSupported/BackendUnavailable): {msg}");
    }
    assert!(
        msg.contains("Machine.set_primary_hdd_opfs_cow"),
        "expected operation name in message, got: {msg}"
    );
    assert!(
        msg.contains(&base_path),
        "expected base_path in message, got: {msg}"
    );
    assert!(
        msg.contains(&overlay_path),
        "expected overlay_path in message, got: {msg}"
    );
    assert!(
        msg.contains("DedicatedWorker"),
        "expected DedicatedWorker hint in message, got: {msg}"
    );
}
