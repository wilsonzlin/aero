use crate::{Result, StorageError};
use futures_channel::oneshot;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// Yields to the browser event loop.
///
/// Used between flush chunks so large flushes don't monopolize the worker.
pub async fn yield_to_event_loop() {
    // Prefer `setTimeout(0)` over a resolved Promise: awaiting a microtask can
    // still monopolize the worker and starve message handling. A macrotask yield
    // gives the event loop a chance to process other tasks in between flush
    // chunks.
    let (send, recv) = oneshot::channel::<()>();
    let send = std::rc::Rc::new(std::cell::RefCell::new(Some(send)));

    let cb_send = send.clone();
    let cb = Closure::once(move || {
        if let Some(send) = cb_send.borrow_mut().take() {
            let _ = send.send(());
        }
    });

    let scheduled = if let Some(window) = web_sys::window() {
        window.set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 0)
    } else {
        let global = js_sys::global();
        let scope: web_sys::WorkerGlobalScope = match global.dyn_into() {
            Ok(scope) => scope,
            Err(_) => {
                // Fall back to a microtask yield if we can't determine the scope.
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(
                    &JsValue::UNDEFINED,
                ))
                .await;
                return;
            }
        };
        scope.set_timeout_with_callback_and_timeout_and_arguments_0(cb.as_ref().unchecked_ref(), 0)
    };

    if scheduled.is_err() {
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED))
            .await;
        return;
    }

    let _ = recv.await;
}

fn factory() -> Result<web_sys::IdbFactory> {
    // Support both main-thread Window and DedicatedWorkerGlobalScope.
    if let Some(window) = web_sys::window() {
        let idb = window.indexed_db().map_err(StorageError::from)?;
        return idb.ok_or(StorageError::IndexedDbUnavailable);
    }

    let global = js_sys::global();
    let scope: web_sys::WorkerGlobalScope = global
        .dyn_into()
        .map_err(|_| StorageError::IndexedDbUnavailable)?;
    let idb = scope.indexed_db().map_err(StorageError::from)?;
    idb.ok_or(StorageError::IndexedDbUnavailable)
}

pub async fn open_database_with_schema<F>(
    name: &str,
    version: u32,
    mut upgrade: F,
) -> Result<web_sys::IdbDatabase>
where
    F: FnMut(&web_sys::IdbDatabase, u32, u32) -> std::result::Result<(), JsValue> + 'static,
{
    let factory = factory()?;
    let request = factory
        .open_with_u32(name, version)
        .map_err(StorageError::from)?;

    let upgrade_error: std::rc::Rc<std::cell::RefCell<Option<JsValue>>> = Default::default();
    let upgrade_error_cb = upgrade_error.clone();

    let target_version = version;
    let upgrade_closure = Closure::wrap(Box::new(move |event: web_sys::IdbVersionChangeEvent| {
        let Some(target) = event.target() else {
            return;
        };

        let request: web_sys::IdbOpenDbRequest = match target.dyn_into() {
            Ok(req) => req,
            Err(_) => return,
        };
        let db: web_sys::IdbDatabase = match request.result() {
            Ok(val) => match val.dyn_into() {
                Ok(db) => db,
                Err(_) => return,
            },
            Err(_) => return,
        };

        let old_version = event.old_version() as u32;
        let new_version = event
            .new_version()
            .map(|v| v as u32)
            .unwrap_or(target_version);

        if let Err(err) = upgrade(&db, old_version, new_version) {
            *upgrade_error_cb.borrow_mut() = Some(err);
            // Abort the versionchange transaction so the open fails.
            if let Some(tx) = request.transaction() {
                let _ = tx.abort();
            }
        }
    }) as Box<dyn FnMut(_)>);
    request.set_onupgradeneeded(Some(upgrade_closure.as_ref().unchecked_ref()));

    let db_value = await_open_request(request).await?;
    // Keep the upgrade callback alive until the open request resolves; otherwise
    // the compiler may drop it before the `.await`, and the browser will call
    // into freed Wasm memory when `onupgradeneeded` fires.
    drop(upgrade_closure);
    if let Some(err) = upgrade_error.borrow_mut().take() {
        return Err(StorageError::Js(err));
    }

    db_value
        .dyn_into::<web_sys::IdbDatabase>()
        .map_err(StorageError::from)
}

