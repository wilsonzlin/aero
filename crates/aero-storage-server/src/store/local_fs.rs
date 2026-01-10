use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::{BoxedAsyncRead, ImageMeta, ImageStore, StoreError, CONTENT_TYPE_DISK_IMAGE};

/// Local filesystem-backed [`ImageStore`].
///
/// # Security
/// `image_id` is treated as an opaque identifier and is restricted to ASCII
/// `[A-Za-z0-9._-]` to prevent path traversal.
#[derive(Debug, Clone)]
pub struct LocalFsImageStore {
    root: PathBuf,
}

impl LocalFsImageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, image_id: &str) -> Result<PathBuf, StoreError> {
        validate_image_id(image_id)?;
        Ok(self.root.join(image_id))
    }
}

pub(crate) fn validate_image_id(image_id: &str) -> Result<(), StoreError> {
    if image_id.is_empty() || image_id == "." || image_id == ".." {
        return Err(StoreError::InvalidImageId {
            image_id: image_id.to_string(),
        });
    }

    let is_allowed = image_id.bytes().all(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-' => true,
        _ => false,
    });

    if !is_allowed {
        return Err(StoreError::InvalidImageId {
            image_id: image_id.to_string(),
        });
    }

    Ok(())
}

fn weak_etag_from_size_and_mtime(size: u64, mtime: Option<SystemTime>) -> String {
    // Deterministic weak ETag based on (size, mtime). This avoids hashing large images.
    //
    // Note: filesystems with coarse mtime resolution may not change this ETag for rapid edits.
    let mtime = mtime
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0));

    format!(
        "W/\"{size:x}-{sec:x}-{nsec:x}\"",
        sec = mtime.0,
        nsec = mtime.1
    )
}

fn map_not_found(err: std::io::Error) -> StoreError {
    if err.kind() == std::io::ErrorKind::NotFound {
        StoreError::NotFound
    } else {
        StoreError::Io(err)
    }
}

#[async_trait::async_trait]
impl ImageStore for LocalFsImageStore {
    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        let path = self.path_for(image_id)?;
        let meta = fs::metadata(path).await.map_err(map_not_found)?;

        let last_modified = meta.modified().ok();
        let etag = Some(weak_etag_from_size_and_mtime(meta.len(), last_modified));

        Ok(ImageMeta {
            size: meta.len(),
            etag,
            last_modified,
            content_type: CONTENT_TYPE_DISK_IMAGE,
        })
    }

    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        let path = self.path_for(image_id)?;

        let mut file = fs::File::open(&path).await.map_err(map_not_found)?;
        let size = file.metadata().await?.len();

        let end = start
            .checked_add(len)
            .ok_or(StoreError::InvalidRange { start, len, size })?;
        if start > size || end > size {
            return Err(StoreError::InvalidRange { start, len, size });
        }

        file.seek(SeekFrom::Start(start)).await?;

        Ok(Box::pin(file.take(len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

    #[test]
    fn image_id_validation_rejects_traversal() {
        let invalid = [
            "",
            ".",
            "..",
            "../x",
            "..\\x",
            "a/b",
            "a\\b",
            "%2e%2e%2fx",
            "..%2Fx",
            "x%2F..%2Fetc%2Fpasswd",
        ];

        for image_id in invalid {
            assert!(
                validate_image_id(image_id).is_err(),
                "expected invalid image_id: {image_id:?}"
            );
        }

        let valid = ["a", "test.img", "ABC_123-foo.bar", "a..b"];
        for image_id in valid {
            assert!(
                validate_image_id(image_id).is_ok(),
                "expected valid image_id: {image_id:?}"
            );
        }
    }

    #[tokio::test]
    async fn open_range_returns_expected_bytes_for_random_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let image_id = "test.img";
        let path = root.join(image_id);

        let data: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
        tokio::fs::write(&path, &data).await.unwrap();

        let store = LocalFsImageStore::new(root);
        let size = data.len() as u64;

        let mut rng = StdRng::seed_from_u64(0x5EED);
        for _ in 0..64 {
            let start = rng.gen_range(0..=size);
            let max_len = size - start;
            let len = rng.gen_range(0..=max_len);

            let mut reader = store.open_range(image_id, start, len).await.unwrap();
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).await.unwrap();

            assert_eq!(buf.len() as u64, len);
            assert_eq!(
                &buf[..],
                &data[start as usize..(start + len) as usize],
                "mismatch for start={start} len={len}"
            );
        }
    }

    #[tokio::test]
    async fn open_range_supports_offsets_beyond_4gib() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let image_id = "sparse.img";
        let path = root.join(image_id);

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .await
            .unwrap();

        let total_len = 4_u64 * 1024 * 1024 * 1024 + 1024;
        file.set_len(total_len).await.unwrap();

        let expected = b"0123456789abcdef";
        let start = total_len - expected.len() as u64;

        file.seek(SeekFrom::Start(start)).await.unwrap();
        file.write_all(expected).await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        let store = LocalFsImageStore::new(root);
        let mut reader = store
            .open_range(image_id, start, expected.len() as u64)
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();

        assert_eq!(buf, expected);
    }
}
