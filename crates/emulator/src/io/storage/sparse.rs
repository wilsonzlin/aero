use std::sync::{Arc, Mutex};

#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::io::storage::error::StorageError;

/// Minimal read/write interface for a persistent byte-addressable store.
///
/// This is the abstraction used by `StreamingDisk` for:
/// - the *remote* base-image cache, and
/// - the guest write overlay.
///
/// In production this would be backed by OPFS sparse files (preferred) or an
/// IndexedDB block store fallback.
pub trait SparseStore: Send + Sync {
    fn size(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError>;
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError>;
    fn flush(&self) -> Result<(), StorageError>;
}

/// An in-memory store. Used for unit/integration tests.
#[derive(Clone)]
pub struct InMemoryStore {
    size: u64,
    data: Arc<Mutex<Vec<u8>>>,
}

impl InMemoryStore {
    pub fn new(size: u64) -> Self {
        Self {
            size,
            data: Arc::new(Mutex::new(vec![0; size as usize])),
        }
    }
}

impl SparseStore for InMemoryStore {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| StorageError::Protocol("read overflow".to_string()))?;
        if end > self.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: self.size,
            });
        }
        let data = self
            .data
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?;
        buf.copy_from_slice(&data[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| StorageError::Protocol("write overflow".to_string()))?;
        if end > self.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: self.size,
            });
        }
        let mut data = self
            .data
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?;
        data[offset as usize..end as usize].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&self) -> Result<(), StorageError> {
        Ok(())
    }
}

/// A file-backed store. When used on a filesystem that supports sparse files,
/// unwritten regions remain holes.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileStore {
    size: u64,
    file: Mutex<std::fs::File>,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileStore {
    pub fn create(path: impl AsRef<Path>, size: u64) -> Result<Self, StorageError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)
            .map_err(|e| StorageError::Io(e.to_string()))?;

        file.set_len(size)
            .map_err(|e| StorageError::Io(e.to_string()))?;

        Ok(Self {
            size,
            file: Mutex::new(file),
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SparseStore for FileStore {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), StorageError> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| StorageError::Protocol("read overflow".to_string()))?;
        if end > self.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: self.size,
            });
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| StorageError::Io(e.to_string()))?;
        file.read_exact(buf)
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(())
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), StorageError> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or_else(|| StorageError::Protocol("write overflow".to_string()))?;
        if end > self.size {
            return Err(StorageError::OutOfBounds {
                offset,
                len: buf.len() as u64,
                size: self.size,
            });
        }

        let mut file = self
            .file
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| StorageError::Io(e.to_string()))?;
        file.write_all(buf)
            .map_err(|e| StorageError::Io(e.to_string()))?;
        Ok(())
    }

    fn flush(&self) -> Result<(), StorageError> {
        let mut file = self
            .file
            .lock()
            .map_err(|_| StorageError::Io("poisoned lock".to_string()))?;
        file.flush().map_err(|e| StorageError::Io(e.to_string()))
    }
}
