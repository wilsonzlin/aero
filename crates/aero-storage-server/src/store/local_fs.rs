use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::OnceCell;

use super::manifest::{Manifest, ManifestError, ManifestImage};
use super::{
    validate_image_id, BoxedAsyncRead, ImageCatalogEntry, ImageMeta, ImageStore, StoreError,
    CONTENT_TYPE_DISK_IMAGE,
};

/// Local filesystem-backed [`ImageStore`].
///
/// # Catalog source
///
/// If a `manifest.json` is present under `root`, it is used as the image catalog (preferred).
/// Otherwise, the store falls back to a stable directory listing of `root` (development only),
/// unless [`LocalFsImageStore::with_require_manifest`] is enabled.
#[derive(Debug, Clone)]
pub struct LocalFsImageStore {
    root: PathBuf,
    require_manifest: bool,
    manifest: std::sync::Arc<OnceCell<Option<Manifest>>>,
}

impl LocalFsImageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        // Canonicalize once so we can safely compare canonicalized image paths against it.
        //
        // If canonicalization fails (e.g. the directory doesn't exist yet), we fall back to the
        // provided path; subsequent image resolutions will fail closed.
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        Self {
            root,
            require_manifest: false,
            manifest: std::sync::Arc::new(OnceCell::new()),
        }
    }

    /// Require `manifest.json` to exist under the store root.
    ///
    /// When enabled, the store will not fall back to directory listing in `list_images`, and
    /// `get_meta/get_image/open_range` will fail if no manifest can be loaded.
    pub fn with_require_manifest(mut self, require_manifest: bool) -> Self {
        self.require_manifest = require_manifest;
        self
    }

    async fn ensure_within_root(&self, path: &Path) -> Result<PathBuf, StoreError> {
        // Canonicalize the requested path (resolving symlinks) and ensure it still lives under
        // our canonical root. This prevents symlink/path-escape attacks where a file inside the
        // root points outside the configured image directory.
        let canonical = fs::canonicalize(path).await.map_err(map_not_found)?;
        if !canonical.starts_with(&self.root) {
            return Err(StoreError::NotFound);
        }
        Ok(canonical)
    }

    async fn load_manifest(&self) -> Result<Option<Manifest>, StoreError> {
        let root = self.root.clone();
        let require_manifest = self.require_manifest;
        self.manifest
            .get_or_try_init(|| async move {
                let manifest_path = root.join("manifest.json");
                match fs::read_to_string(manifest_path).await {
                    Ok(raw) => Ok(Some(Manifest::parse_str(&raw)?)),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        if require_manifest {
                            Err(ManifestError::Missing {
                                path: root.join("manifest.json").display().to_string(),
                            }
                            .into())
                        } else {
                            Ok(None)
                        }
                    }
                    Err(err) => Err(StoreError::Io(err)),
                }
            })
            .await
            .cloned()
    }

    async fn lookup_manifest_image(
        &self,
        image_id: &str,
    ) -> Result<Option<ManifestImage>, StoreError> {
        let Some(manifest) = self.load_manifest().await? else {
            return Ok(None);
        };

        manifest
            .images
            .iter()
            .find(|img| img.id == image_id)
            .cloned()
            .ok_or(StoreError::NotFound)
            .map(Some)
    }

    async fn resolve_image(&self, image_id: &str) -> Result<ResolvedImage, StoreError> {
        validate_image_id(image_id)?;

        if let Some(image) = self.lookup_manifest_image(image_id).await? {
            let path = self.root.join(Path::new(&image.file));
            return Ok(ResolvedImage {
                id: image.id,
                name: image.name,
                description: image.description,
                recommended_chunk_size_bytes: image.recommended_chunk_size_bytes,
                public: image.public,
                etag: image.etag,
                last_modified: image.last_modified_time,
                path,
            });
        }

        // Directory-listing fallback (dev mode): `image_id` is also the filename.
        let path = self.root.join(Path::new(image_id));

        Ok(ResolvedImage {
            id: image_id.to_string(),
            name: image_id.to_string(),
            description: None,
            recommended_chunk_size_bytes: None,
            public: true,
            etag: None,
            last_modified: None,
            path,
        })
    }

    async fn meta_from_path(
        &self,
        path: &Path,
        etag_override: Option<&str>,
        last_modified_override: Option<SystemTime>,
    ) -> Result<ImageMeta, StoreError> {
        let path = self.ensure_within_root(path).await?;
        let meta = fs::metadata(&path).await.map_err(map_not_found)?;
        // `SystemTime` can represent times before the Unix epoch, but HTTP `Last-Modified`
        // formatting (via `httpdate`) cannot. Normalize pre-epoch mtimes to `None` so we avoid
        // emitting misleading JSON timestamps and ensure derived validators behave consistently.
        let last_modified = last_modified_override
            .or_else(|| meta.modified().ok())
            .filter(|t| t.duration_since(UNIX_EPOCH).is_ok());
        let etag = Some(match etag_override {
            Some(etag) => etag.to_string(),
            None => etag_from_size_and_mtime(meta.len(), last_modified),
        });

        Ok(ImageMeta {
            size: meta.len(),
            etag,
            last_modified,
            content_type: CONTENT_TYPE_DISK_IMAGE,
        })
    }
}

