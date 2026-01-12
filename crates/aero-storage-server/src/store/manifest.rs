use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::http::HeaderValue;
use thiserror::Error;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Manifest {
    pub images: Vec<ManifestImage>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestImage {
    pub id: String,
    pub file: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub recommended_chunk_size_bytes: Option<u64>,
    #[serde(default = "default_public")]
    pub public: bool,
    /// Optional cache validator override.
    ///
    /// Must be a valid HTTP header value (and should be a quoted entity-tag, e.g. `"abc"`).
    #[serde(default)]
    pub etag: Option<String>,
    /// Optional last-modified override, provided as an RFC3339 timestamp (e.g. `2026-01-10T00:00:00Z`).
    #[serde(default)]
    pub last_modified: Option<String>,
    /// Parsed `last_modified` value, validated during [`Manifest::parse_str`].
    #[serde(skip)]
    pub last_modified_time: Option<SystemTime>,
}

fn default_public() -> bool {
    true
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("duplicate image id: {0}")]
    DuplicateId(String),
    #[error("invalid image id: {0}")]
    InvalidId(String),
    #[error("invalid file path for image {id}: {file}")]
    InvalidFilePath { id: String, file: String },
    #[error("manifest.json is required but missing at {path}")]
    Missing { path: String },
    #[error("invalid etag for image {id}: {etag:?}: {reason}")]
    InvalidEtag {
        id: String,
        etag: String,
        reason: String,
    },
    #[error("invalid last_modified for image {id}: {last_modified:?}: {reason}")]
    InvalidLastModified {
        id: String,
        last_modified: String,
        reason: String,
    },
    #[error("manifest must include at least one image")]
    Empty,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum ManifestFormat {
    V1 { images: Vec<ManifestImage> },
    V0(Vec<ManifestImage>),
}

impl Manifest {
    pub fn parse_str(json: &str) -> Result<Self, ManifestError> {
        let parsed: ManifestFormat = serde_json::from_str(json)?;
        let mut images = match parsed {
            ManifestFormat::V1 { images } => images,
            ManifestFormat::V0(images) => images,
        };

        if images.is_empty() {
            return Err(ManifestError::Empty);
        }

        let mut ids = HashMap::<String, ()>::new();
        for image in &mut images {
            validate_id(&image.id)?;
            validate_file_path(&image.id, &image.file)?;
            if let Some(etag) = image.etag.as_mut() {
                // Normalize so callers don't accidentally include leading/trailing whitespace that
                // would later break strict validator comparisons (e.g. `If-Range`).
                *etag = etag.trim().to_string();
                validate_etag(&image.id, etag)?;
            }
            if let Some(last_modified) = image.last_modified.as_mut() {
                *last_modified = last_modified.trim().to_string();
                let parsed = parse_last_modified_rfc3339(&image.id, last_modified)?;
                image.last_modified_time = Some(parsed);
            }

            if ids.insert(image.id.clone(), ()).is_some() {
                return Err(ManifestError::DuplicateId(image.id.clone()));
            }
        }

        Ok(Self { images })
    }
}

fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() || id.len() > super::MAX_IMAGE_ID_LEN || id == "." || id == ".." {
        return Err(ManifestError::InvalidId(super::truncate_for_error(
            id,
            super::MAX_IMAGE_ID_LEN,
        )));
    }

    let is_allowed = id.bytes().all(|b| {
        matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
        )
    });

    if !is_allowed {
        return Err(ManifestError::InvalidId(super::truncate_for_error(
            id,
            super::MAX_IMAGE_ID_LEN,
        )));
    }
    Ok(())
}

fn validate_file_path(id: &str, file: &str) -> Result<(), ManifestError> {
    if file.is_empty() || file.len() > 512 || file.contains('\0') {
        return Err(ManifestError::InvalidFilePath {
            id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
            file: super::truncate_for_error(file, 512),
        });
    }
    let path = std::path::Path::new(file);
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(ManifestError::InvalidFilePath {
                    id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
                    file: super::truncate_for_error(file, 512),
                })
            }
        }
    }
    Ok(())
}

fn validate_etag(id: &str, etag: &str) -> Result<(), ManifestError> {
    let etag = etag.trim();
    if etag.is_empty() {
        return Err(ManifestError::InvalidEtag {
            id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
            etag: super::truncate_for_error(etag, 512),
            reason: "etag must not be empty".to_string(),
        });
    }

    HeaderValue::from_str(etag).map_err(|err| ManifestError::InvalidEtag {
        id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
        etag: super::truncate_for_error(etag, 512),
        reason: err.to_string(),
    })?;

    // Enforce HTTP entity-tag format (RFC 9110 § 8.8.3). We validate this separately from
    // `HeaderValue` parsing so operators get a clear manifest error rather than surprising cache
    // misses at runtime.
    let tag = etag
        .strip_prefix("W/")
        .or_else(|| etag.strip_prefix("w/"))
        .unwrap_or(etag);
    let is_quoted = tag.starts_with('"') && tag.ends_with('"') && tag.len() >= 2;
    let inner_has_quote = is_quoted && tag[1..tag.len() - 1].contains('"');
    if !is_quoted || inner_has_quote {
        return Err(ManifestError::InvalidEtag {
            id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
            etag: super::truncate_for_error(etag, 512),
            reason: "etag must be a quoted HTTP entity-tag (e.g. \"v1\" or W/\"v1\")".to_string(),
        });
    }

    Ok(())
}

