use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum SigningPolicy {
    /// Test-signed drivers. Requires shipping a certificate and (typically) enabling Test Signing
    /// on Windows 7 x64.
    #[default]
    #[value(name = "test", alias = "testsigning", alias = "test-signing")]
    Test,
    /// Production/WHQL-signed drivers. No custom certificate is expected.
    #[value(name = "production", alias = "prod", alias = "whql")]
    Production,
    /// No signing expectations. Used for development scenarios where drivers may not be signed.
    #[value(
        name = "none",
        alias = "nointegritychecks",
        alias = "no-integrity-checks"
    )]
    None,
}

impl SigningPolicy {
    pub fn certs_required(self) -> bool {
        matches!(self, SigningPolicy::Test)
    }
}

impl std::fmt::Display for SigningPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SigningPolicy::Test => "test",
            SigningPolicy::Production => "production",
            SigningPolicy::None => "none",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub package: ManifestPackage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inputs: Option<ManifestInputs>,
    pub signing_policy: SigningPolicy,
    pub certs_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ManifestProvenance>,
    pub files: Vec<ManifestFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestInputs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packaging_spec: Option<ManifestInputFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_device_contract: Option<ManifestWindowsDeviceContractInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aero_packager_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestInputFile {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestWindowsDeviceContractInput {
    pub path: String,
    pub sha256: String,
    pub contract_name: String,
    pub contract_version: String,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestProvenance {
    /// Packaging spec path as provided to the packager (string form).
    pub packaging_spec_path: String,
    /// SHA-256 of the packaging spec JSON after canonicalization (stable key ordering).
    pub packaging_spec_sha256: String,
    /// Windows device contract path as provided to the packager (string form).
    pub windows_device_contract_path: String,
    /// SHA-256 of the windows device contract JSON after canonicalization (stable key ordering).
    pub windows_device_contract_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPackage {
    pub name: String,
    pub version: String,
    pub build_id: String,
    pub source_date_epoch: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    pub fn new(
        version: String,
        build_id: String,
        source_date_epoch: i64,
        signing_policy: SigningPolicy,
        mut files: Vec<ManifestFileEntry>,
    ) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            schema_version: 4,
            package: ManifestPackage {
                name: "aero-guest-tools".to_string(),
                version,
                build_id,
                source_date_epoch,
            },
            inputs: None,
            provenance: None,
            signing_policy,
            certs_required: signing_policy.certs_required(),
            files,
        }
    }
}