#[derive(Debug, Clone)]
struct ResolvedImage {
    id: String,
    name: String,
    description: Option<String>,
    recommended_chunk_size_bytes: Option<u64>,
    public: bool,
    etag: Option<String>,
    last_modified: Option<SystemTime>,
    path: PathBuf,
}

fn etag_from_size_and_mtime(size: u64, mtime: Option<SystemTime>) -> String {
    // Deterministic ETag based on (size, mtime). This avoids hashing large images.
    //
    // Note: filesystems with coarse mtime resolution may not change this ETag for rapid edits.
    let mtime = mtime
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
        .unwrap_or((0, 0));

    format!(
        "\"{size:x}-{sec:x}-{nsec:x}\"",
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
    async fn list_images(&self) -> Result<Vec<ImageCatalogEntry>, StoreError> {
        if let Some(manifest) = self.load_manifest().await? {
            let mut out = Vec::with_capacity(manifest.images.len());
            for image in &manifest.images {
                let resolved = ResolvedImage {
                    id: image.id.clone(),
                    name: image.name.clone(),
                    description: image.description.clone(),
                    recommended_chunk_size_bytes: image.recommended_chunk_size_bytes,
                    public: image.public,
                    etag: image.etag.clone(),
                    last_modified: image.last_modified_time,
                    path: self.root.join(Path::new(&image.file)),
                };

                let meta = self
                    .meta_from_path(
                        &resolved.path,
                        resolved.etag.as_deref(),
                        resolved.last_modified,
                    )
                    .await?;

                out.push(ImageCatalogEntry {
                    id: resolved.id,
                    name: resolved.name,
                    description: resolved.description,
                    recommended_chunk_size_bytes: resolved.recommended_chunk_size_bytes,
                    public: resolved.public,
                    meta,
                });
            }
            return Ok(out);
        }

        let mut dir = fs::read_dir(&self.root).await?;
        let mut ids = Vec::<String>::new();
        while let Some(entry) = dir.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            if file_name == "manifest.json" {
                continue;
            }
            if validate_image_id(&file_name).is_err() {
                continue;
            }
            ids.push(file_name);
        }
        ids.sort();

        let mut out = Vec::with_capacity(ids.len());
        for image_id in ids {
            out.push(self.get_image(&image_id).await?);
        }
        Ok(out)
    }

    async fn get_image(&self, image_id: &str) -> Result<ImageCatalogEntry, StoreError> {
        let resolved = self.resolve_image(image_id).await?;
        let meta = self
            .meta_from_path(
                &resolved.path,
                resolved.etag.as_deref(),
                resolved.last_modified,
            )
            .await?;

        Ok(ImageCatalogEntry {
            id: resolved.id,
            name: resolved.name,
            description: resolved.description,
            recommended_chunk_size_bytes: resolved.recommended_chunk_size_bytes,
            public: resolved.public,
            meta,
        })
    }

    async fn get_meta(&self, image_id: &str) -> Result<ImageMeta, StoreError> {
        let resolved = self.resolve_image(image_id).await?;
        self.meta_from_path(
            &resolved.path,
            resolved.etag.as_deref(),
            resolved.last_modified,
        )
        .await
    }

    async fn open_range(
        &self,
        image_id: &str,
        start: u64,
        len: u64,
    ) -> Result<BoxedAsyncRead, StoreError> {
        let resolved = self.resolve_image(image_id).await?;
        let path = self.ensure_within_root(&resolved.path).await?;

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

        let too_long = "a".repeat(crate::store::MAX_IMAGE_ID_LEN + 1);
        let err = validate_image_id(&too_long).unwrap_err();
        match err {
            StoreError::InvalidImageId { image_id } => {
                assert!(
                    image_id.len() <= crate::store::MAX_IMAGE_ID_LEN,
                    "expected truncated image_id, got len={}",
                    image_id.len()
                );
            }
            other => panic!("expected InvalidImageId, got {other:?}"),
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

        let meta = store.get_meta(image_id).await.unwrap();
        assert_eq!(meta.size, size);
        assert_eq!(meta.content_type, CONTENT_TYPE_DISK_IMAGE);
        assert!(meta.etag.is_some());

        assert!(store.exists(image_id).await.unwrap());
        assert!(!store.exists("missing.img").await.unwrap());

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
            .truncate(true)
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
