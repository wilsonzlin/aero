#![cfg(target_arch = "wasm32")]

use aero_wasm::Machine;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

use js_sys::{Array, Reflect};

// OPFS sync access handles are only available from a DedicatedWorker.
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_worker);

fn unique_opfs_path(prefix: &str) -> String {
    let now = js_sys::Date::now() as u64;
    let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
    format!("{prefix}-{now:x}-{rand:x}.img")
}

#[wasm_bindgen_test(async)]
async fn opfs_attach_can_opt_in_to_setting_snapshot_overlay_refs() {
    // wasm-bindgen-test can run in environments where OPFS is unavailable (e.g. Node). In those
    // cases, skip the test.
    if !aero_opfs::platform::storage::opfs::is_opfs_supported() {
        return;
    }

    let path = unique_opfs_path("aero-wasm-test-disk");

    let mut machine = Machine::new(2 * 1024 * 1024).expect("Machine::new");

    let progress = js_sys::Function::new_no_args("");
    let attach_res = machine
        .set_disk_opfs_with_progress_and_set_overlay_ref(path.clone(), true, 4096, progress)
        .await;

    if let Err(err) = attach_res {
        let msg = err
            .as_string()
            .or_else(|| {
                err.dyn_ref::<js_sys::Error>()
                    .map(|e| String::from(e.message()))
            })
            .unwrap_or_else(|| format!("{err:?}"));
        // Treat OPFS unavailability (e.g. missing sync access handle support) as a skip.
        if msg.contains("OPFS")
            || msg.contains("backend unavailable")
            || msg.contains("not supported")
        {
            return;
        }
        panic!("set_disk_opfs_with_progress_and_set_overlay_ref failed unexpectedly: {msg}");
    }

    // Snapshot should include a DISKS section carrying the overlay refs we set.
    let snap = machine.snapshot_full().expect("snapshot_full");
    machine.restore_snapshot(&snap).expect("restore_snapshot");

    let overlays = machine.take_restored_disk_overlays();
    assert!(
        !overlays.is_null(),
        "expected snapshot restore to surface disk overlay refs"
    );

    let arr = Array::from(&overlays);
    let mut found = false;
    for i in 0..arr.length() {
        let entry = arr.get(i);
        let disk_id = Reflect::get(&entry, &JsValue::from_str("disk_id"))
            .expect("disk_id present")
            .as_f64()
            .expect("disk_id is number") as u32;
        if disk_id != Machine::disk_id_primary_hdd() {
            continue;
        }

        let base_image = Reflect::get(&entry, &JsValue::from_str("base_image"))
            .expect("base_image present")
            .as_string()
            .expect("base_image is string");
        let overlay_image = Reflect::get(&entry, &JsValue::from_str("overlay_image"))
            .expect("overlay_image present")
            .as_string()
            .expect("overlay_image is string");

        assert_eq!(base_image, path);
        assert_eq!(overlay_image, "");
        found = true;
        break;
    }

    assert!(
        found,
        "expected DISKS entry for primary HDD (disk_id={})",
        Machine::disk_id_primary_hdd()
    );
}

