use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub package: ManifestPackage,
    pub files: Vec<ManifestFileEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestPackage {
    pub name: String,
    pub version: String,
    pub build_id: String,
    pub source_date_epoch: i64,
}

#[derive(Debug, Clone, Serialize)]
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
        mut files: Vec<ManifestFileEntry>,
    ) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Self {
            schema_version: 1,
            package: ManifestPackage {
                name: "aero-guest-tools".to_string(),
                version,
                build_id,
                source_date_epoch,
            },
            files,
        }
    }
}