fn parse_last_modified_rfc3339(id: &str, last_modified: &str) -> Result<SystemTime, ManifestError> {
    let dt = time::OffsetDateTime::parse(
        last_modified,
        &time::format_description::well_known::Rfc3339,
    )
    .map_err(|err| ManifestError::InvalidLastModified {
        id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
        last_modified: super::truncate_for_error(last_modified, 256),
        reason: err.to_string(),
    })?;

    let nanos = dt.unix_timestamp_nanos();
    // Our HTTP `Last-Modified` header uses `httpdate::fmt_http_date`, which panics for times
    // before the Unix epoch. Reject pre-epoch values at manifest-load time to avoid runtime
    // panics and confusing caching behaviour.
    if nanos < 0 {
        return Err(ManifestError::InvalidLastModified {
            id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
            last_modified: super::truncate_for_error(last_modified, 256),
            reason: "last_modified must be at or after 1970-01-01T00:00:00Z".to_string(),
        });
    }

    system_time_from_unix_timestamp_nanos(nanos).map_err(|reason| {
        ManifestError::InvalidLastModified {
            id: super::truncate_for_error(id, super::MAX_IMAGE_ID_LEN),
            last_modified: super::truncate_for_error(last_modified, 256),
            reason,
        }
    })
}

fn system_time_from_unix_timestamp_nanos(nanos: i128) -> Result<SystemTime, String> {
    const NANOS_PER_SEC: u128 = 1_000_000_000;

    if nanos >= 0 {
        let nanos = nanos as u128;
        let secs = nanos / NANOS_PER_SEC;
        let sub_nanos = (nanos % NANOS_PER_SEC) as u32;
        let secs: u64 = secs
            .try_into()
            .map_err(|_| "timestamp out of range".to_string())?;
        let dur = std::time::Duration::new(secs, sub_nanos);
        UNIX_EPOCH
            .checked_add(dur)
            .ok_or_else(|| "timestamp out of range".to_string())
    } else {
        let nanos_abs = nanos
            .checked_abs()
            .ok_or_else(|| "timestamp out of range".to_string())? as u128;
        let secs = nanos_abs / NANOS_PER_SEC;
        let sub_nanos = (nanos_abs % NANOS_PER_SEC) as u32;
        let secs: u64 = secs
            .try_into()
            .map_err(|_| "timestamp out of range".to_string())?;
        let dur = std::time::Duration::new(secs, sub_nanos);
        UNIX_EPOCH
            .checked_sub(dur)
            .ok_or_else(|| "timestamp out of range".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v1_manifest() {
        let manifest = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "win7", "file": "win7.img", "name": "Windows 7", "public": true }
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.images.len(), 1);
        assert_eq!(manifest.images[0].id, "win7");
        assert_eq!(manifest.images[0].file, "win7.img");
        assert_eq!(manifest.images[0].name, "Windows 7");
    }

    #[test]
    fn parses_v0_manifest_array() {
        let manifest = Manifest::parse_str(
            r#"[
              { "id": "win7", "file": "win7.img", "name": "Windows 7", "public": true }
            ]"#,
        )
        .unwrap();

        assert_eq!(manifest.images.len(), 1);
        assert_eq!(manifest.images[0].id, "win7");
    }

    #[test]
    fn rejects_duplicate_ids() {
        let err = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "dup", "file": "a.img", "name": "A", "public": true },
                { "id": "dup", "file": "b.img", "name": "B", "public": true }
              ]
            }"#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::DuplicateId(_)));
    }

    #[test]
    fn rejects_path_traversal() {
        let err = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "bad", "file": "../secret.img", "name": "Bad", "public": true }
              ]
            }"#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::InvalidFilePath { .. }));
    }

    #[test]
    fn rejects_invalid_etag_header_value() {
        let err = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "bad", "file": "bad.img", "name": "Bad", "etag": "bad\netag", "public": true }
              ]
            }"#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::InvalidEtag { .. }));
    }

    #[test]
    fn rejects_unquoted_etag() {
        let err = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "bad", "file": "bad.img", "name": "Bad", "etag": "unquoted", "public": true }
              ]
            }"#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::InvalidEtag { .. }));
    }

    #[test]
    fn trims_etag_whitespace() {
        let manifest = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "disk", "file": "disk.img", "name": "Disk", "etag": "   \"v1\"  ", "public": true }
              ]
            }"#,
        )
        .unwrap();

        assert_eq!(manifest.images[0].etag.as_deref(), Some("\"v1\""));
    }

    #[test]
    fn invalid_id_error_is_truncated() {
        let long_id = "a".repeat(super::super::MAX_IMAGE_ID_LEN + 10);
        let json = format!(
            r#"{{
              "images": [
                {{ "id": "{long_id}", "file": "disk.img", "name": "Disk", "public": true }}
              ]
            }}"#
        );

        let err = Manifest::parse_str(&json).unwrap_err();
        let ManifestError::InvalidId(id) = err else {
            panic!("expected InvalidId, got {err:?}");
        };
        assert!(
            id.len() <= super::super::MAX_IMAGE_ID_LEN,
            "expected truncated id, got len={}",
            id.len()
        );
    }

    #[test]
    fn rejects_pre_epoch_last_modified() {
        let err = Manifest::parse_str(
            r#"{
              "images": [
                { "id": "bad", "file": "bad.img", "name": "Bad", "last_modified": "1969-12-31T23:59:59Z", "public": true }
              ]
            }"#,
        )
        .unwrap_err();

        assert!(matches!(err, ManifestError::InvalidLastModified { .. }));
    }
}
