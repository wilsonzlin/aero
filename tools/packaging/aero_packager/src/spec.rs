use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Packaging-time validation inputs.
///
/// The packager consumes driver build artifacts from the CI pipeline, but it
/// also needs a stable list of "required" drivers to validate and include.
///
/// This spec is intentionally small; it should be easy to update without code
/// changes as drivers are added/renamed.
#[derive(Debug, Clone, Deserialize)]
pub struct PackagingSpec {
    #[serde(default)]
    pub required_drivers: Vec<RequiredDriver>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequiredDriver {
    pub name: String,
    /// A list of regex patterns that must appear somewhere in at least one INF
    /// file for this driver (per-architecture).
    #[serde(default)]
    pub expected_hardware_ids: Vec<String>,
}

impl PackagingSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
    }
}
