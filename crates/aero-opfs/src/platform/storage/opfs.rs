use crate::DiskError;

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use js_sys::{Function, Object, Promise, Reflect};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    #[wasm_bindgen]
    extern "C" {
        #[derive(Clone)]
        pub type FileSystemDirectoryHandle;

        #[wasm_bindgen(catch, method, js_name = getDirectoryHandle)]
        fn get_directory_handle(
            this: &FileSystemDirectoryHandle,
            name: &str,
            options: &JsValue,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = getFileHandle)]
        fn get_file_handle(
            this: &FileSystemDirectoryHandle,
            name: &str,
            options: &JsValue,
        ) -> core::result::Result<Promise, JsValue>;

        #[derive(Clone)]
        pub type FileSystemFileHandle;

        #[wasm_bindgen(catch, method, js_name = createSyncAccessHandle)]
        fn create_sync_access_handle(
            this: &FileSystemFileHandle,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = createWritable)]
        fn create_writable(
            this: &FileSystemFileHandle,
            options: &JsValue,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = getFile)]
        fn get_file(this: &FileSystemFileHandle) -> core::result::Result<Promise, JsValue>;

        #[derive(Clone)]
        pub type FileSystemSyncAccessHandle;

        #[wasm_bindgen(catch, method, js_name = read)]
        pub fn read(
            this: &FileSystemSyncAccessHandle,
            buffer: &mut [u8],
            options: &JsValue,
        ) -> core::result::Result<u32, JsValue>;

        #[wasm_bindgen(catch, method, js_name = write)]
        pub fn write(
            this: &FileSystemSyncAccessHandle,
            buffer: &[u8],
            options: &JsValue,
        ) -> core::result::Result<u32, JsValue>;

        #[wasm_bindgen(catch, method, js_name = flush)]
        pub fn flush(this: &FileSystemSyncAccessHandle) -> core::result::Result<(), JsValue>;

        #[wasm_bindgen(catch, method, js_name = close)]
        pub fn close(this: &FileSystemSyncAccessHandle) -> core::result::Result<(), JsValue>;

        #[wasm_bindgen(catch, method, js_name = getSize)]
        pub fn get_size(this: &FileSystemSyncAccessHandle) -> core::result::Result<f64, JsValue>;

        #[wasm_bindgen(catch, method, js_name = truncate)]
        pub fn truncate(
            this: &FileSystemSyncAccessHandle,
            size: f64,
        ) -> core::result::Result<(), JsValue>;

        #[derive(Clone)]
        pub type FileSystemWritableFileStream;

        #[wasm_bindgen(catch, method, js_name = write)]
        fn writable_write_promise(
            this: &FileSystemWritableFileStream,
            data: &JsValue,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = seek)]
        fn writable_seek_promise(
            this: &FileSystemWritableFileStream,
            position: f64,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = truncate)]
        fn writable_truncate_promise(
            this: &FileSystemWritableFileStream,
            size: f64,
        ) -> core::result::Result<Promise, JsValue>;

        #[wasm_bindgen(catch, method, js_name = close)]
        fn writable_close_promise(
            this: &FileSystemWritableFileStream,
        ) -> core::result::Result<Promise, JsValue>;
    }

    pub(crate) fn disk_error_from_js(err: JsValue) -> DiskError {
        use wasm_bindgen::JsCast;

        if err.is_instance_of::<web_sys::DomException>() {
            let dom: web_sys::DomException = err.unchecked_into();
            match dom.name().as_str() {
                "QuotaExceededError" => return DiskError::QuotaExceeded,
                // Indicates the browser is blocking access to storage APIs (e.g. disabled
                // IndexedDB/OPFS, third-party iframe restrictions, etc).
                "NotAllowedError" | "SecurityError" => return DiskError::BackendUnavailable,
                "InvalidStateError" => return DiskError::InvalidState(dom.message()),
                "NotSupportedError" => return DiskError::NotSupported(dom.message()),
                _ => return DiskError::Io(format!("{}: {}", dom.name(), dom.message())),
            }
        }

        if err.is_instance_of::<js_sys::TypeError>() {
            let msg = js_sys::Error::from(err).message();
            return DiskError::NotSupported(msg.into());
        }

        if err.is_instance_of::<js_sys::Error>() {
            let e: js_sys::Error = err.unchecked_into();
            return DiskError::Io(e.message().into());
        }

        DiskError::Io(format!("{err:?}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use js_sys::{Array, Function, Reflect};
        use wasm_bindgen::JsCast;
        use wasm_bindgen::JsValue;
        use wasm_bindgen_test::wasm_bindgen_test;

        wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

        fn dom_exception(name: &str, message: &str) -> JsValue {
            // `new DOMException(message, name)`
            let ctor = Reflect::get(&js_sys::global(), &JsValue::from_str("DOMException"))
                .expect("DOMException global should exist");
            let ctor: Function = ctor.dyn_into().expect("DOMException should be a constructor");
            let args = Array::new();
            args.push(&JsValue::from_str(message));
            args.push(&JsValue::from_str(name));
            Reflect::construct(&ctor, &args).expect("construct DOMException")
        }

        #[wasm_bindgen_test]
        fn dom_exception_security_error_maps_to_backend_unavailable() {
            let err = dom_exception("SecurityError", "blocked");
            assert!(matches!(
                disk_error_from_js(err),
                DiskError::BackendUnavailable
            ));
        }

        #[wasm_bindgen_test]
        fn dom_exception_not_allowed_error_maps_to_backend_unavailable() {
            let err = dom_exception("NotAllowedError", "blocked");
            assert!(matches!(
                disk_error_from_js(err),
                DiskError::BackendUnavailable
            ));
        }

        #[wasm_bindgen_test]
        fn dom_exception_quota_exceeded_maps_to_quota_exceeded() {
            let err = dom_exception("QuotaExceededError", "quota exceeded");
            assert!(matches!(disk_error_from_js(err), DiskError::QuotaExceeded));
        }

        #[wasm_bindgen_test]
        fn dom_exception_not_supported_maps_to_not_supported() {
            let err = dom_exception("NotSupportedError", "nope");
            assert!(matches!(
                disk_error_from_js(err),
                DiskError::NotSupported(msg) if msg == "nope"
            ));
        }

        #[wasm_bindgen_test]
        fn js_type_error_maps_to_not_supported() {
            let err = js_sys::TypeError::new("bad").into();
            assert!(matches!(
                disk_error_from_js(err),
                DiskError::NotSupported(msg) if msg == "bad"
            ));
        }

        #[wasm_bindgen_test]
        fn js_error_maps_to_io() {
            let err = js_sys::Error::new("boom").into();
            assert!(matches!(
                disk_error_from_js(err),
                DiskError::Io(msg) if msg == "boom"
            ));
        }
    }

    async fn await_promise(promise: Promise) -> core::result::Result<JsValue, DiskError> {
        JsFuture::from(promise).await.map_err(disk_error_from_js)
    }

    pub fn is_worker_scope() -> bool {
        js_sys::global()
            .dyn_into::<web_sys::WorkerGlobalScope>()
            .is_ok()
    }

    pub fn is_opfs_supported() -> bool {
        let global = js_sys::global();
        let navigator = Reflect::get(&global, &JsValue::from_str("navigator")).ok();
        let Some(navigator) = navigator else {
            return false;
        };
        let storage = Reflect::get(&navigator, &JsValue::from_str("storage")).ok();
        let Some(storage) = storage else {
            return false;
        };
        let get_directory = Reflect::get(&storage, &JsValue::from_str("getDirectory")).ok();
        let Some(get_directory) = get_directory else {
            return false;
        };
        get_directory.is_function()
    }

    pub async fn get_root_dir() -> Result<FileSystemDirectoryHandle, DiskError> {
        if !is_opfs_supported() {
            return Err(DiskError::NotSupported(
                "OPFS is unavailable (navigator.storage.getDirectory missing)".to_string(),
            ));
        }

        let global = js_sys::global();
        let navigator =
            Reflect::get(&global, &JsValue::from_str("navigator")).map_err(disk_error_from_js)?;
        let storage =
            Reflect::get(&navigator, &JsValue::from_str("storage")).map_err(disk_error_from_js)?;
        let get_directory = Reflect::get(&storage, &JsValue::from_str("getDirectory"))
            .map_err(disk_error_from_js)?;
        let get_directory: Function = get_directory.dyn_into().map_err(disk_error_from_js)?;

        let promise = get_directory
            .call0(&storage)
            .map_err(disk_error_from_js)?
            .dyn_into::<Promise>()
            .map_err(disk_error_from_js)?;

        await_promise(promise)
            .await?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(disk_error_from_js)
    }

    async fn get_or_create_directory(
        parent: &FileSystemDirectoryHandle,
        name: &str,
    ) -> Result<FileSystemDirectoryHandle, DiskError> {
        let opts = Object::new();
        Reflect::set(
            &opts,
            &JsValue::from_str("create"),
            &JsValue::from_bool(true),
        )
        .map_err(disk_error_from_js)?;
        let promise = parent
            .get_directory_handle(name, &opts.into())
            .map_err(disk_error_from_js)?;
        await_promise(promise)
            .await?
            .dyn_into::<FileSystemDirectoryHandle>()
            .map_err(disk_error_from_js)
    }

    async fn get_or_create_file(
        parent: &FileSystemDirectoryHandle,
        name: &str,
        create: bool,
    ) -> Result<FileSystemFileHandle, DiskError> {
        let opts = Object::new();
        Reflect::set(
            &opts,
            &JsValue::from_str("create"),
            &JsValue::from_bool(create),
        )
        .map_err(disk_error_from_js)?;
        let promise = parent
            .get_file_handle(name, &opts.into())
            .map_err(disk_error_from_js)?;
        await_promise(promise)
            .await?
            .dyn_into::<FileSystemFileHandle>()
            .map_err(disk_error_from_js)
    }

    pub async fn open_file(path: &str, create: bool) -> Result<FileSystemFileHandle, DiskError> {
        let root = get_root_dir().await?;
        let mut dir = root;

        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let (dirs, file_name) = match parts.split_last() {
            Some((file, dirs)) => (dirs, *file),
            None => {
                return Err(DiskError::Io("OPFS path must not be empty".to_string()));
            }
        };

        for component in dirs {
            dir = get_or_create_directory(&dir, component).await?;
        }

        get_or_create_file(&dir, file_name, create).await
    }

    pub fn file_handle_supports_sync_access_handle(file: &FileSystemFileHandle) -> bool {
        Reflect::has(file.as_ref(), &JsValue::from_str("createSyncAccessHandle")).unwrap_or(false)
    }

    pub async fn create_sync_handle(
        file: &FileSystemFileHandle,
    ) -> Result<FileSystemSyncAccessHandle, DiskError> {
        let handle = match await_promise(
            file.create_sync_access_handle()
                .map_err(disk_error_from_js)?,
        )
        .await
        {
            Ok(handle) => handle,
            Err(DiskError::InvalidState(_)) => return Err(DiskError::InUse),
            Err(e) => return Err(e),
        };

        handle
            .dyn_into::<FileSystemSyncAccessHandle>()
            .map_err(disk_error_from_js)
    }

    pub async fn create_writable_stream(
        file: &FileSystemFileHandle,
        keep_existing_data: bool,
    ) -> Result<FileSystemWritableFileStream, DiskError> {
        let opts = Object::new();
        Reflect::set(
            &opts,
            &JsValue::from_str("keepExistingData"),
            &JsValue::from_bool(keep_existing_data),
        )
        .map_err(disk_error_from_js)?;

        let promise = file
            .create_writable(&opts.into())
            .map_err(disk_error_from_js)?;

        let stream = match await_promise(promise).await {
            Ok(stream) => stream,
            // When a file already has an open sync access handle, browsers use
            // `InvalidStateError`; map it to the more semantic `InUse`.
            Err(DiskError::InvalidState(_)) => return Err(DiskError::InUse),
            Err(e) => return Err(e),
        };

        stream
            .dyn_into::<FileSystemWritableFileStream>()
            .map_err(disk_error_from_js)
    }

    pub async fn get_file_obj(file: &FileSystemFileHandle) -> Result<web_sys::File, DiskError> {
        let promise = file.get_file().map_err(disk_error_from_js)?;
        await_promise(promise)
            .await?
            .dyn_into::<web_sys::File>()
            .map_err(disk_error_from_js)
    }

    pub async fn writable_seek(
        stream: &FileSystemWritableFileStream,
        position: f64,
    ) -> Result<(), DiskError> {
        let promise = stream
            .writable_seek_promise(position)
            .map_err(disk_error_from_js)?;
        await_promise(promise).await?;
        Ok(())
    }

    pub async fn writable_write(
        stream: &FileSystemWritableFileStream,
        data: &JsValue,
    ) -> Result<(), DiskError> {
        let promise = stream
            .writable_write_promise(data)
            .map_err(disk_error_from_js)?;
        await_promise(promise).await?;
        Ok(())
    }

    pub async fn writable_truncate(
        stream: &FileSystemWritableFileStream,
        size: f64,
    ) -> Result<(), DiskError> {
        let promise = stream
            .writable_truncate_promise(size)
            .map_err(disk_error_from_js)?;
        await_promise(promise).await?;
        Ok(())
    }

    pub async fn writable_close(stream: &FileSystemWritableFileStream) -> Result<(), DiskError> {
        let promise = stream
            .writable_close_promise()
            .map_err(disk_error_from_js)?;
        await_promise(promise).await?;
        Ok(())
    }

    pub use FileSystemDirectoryHandle as DirectoryHandle;
    pub use FileSystemFileHandle as FileHandle;
    pub use FileSystemSyncAccessHandle as SyncAccessHandle;
    pub use FileSystemWritableFileStream as WritableStream;
}

#[cfg(target_arch = "wasm32")]
pub use wasm::*;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;

    pub fn is_worker_scope() -> bool {
        false
    }

    pub fn is_opfs_supported() -> bool {
        false
    }

    pub async fn get_root_dir() -> Result<(), DiskError> {
        Err(DiskError::NotSupported("OPFS is wasm-only".to_string()))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
