use std::collections::HashMap;

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
        let images = match parsed {
            ManifestFormat::V1 { images } => images,
            ManifestFormat::V0(images) => images,
        };

        if images.is_empty() {
            return Err(ManifestError::Empty);
        }

        let mut ids = HashMap::<&str, ()>::new();
        for image in &images {
            validate_id(&image.id)?;
            validate_file_path(&image.id, &image.file)?;
            if ids.insert(image.id.as_str(), ()).is_some() {
                return Err(ManifestError::DuplicateId(image.id.clone()));
            }
        }

        Ok(Self { images })
    }
}

fn validate_id(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() || id.len() > 128 || id == "." || id == ".." {
        return Err(ManifestError::InvalidId(id.to_string()));
    }

    let is_allowed = id.bytes().all(|b| {
        matches!(
            b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'
        )
    });

    if !is_allowed {
        return Err(ManifestError::InvalidId(id.to_string()));
    }
    Ok(())
}

fn validate_file_path(id: &str, file: &str) -> Result<(), ManifestError> {
    if file.is_empty() || file.len() > 512 || file.contains('\0') {
        return Err(ManifestError::InvalidFilePath {
            id: id.to_string(),
            file: file.to_string(),
        });
    }
    let path = std::path::Path::new(file);
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) | std::path::Component::CurDir => {}
            _ => {
                return Err(ManifestError::InvalidFilePath {
                    id: id.to_string(),
                    file: file.to_string(),
                })
            }
        }
    }
    Ok(())
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
}
