use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SigningPolicy {
    /// Test-signed drivers. Requires shipping a certificate and (typically) enabling Test Signing
    /// on Windows 7 x64.
    #[value(name = "test", alias = "testsigning", alias = "test-signing")]
    Test,
    /// Production/WHQL-signed drivers. No custom certificate is expected.
    #[value(name = "production", alias = "prod", alias = "whql")]
    Production,
    /// No signing expectations. Used for development scenarios where drivers may not be signed.
    #[value(name = "none")]
    None,
}

impl SigningPolicy {
    pub fn certs_required(self) -> bool {
        matches!(self, SigningPolicy::Test)
    }
}

impl Default for SigningPolicy {
    fn default() -> Self {
        SigningPolicy::Test
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
    pub signing_policy: SigningPolicy,
    pub certs_required: bool,
    pub files: Vec<ManifestFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestPackage {
    pub name: String,
    pub version: String,
    pub build_id: String,
    pub source_date_epoch: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            schema_version: 2,
            package: ManifestPackage {
                name: "aero-guest-tools".to_string(),
                version,
                build_id,
                source_date_epoch,
            },
            signing_policy,
            certs_required: signing_policy.certs_required(),
            files,
        }
    }
}
