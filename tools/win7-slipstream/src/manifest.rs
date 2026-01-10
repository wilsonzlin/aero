use crate::cli::BackendKind;
use crate::unattend::UnattendMode;
use crate::wim::{Arch, SigningMode};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
}

impl ToolInfo {
    pub fn current() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME").to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateManifest {
    pub sha256: String,
    pub thumbprint_sha1: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub tool: ToolInfo,
    pub input_iso_sha256: String,
    pub driver_pack_sha256: String,
    pub signing_mode: SigningMode,
    pub arch: Arch,
    pub backend: BackendKind,
    pub unattend: UnattendMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificate: Option<CertificateManifest>,
    pub patched_paths: Vec<PatchedPath>,
}

impl Manifest {
    pub fn to_json_pretty(&self) -> Result<String> {
        let mut sorted = self.clone();
        sorted
            .patched_paths
            .sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.kind.cmp(&b.kind)));
        sorted
            .patched_paths
            .dedup_by(|a, b| a.path == b.path && a.kind == b.kind);
        serde_json::to_string_pretty(&sorted).context("Failed to serialize manifest as JSON")
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("Failed to parse AERO/MANIFEST.json")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchedPath {
    pub path: String,
    pub kind: PatchedPathKind,
}

impl PatchedPath {
    pub fn new_file(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: PatchedPathKind::File,
        }
    }

    pub fn new_dir(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: PatchedPathKind::Directory,
        }
    }
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum PatchedPathKind {
    File,
    Directory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_serializes() {
        let m = Manifest {
            tool: ToolInfo::current(),
            input_iso_sha256: "abc".to_string(),
            driver_pack_sha256: "def".to_string(),
            signing_mode: SigningMode::TestSigning,
            arch: Arch::X64,
            backend: BackendKind::CrossWimlib,
            unattend: UnattendMode::DriversOnly,
            certificate: None,
            patched_paths: vec![PatchedPath::new_file("boot/BCD"), PatchedPath::new_dir("AERO")],
        };
        let json = m.to_json_pretty().unwrap();
        assert!(json.contains("\"input_iso_sha256\""));
        assert!(json.contains("\"boot/BCD\""));
    }
}
