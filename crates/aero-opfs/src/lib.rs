//! Origin Private File System (OPFS) storage backends for Aero (wasm32).
//!
//! This crate provides wasm32 implementations of `aero-storage` traits on top of browser
//! persistence APIs.
//!
//! Note: the Cargo package name is `aero-opfs`, but it is imported as `aero_opfs` in Rust code
//! (hyphens become underscores).
//!
//! The primary, boot-critical storage path is OPFS `FileSystemSyncAccessHandle` (fast and
//! synchronous in a Dedicated Worker).
//!
//! Some additional backends (async OPFS APIs, IndexedDB) exist as async-only fallbacks for
//! host-side tooling and environments where sync handles are unavailable. Those async-only
//! backends do **not** implement the canonical synchronous `aero_storage::{StorageBackend, VirtualDisk}`
//! traits used by the Rust disk/controller stack.
//!
//! See:
//! - `docs/19-indexeddb-storage-story.md`
//! - `docs/20-storage-trait-consolidation.md`
//!
//! Main types:
//!
//! - [`OpfsByteStorage`]: implements [`aero_storage::StorageBackend`] using OPFS
//!   `FileSystemSyncAccessHandle` when available.
//! - [`OpfsBackend`]: implements [`aero_storage::VirtualDisk`] for disk-oriented I/O.
//! - [`OpfsStorage`]: convenience wrapper that chooses the best available persistence backend
//!   (sync OPFS when possible; otherwise async fallbacks).
//! - [`OpfsSyncFile`]: wraps `FileSystemSyncAccessHandle` with a cursor and implements
//!   `std::io::{Read, Write, Seek}` for streaming snapshot read/write.
//!
//! Most APIs are meaningful only on wasm32; non-wasm builds provide stubs that return
//! [`DiskError::NotSupported`].
//!
//! ## Errors
//!
//! All public APIs in this crate use the canonical [`DiskError`] type from
//! [`aero_storage`], re-exported here for convenience.

pub mod io;
pub mod platform;

mod error;
pub use error::{DiskError, DiskResult};

pub use crate::io::snapshot_file::OpfsSyncFile;
pub use crate::io::storage::backends::opfs::{
    OpfsBackend, OpfsBackendMode, OpfsByteStorage, OpfsSendDisk, OpfsStorage,
};

/// Returns whether OPFS `FileSystemSyncAccessHandle` APIs are available in the **current** JS
/// execution context.
///
/// This is a best-effort preflight check intended for callers that need a synchronous OPFS backend
/// (e.g. boot-critical disk/controller stacks) and want to fail fast with a clear reason when the
/// browser/context cannot support it.
///
/// Conditions checked (wasm32 only):
/// - OPFS is exposed (`navigator.storage.getDirectory` exists)
/// - Running in a worker scope (sync access handles are worker-only)
/// - `FileSystemFileHandle.prototype.createSyncAccessHandle` exists
///
/// On non-wasm targets this always returns `false`.
#[cfg(target_arch = "wasm32")]
pub fn opfs_sync_access_supported() -> bool {
    use crate::platform::storage::opfs as opfs_platform;
    use js_sys::Reflect;
    use wasm_bindgen::JsValue;

    if !opfs_platform::is_opfs_supported() {
        return false;
    }
    if !opfs_platform::is_worker_scope() {
        return false;
    }

    // `FileSystemFileHandle` is an interface; in browsers that implement the File System Access
    // APIs it is typically exposed as a global interface object with a `.prototype`.
    let global = js_sys::global();
    let ctor = match Reflect::get(&global, &JsValue::from_str("FileSystemFileHandle")) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let proto = match Reflect::get(&ctor, &JsValue::from_str("prototype")) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let method = match Reflect::get(&proto, &JsValue::from_str("createSyncAccessHandle")) {
        Ok(v) => v,
        Err(_) => return false,
    };
    method.is_function()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn opfs_sync_access_supported() -> bool {
    false
}

// wasm-bindgen-test defaults to running under Node. OPFS requires a browser environment,
// so configure wasm-only tests to run in a browser once per crate.
#[cfg(all(test, target_arch = "wasm32"))]
wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

#[cfg(test)]
mod opfs_sync_access_supported_tests {
    use super::opfs_sync_access_supported;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn returns_false_on_non_wasm() {
        assert!(!opfs_sync_access_supported());
    }

    #[cfg(target_arch = "wasm32")]
    mod wasm {
        use super::opfs_sync_access_supported;
        use crate::platform::storage::opfs as opfs_platform;
        use js_sys::Reflect;
        use wasm_bindgen::JsValue;
        use wasm_bindgen_test::wasm_bindgen_test;

        fn proto_has_create_sync_access_handle() -> bool {
            let global = js_sys::global();
            let ctor = Reflect::get(&global, &JsValue::from_str("FileSystemFileHandle")).ok();
            let Some(ctor) = ctor else {
                return false;
            };
            let proto = Reflect::get(&ctor, &JsValue::from_str("prototype")).ok();
            let Some(proto) = proto else {
                return false;
            };
            let method = Reflect::get(&proto, &JsValue::from_str("createSyncAccessHandle")).ok();
            let Some(method) = method else {
                return false;
            };
            method.is_function()
        }

        #[wasm_bindgen_test]
        fn matches_preflight_conjunction() {
            let expected = opfs_platform::is_opfs_supported()
                && opfs_platform::is_worker_scope()
                && proto_has_create_sync_access_handle();
            assert_eq!(opfs_sync_access_supported(), expected);
        }
    }
}
