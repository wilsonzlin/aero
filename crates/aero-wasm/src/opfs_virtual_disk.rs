//! Helpers for opening OPFS-backed disks with an explicit format (raw vs aerosparse).
//!
//! The browser disk manager stores metadata describing the disk bytes format. For HDD base images
//! this can be either:
//! - `"raw"`: a flat sector file, or
//! - `"aerospar"`: `aero_storage::AeroSparseDisk` (sparse file with header + allocation table).
//!
//! When opening disk bytes from OPFS we must respect this metadata; otherwise a sparse header will
//! be interpreted as guest-visible disk bytes.

use aero_storage::{DiskError, StorageBackend, VirtualDisk, VirtualDiskSend};

/// Open a byte-addressed backend as a `VirtualDisk` based on an explicit format string.
///
/// This is used by the wasm32 OPFS integration and is also unit-tested on native targets using
/// `aero_storage::MemBackend`.
pub(crate) fn open_virtual_disk_from_backend<B: StorageBackend + VirtualDiskSend + 'static>(
    backend: B,
    format: &str,
    expected_size_bytes: Option<u64>,
) -> aero_storage::Result<Box<dyn VirtualDisk>> {
    if format.eq_ignore_ascii_case("aerospar") || format.eq_ignore_ascii_case("aerosparse") {
        let disk = aero_storage::AeroSparseDisk::open(backend)?;
        if let Some(expected) = expected_size_bytes {
            if expected != 0 && disk.header().disk_size_bytes != expected {
                return Err(DiskError::Io(format!(
                    "aerosparse disk_size_bytes mismatch: header={} expected={expected}",
                    disk.header().disk_size_bytes
                )));
            }
        }
        return Ok(Box::new(disk));
    }

    // Default: treat as a raw disk image.
    let disk = aero_storage::RawDisk::open(backend)?;
    if let Some(expected) = expected_size_bytes {
        if expected != 0 && disk.capacity_bytes() != expected {
            return Err(DiskError::Io(format!(
                "raw disk size mismatch: file={} expected={expected}",
                disk.capacity_bytes()
            )));
        }
    }
    Ok(Box::new(disk))
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn open_opfs_virtual_disk(
    path: &str,
    format: &str,
) -> Result<Box<dyn VirtualDisk>, wasm_bindgen::JsValue> {
    if format.eq_ignore_ascii_case("aerospar") || format.eq_ignore_ascii_case("aerosparse") {
        let backend = aero_opfs::OpfsByteStorage::open(path, false)
            .await
            .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
        return open_virtual_disk_from_backend(backend, format, None)
            .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()));
    }

    // Default to the raw-sector OPFS disk backend.
    let backend = aero_opfs::OpfsBackend::open_existing(path)
        .await
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
    Ok(Box::new(backend))
}

#[cfg(test)]
mod tests {
    use super::open_virtual_disk_from_backend;

    use aero_storage::{AeroSparseConfig, AeroSparseDisk, MemBackend, VirtualDisk};

    #[test]
    fn opens_aerosparse_disk_by_format_and_validates_header_size() {
        let disk_size_bytes = 2 * 1024 * 1024;

        let backend = MemBackend::new();
        let mut sparse = AeroSparseDisk::create(
            backend,
            AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: 1024 * 1024,
            },
        )
        .expect("create aerosparse should succeed");

        // Write a recognizable payload to the guest-visible disk bytes at offset 0.
        sparse
            .write_at(0, b"hello")
            .expect("write to aerosparse disk should succeed");
        sparse.flush().expect("flush aerosparse should succeed");

        let backend = sparse.into_backend();

        // Sanity: underlying bytes contain the aerosparse header magic at the file start.
        assert_eq!(&backend.as_slice()[0..8], b"AEROSPAR");

        // Open using the helper and ensure we see the guest-visible bytes, not the header.
        let mut disk =
            open_virtual_disk_from_backend(backend.clone(), "aerospar", Some(disk_size_bytes))
                .expect("open aerosparse via helper should succeed");

        assert_eq!(disk.capacity_bytes(), disk_size_bytes);
        let mut buf = [0u8; 8];
        disk.read_at(0, &mut buf)
            .expect("read from opened disk should succeed");
        assert_eq!(&buf[..5], b"hello");

        // Size mismatch should error.
        let err = match open_virtual_disk_from_backend(
            backend,
            "aerospar",
            Some(disk_size_bytes + aero_storage::SECTOR_SIZE as u64),
        ) {
            Ok(_) => panic!("size mismatch should error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("mismatch"),
            "unexpected error: {err}"
        );
    }
}