pub async fn delete_database(name: &str) -> Result<()> {
    let factory = factory()?;
    let request = factory.delete_database(name).map_err(StorageError::from)?;
    let _ = await_open_request(request).await?;
    Ok(())
}

async fn await_open_request(request: web_sys::IdbOpenDbRequest) -> Result<JsValue> {
    let (send, recv) = oneshot::channel::<std::result::Result<JsValue, StorageError>>();
    let send = std::rc::Rc::new(std::cell::RefCell::new(Some(send)));

    let success_send = send.clone();
    let success_req = request.clone();
    let success_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = success_send.borrow_mut().take() {
            let res = success_req.result().map_err(StorageError::from);
            let _ = send.send(res);
        }
    });

    let error_send = send.clone();
    let error_req = request.clone();
    let error_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = error_send.borrow_mut().take() {
            let err = match error_req.error() {
                Ok(Some(ex)) => StorageError::from_dom_exception(&ex),
                Ok(None) => StorageError::Js(JsValue::from_str("indexeddb open request failed")),
                Err(js) => StorageError::Js(js),
            };
            let _ = send.send(Err(err));
        }
    });

    request.set_onsuccess(Some(success_closure.as_ref().unchecked_ref()));
    request.set_onerror(Some(error_closure.as_ref().unchecked_ref()));

    // Keep closures alive until the request resolves.
    let res = recv
        .await
        .map_err(|_| StorageError::Js(JsValue::from_str("indexeddb open request canceled")))?;
    drop(success_closure);
    drop(error_closure);
    res
}

pub async fn await_request(request: web_sys::IdbRequest) -> Result<JsValue> {
    let (send, recv) = oneshot::channel::<std::result::Result<JsValue, StorageError>>();
    let send = std::rc::Rc::new(std::cell::RefCell::new(Some(send)));

    let success_send = send.clone();
    let success_req = request.clone();
    let success_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = success_send.borrow_mut().take() {
            let res = success_req.result().map_err(StorageError::from);
            let _ = send.send(res);
        }
    });

    let error_send = send.clone();
    let error_req = request.clone();
    let error_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = error_send.borrow_mut().take() {
            let err = match error_req.error() {
                Ok(Some(ex)) => StorageError::from_dom_exception(&ex),
                Ok(None) => StorageError::Js(JsValue::from_str("indexeddb request failed")),
                Err(js) => StorageError::Js(js),
            };
            let _ = send.send(Err(err));
        }
    });

    request.set_onsuccess(Some(success_closure.as_ref().unchecked_ref()));
    request.set_onerror(Some(error_closure.as_ref().unchecked_ref()));

    let res = recv
        .await
        .map_err(|_| StorageError::Js(JsValue::from_str("indexeddb request canceled")))?;
    drop(success_closure);
    drop(error_closure);
    res
}

pub async fn await_transaction(tx: web_sys::IdbTransaction) -> Result<()> {
    let (send, recv) = oneshot::channel::<std::result::Result<(), StorageError>>();
    let send = std::rc::Rc::new(std::cell::RefCell::new(Some(send)));

    let ok_send = send.clone();
    let ok_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = ok_send.borrow_mut().take() {
            let _ = send.send(Ok(()));
        }
    });

    let err_send = send.clone();
    let err_tx = tx.clone();
    let err_closure = Closure::once(move |_event: web_sys::Event| {
        if let Some(send) = err_send.borrow_mut().take() {
            let err = err_tx
                .error()
                .map(|e| StorageError::from_dom_exception(&e))
                .unwrap_or_else(|| {
                    StorageError::Js(JsValue::from_str("indexeddb transaction failed"))
                });
            let _ = send.send(Err(err));
        }
    });

    tx.set_oncomplete(Some(ok_closure.as_ref().unchecked_ref()));
    tx.set_onabort(Some(err_closure.as_ref().unchecked_ref()));
    tx.set_onerror(Some(err_closure.as_ref().unchecked_ref()));

    let res = recv
        .await
        .map_err(|_| StorageError::Js(JsValue::from_str("indexeddb transaction canceled")))?;
    drop(ok_closure);
    drop(err_closure);
    res
}

