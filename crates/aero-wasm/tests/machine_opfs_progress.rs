#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Function;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

// OPFS sync access handles are worker-only.
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_worker);

fn unique_opfs_path(prefix: &str) -> String {
    let now = js_sys::Date::now() as u64;
    let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
    format!("{prefix}-{now:x}-{rand:x}.img")
}

fn js_value_error_message(err: &JsValue) -> String {
    if let Some(s) = err.as_string() {
        return s;
    }
    if let Ok(e) = err.clone().dyn_into::<js_sys::Error>() {
        return e.message().into();
    }
    format!("{err:?}")
}

#[wasm_bindgen_test(async)]
async fn set_disk_opfs_with_progress_invokes_callback() {
    let path = unique_opfs_path("aero-wasm-opfs-progress");

    let progress_samples: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let progress_samples_cb = Rc::clone(&progress_samples);
    let cb = Closure::wrap(Box::new(move |p: f64| {
        progress_samples_cb.borrow_mut().push(p);
    }) as Box<dyn FnMut(f64)>);
    let cb_fn: Function = cb.as_ref().unchecked_ref::<Function>().clone();

    let mut machine = aero_wasm::Machine::new(64 * 1024 * 1024).expect("Machine::new");
    let res = machine
        .set_disk_opfs_with_progress(path.clone(), true, 512 * 1024, cb_fn)
        .await;

    match res {
        Ok(()) => {}
        Err(err) => {
            let msg = js_value_error_message(&err);
            // OPFS is not available in all wasm-bindgen-test environments (notably Node),
            // and sync access handles are worker-only. Skip the test in those cases.
            if msg.contains("backend not supported") || msg.contains("backend unavailable") {
                return;
            }
            panic!("Machine.set_disk_opfs_with_progress failed: {msg}");
        }
    }

    // Keep the closure alive for the duration of the async call (and any nested awaits).
    drop(cb);

    let samples = progress_samples.borrow();
    assert!(
        samples.len() >= 2,
        "expected progress callback to be invoked at least twice; got {samples:?}"
    );
    assert!(
        samples.iter().any(|&v| v == 0.0),
        "expected progress callback to include 0.0; got {samples:?}"
    );
    assert!(
        samples.iter().any(|&v| v == 1.0),
        "expected progress callback to include 1.0; got {samples:?}"
    );
}

#[wasm_bindgen_test(async)]
async fn attach_ide_primary_master_disk_opfs_with_progress_invokes_callback() {
    let path = unique_opfs_path("aero-wasm-opfs-progress-ide-primary-master");

    let progress_samples: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let progress_samples_cb = Rc::clone(&progress_samples);
    let cb = Closure::wrap(Box::new(move |p: f64| {
        progress_samples_cb.borrow_mut().push(p);
    }) as Box<dyn FnMut(f64)>);
    let cb_fn: Function = cb.as_ref().unchecked_ref::<Function>().clone();

    let mut machine = aero_wasm::Machine::new(64 * 1024 * 1024).expect("Machine::new");
    let res = machine
        .attach_ide_primary_master_disk_opfs_with_progress(path.clone(), true, 512 * 1024, cb_fn)
        .await;

    match res {
        Ok(()) => {}
        Err(err) => {
            let msg = js_value_error_message(&err);
            if msg.contains("backend not supported") || msg.contains("backend unavailable") {
                return;
            }
            panic!("Machine.attach_ide_primary_master_disk_opfs_with_progress failed: {msg}");
        }
    }

    drop(cb);

    let samples = progress_samples.borrow();
    assert!(
        samples.len() >= 2,
        "expected progress callback to be invoked at least twice; got {samples:?}"
    );
    assert!(
        samples.iter().any(|&v| v == 0.0),
        "expected progress callback to include 0.0; got {samples:?}"
    );
    assert!(
        samples.iter().any(|&v| v == 1.0),
        "expected progress callback to include 1.0; got {samples:?}"
    );
}
