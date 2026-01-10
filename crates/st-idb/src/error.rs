use wasm_bindgen::JsValue;

pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("IndexedDB is not available in this context")]
    IndexedDbUnavailable,
    #[error("disk I/O out of bounds (offset={offset}, len={len}, capacity={capacity})")]
    OutOfBounds {
        offset: u64,
        len: usize,
        capacity: u64,
    },
    #[error("stored data is corrupt or has an unexpected format: {0}")]
    Corrupt(&'static str),
    #[error("unsupported on-disk format version {0}")]
    UnsupportedFormat(u32),
    #[error("quota exceeded while writing to IndexedDB")]
    QuotaExceeded,
    #[error("indexeddb operation failed: {0:?}")]
    Js(JsValue),
}

impl StorageError {
    pub(crate) fn from_dom_exception(ex: &web_sys::DomException) -> Self {
        // https://webidl.spec.whatwg.org/#idl-DOMException-error-names
        match ex.name().as_str() {
            "QuotaExceededError" => StorageError::QuotaExceeded,
            _ => StorageError::Js(ex.into()),
        }
    }
}

impl From<JsValue> for StorageError {
    fn from(value: JsValue) -> Self {
        StorageError::Js(value)
    }
}
