use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::io::storage::{error::StorageError, rangeset::RangeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingMetadata {
    pub downloaded: RangeSet,
    pub dirty: RangeSet,
}

pub trait MetadataStore: Send + Sync {
    fn load(&self) -> Result<Option<StreamingMetadata>, StorageError>;
    fn save(&self, meta: &StreamingMetadata) -> Result<(), StorageError>;
}

#[derive(Clone, Default)]
pub struct InMemoryMetadataStore {
    inner: Arc<Mutex<Option<StreamingMetadata>>>,
}

impl InMemoryMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MetadataStore for InMemoryMetadataStore {
    fn load(&self) -> Result<Option<StreamingMetadata>, StorageError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?
            .clone())
    }

    fn save(&self, meta: &StreamingMetadata) -> Result<(), StorageError> {
        *self
            .inner
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))? = Some(meta.clone());
        Ok(())
    }
}

pub struct JsonFileMetadataStore {
    path: PathBuf,
}

impl JsonFileMetadataStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn tmp_path(path: &Path) -> PathBuf {
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        PathBuf::from(tmp)
    }
}

impl MetadataStore for JsonFileMetadataStore {
    fn load(&self) -> Result<Option<StreamingMetadata>, StorageError> {
        if !self.path.exists() {
            return Ok(None);
        }

        let raw = fs::read_to_string(&self.path).map_err(|e| StorageError::Io(e.to_string()))?;
        let meta =
            serde_json::from_str(&raw).map_err(|e| StorageError::Protocol(e.to_string()))?;
        Ok(Some(meta))
    }

    fn save(&self, meta: &StreamingMetadata) -> Result<(), StorageError> {
        let raw = serde_json::to_string(meta).map_err(|e| StorageError::Protocol(e.to_string()))?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
        }

        let tmp = Self::tmp_path(&self.path);
        fs::write(&tmp, raw).map_err(|e| StorageError::Io(e.to_string()))?;
        fs::rename(&tmp, &self.path).map_err(|e| StorageError::Io(e.to_string()))
    }
}