#[wasm_bindgen_test(async)]
async fn opfs_attach_ide_primary_master_can_opt_in_to_setting_snapshot_overlay_refs() {
    if !aero_opfs::platform::storage::opfs::is_opfs_supported() {
        return;
    }

    let path = unique_opfs_path("aero-wasm-test-ide-primary-master");

    let mut machine = Machine::new(2 * 1024 * 1024).expect("Machine::new");

    let attach_res = machine
        .attach_ide_primary_master_disk_opfs_and_set_overlay_ref(path.clone(), true, 4096)
        .await;

    if let Err(err) = attach_res {
        let msg = err
            .as_string()
            .or_else(|| err.dyn_ref::<js_sys::Error>().and_then(|e| e.message().as_string()))
            .unwrap_or_else(|| format!("{err:?}"));
        if msg.contains("OPFS")
            || msg.contains("backend unavailable")
            || msg.contains("not supported")
        {
            return;
        }
        panic!(
            "attach_ide_primary_master_disk_opfs_and_set_overlay_ref failed unexpectedly: {msg}"
        );
    }

    let snap = machine.snapshot_full().expect("snapshot_full");
    machine.restore_snapshot(&snap).expect("restore_snapshot");

    let overlays = machine.take_restored_disk_overlays();
    assert!(
        !overlays.is_null(),
        "expected snapshot restore to surface disk overlay refs"
    );

    let arr = Array::from(&overlays);
    let mut found = false;
    for i in 0..arr.length() {
        let entry = arr.get(i);
        let disk_id = Reflect::get(&entry, &JsValue::from_str("disk_id"))
            .expect("disk_id present")
            .as_f64()
            .expect("disk_id is number") as u32;
        if disk_id != Machine::disk_id_ide_primary_master() {
            continue;
        }

        let base_image = Reflect::get(&entry, &JsValue::from_str("base_image"))
            .expect("base_image present")
            .as_string()
            .expect("base_image is string");
        let overlay_image = Reflect::get(&entry, &JsValue::from_str("overlay_image"))
            .expect("overlay_image present")
            .as_string()
            .expect("overlay_image is string");

        assert_eq!(base_image, path);
        assert_eq!(overlay_image, "");
        found = true;
        break;
    }

    assert!(
        found,
        "expected DISKS entry for IDE primary master (disk_id={})",
        Machine::disk_id_ide_primary_master()
    );
}

#[wasm_bindgen_test(async)]
async fn opfs_attach_install_media_iso_can_opt_in_to_setting_snapshot_overlay_refs() {
    if !aero_opfs::platform::storage::opfs::is_opfs_supported() {
        return;
    }

    let path = unique_opfs_path("aero-wasm-test-install-media");

    // Seed an OPFS file so the `_existing` ISO attach API can open it. ISO backends require a
    // 2048-byte multiple.
    match aero_opfs::OpfsBackend::open(&path, true, 2048).await {
        Ok(mut backend) => {
            let _ = backend.close();
        }
        Err(aero_opfs::DiskError::NotSupported(_))
        | Err(aero_opfs::DiskError::BackendUnavailable)
        | Err(aero_opfs::DiskError::QuotaExceeded) => return,
        Err(e) => panic!("failed to seed OPFS ISO image: {e:?}"),
    }

    let mut machine = Machine::new(2 * 1024 * 1024).expect("Machine::new");

    let attach_res = machine
        .attach_install_media_iso_opfs_existing_and_set_overlay_ref(path.clone())
        .await;

    if let Err(err) = attach_res {
        let msg = err
            .as_string()
            .or_else(|| err.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())))
            .unwrap_or_else(|| format!("{err:?}"));
        panic!("attach_install_media_iso_opfs_existing_and_set_overlay_ref failed unexpectedly: {msg}");
    }

    let snap = machine.snapshot_full().expect("snapshot_full");
    machine.restore_snapshot(&snap).expect("restore_snapshot");

    let overlays = machine.take_restored_disk_overlays();
    assert!(
        !overlays.is_null(),
        "expected snapshot restore to surface disk overlay refs"
    );

    let arr = Array::from(&overlays);
    let mut found = false;
    for i in 0..arr.length() {
        let entry = arr.get(i);
        let disk_id = Reflect::get(&entry, &JsValue::from_str("disk_id"))
            .expect("disk_id present")
            .as_f64()
            .expect("disk_id is number") as u32;
        if disk_id != Machine::disk_id_install_media() {
            continue;
        }

        let base_image = Reflect::get(&entry, &JsValue::from_str("base_image"))
            .expect("base_image present")
            .as_string()
            .expect("base_image is string");
        let overlay_image = Reflect::get(&entry, &JsValue::from_str("overlay_image"))
            .expect("overlay_image present")
            .as_string()
            .expect("overlay_image is string");

        assert_eq!(base_image, path);
        assert_eq!(overlay_image, "");
        found = true;
        break;
    }

    assert!(
        found,
        "expected DISKS entry for install media (disk_id={})",
        Machine::disk_id_install_media()
    );
}