pub fn transaction_rw(
    db: &web_sys::IdbDatabase,
    store_name: &str,
) -> Result<(web_sys::IdbTransaction, web_sys::IdbObjectStore)> {
    let tx = db
        .transaction_with_str_and_mode(store_name, web_sys::IdbTransactionMode::Readwrite)
        .map_err(StorageError::from)?;
    let store = tx.object_store(store_name).map_err(StorageError::from)?;
    Ok((tx, store))
}

pub fn transaction_ro(
    db: &web_sys::IdbDatabase,
    store_name: &str,
) -> Result<(web_sys::IdbTransaction, web_sys::IdbObjectStore)> {
    let tx = db
        .transaction_with_str_and_mode(store_name, web_sys::IdbTransactionMode::Readonly)
        .map_err(StorageError::from)?;
    let store = tx.object_store(store_name).map_err(StorageError::from)?;
    Ok((tx, store))
}

pub async fn get_value(
    db: &web_sys::IdbDatabase,
    store: &str,
    key: &JsValue,
) -> Result<Option<JsValue>> {
    let (tx, store) = transaction_ro(db, store)?;
    // Important: don't await the request separately and then await the
    // transaction; the transaction may commit in between, causing a race where
    // we miss the `complete` event. Instead, queue the request and await the
    // transaction completion, then read `request.result()`.
    let request = store.get(key).map_err(StorageError::from)?;
    await_transaction(tx).await?;
    let value = request.result().map_err(StorageError::from)?;
    if value.is_undefined() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

pub async fn put_value(
    db: &web_sys::IdbDatabase,
    store: &str,
    key: &JsValue,
    value: &JsValue,
) -> Result<()> {
    let (tx, store) = transaction_rw(db, store)?;
    // Important: do not `.await` between requests in an IndexedDB transaction.
    // Doing so can allow the transaction to auto-commit, causing subsequent
    // operations to fail with `TransactionInactiveError`.
    let _request = store.put_with_key(value, key).map_err(StorageError::from)?;
    await_transaction(tx).await?;
    Ok(())
}

pub async fn delete_value(db: &web_sys::IdbDatabase, store: &str, key: &JsValue) -> Result<()> {
    let (tx, store) = transaction_rw(db, store)?;
    let _request = store.delete(key).map_err(StorageError::from)?;
    await_transaction(tx).await?;
    Ok(())
}

pub async fn get_string(
    db: &web_sys::IdbDatabase,
    store: &str,
    key: &str,
) -> Result<Option<String>> {
    let val = get_value(db, store, &JsValue::from_str(key)).await?;
    Ok(val.and_then(|v| v.as_string()))
}

pub async fn put_string(
    db: &web_sys::IdbDatabase,
    store: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    put_value(
        db,
        store,
        &JsValue::from_str(key),
        &JsValue::from_str(value),
    )
    .await
}

pub fn bytes_to_js_value(bytes: &[u8]) -> JsValue {
    let arr = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    arr.copy_from(bytes);
    arr.into()
}

pub fn js_value_copy_to_bytes(val: &JsValue, dst: &mut [u8]) -> Result<()> {
    // Be strict about accepted JS value types to avoid `Uint8Array::new(val)` implicitly
    // allocating when `val` is not already binary data (e.g. a number length).
    let arr = if let Some(arr) = val.dyn_ref::<js_sys::Uint8Array>() {
        arr.clone()
    } else if val.is_instance_of::<js_sys::ArrayBuffer>() {
        js_sys::Uint8Array::new(val)
    } else {
        return Err(StorageError::Corrupt("expected Uint8Array for stored block"));
    };

    // Defensive bounds: if the IndexedDB entry is corrupt (or attacker-controlled), do not attempt
    // to allocate/copy an absurd amount of data. `st-idb` stores fixed-size blocks, so any size
    // mismatch is invalid.
    if arr.length() as usize != dst.len() {
        return Err(StorageError::Corrupt("stored block size mismatch"));
    }

    arr.copy_to(dst);
    Ok(())
}
