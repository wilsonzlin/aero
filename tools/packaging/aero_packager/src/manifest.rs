use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningPolicy {
    None,
    TestSigning,
    NoIntegrityChecks,
}

impl SigningPolicy {
    pub fn certs_required(self) -> bool {
        self != SigningPolicy::None
    }
}

impl Default for SigningPolicy {
    fn default() -> Self {
        SigningPolicy::TestSigning
    }
}

impl std::fmt::Display for SigningPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SigningPolicy::None => "none",
            SigningPolicy::TestSigning => "testsigning",
            SigningPolicy::NoIntegrityChecks => "nointegritychecks",
        };
        f.write_str(s)
    }
}

impl std::str::FromStr for SigningPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(SigningPolicy::None),
            "testsigning" | "test-signing" => Ok(SigningPolicy::TestSigning),
            "nointegritychecks" | "no-integrity-checks" => Ok(SigningPolicy::NoIntegrityChecks),
            other => Err(format!(
                "invalid signing policy: {other} (expected: none|testsigning|nointegritychecks)"
            )),
        }
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
