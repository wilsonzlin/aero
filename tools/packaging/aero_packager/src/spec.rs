use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

pub(crate) fn normalize_driver_name(name: &str) -> String {
    // Guest Tools historically shipped the AeroGPU driver under `drivers/<arch>/aero-gpu/`.
    // The canonical directory name is now `aerogpu` to match the in-tree driver location and
    // INF naming (`drivers/aerogpu/...`, `aerogpu.inf`). Keep the legacy dashed form as an
    // alias for one release cycle so older specs continue to work.
    if name.eq_ignore_ascii_case("aero-gpu") {
        return "aerogpu".to_string();
    }
    name.to_string()
}

/// Packaging-time validation inputs.
///
/// The packager consumes driver build artifacts from the CI pipeline, but it
/// also needs a stable list of drivers to validate and include.
///
/// This spec is intentionally small; it should be easy to update without code
/// changes as drivers are added/renamed.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "PackagingSpecRaw")]
pub struct PackagingSpec {
    /// If true, optional drivers (i.e. `required=false`) must be present for both x86 and amd64.
    ///
    /// This prevents producing Guest Tools media where a driver is shipped for only one guest
    /// architecture, which can lead to confusing and inconsistent behaviour across x86/x64
    /// Windows guests.
    pub require_optional_drivers_on_all_arches: bool,
    pub drivers: Vec<DriverSpec>,
    #[serde(default)]
    pub fail_on_unlisted_driver_dirs: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverSpec {
    pub name: String,
    /// If true, missing driver artifacts are fatal. If false, missing driver
    /// artifacts are logged as a warning and skipped.
    pub required: bool,
    /// A list of INF filenames (no paths) that must be present in the packaged
    /// driver directory (per-architecture).
    ///
    /// Matching is case-insensitive.
    #[serde(default)]
    pub expected_inf_files: Vec<String>,
    /// A list of Windows service names that must appear in at least one
    /// non-comment `AddService = <svc>` directive across the packaged INF files
    /// (per-architecture).
    ///
    /// Matching is case-insensitive.
    #[serde(default)]
    pub expected_add_services: Vec<String>,
    /// Optional `guest-tools/config/devices.cmd` variable name to source an
    /// expected service name from.
    ///
    /// The variable's value is treated as a single service name and appended
    /// to `expected_add_services` if not already present.
    #[serde(default)]
    pub expected_add_services_from_devices_cmd_var: Option<String>,
    /// A list of regex patterns that must appear somewhere in at least one INF
    /// file for this driver (per-architecture).
    #[serde(default)]
    pub expected_hardware_ids: Vec<String>,
    /// Optional `guest-tools/config/devices.cmd` variable name to source expected HWIDs from.
    ///
    /// The variable's value is parsed as a list of space-separated tokens, where each token may be
    /// quoted. Each resulting HWID is normalized to the base `PCI\VEN_....&DEV_....` form (to avoid
    /// requiring SUBSYS/REV-qualified IDs to appear in INFs), then regex-escaped and appended to
    /// `expected_hardware_ids`.
    #[serde(default)]
    pub expected_hardware_ids_from_devices_cmd_var: Option<String>,
    /// By default, the packager skips common non-distributable build artifacts
    /// (PDBs, objs, source files, etc). If a driver needs one of these files to
    /// be present in the packaged directory, extensions can be explicitly
    /// allowlisted here (case-insensitive, with or without leading dots).
    #[serde(default)]
    pub allow_extensions: Vec<String>,
    /// Similar to `allow_extensions`, but matched against the driver-relative
    /// path (using `/` separators) as a case-insensitive regex.
    ///
    /// This is intended for rare cases where the driver layout needs an
    /// allowlist exception for a specific file name/path.
    #[serde(default)]
    pub allow_path_regexes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackagingSpecRaw {
    #[serde(default)]
    require_optional_drivers_on_all_arches: bool,
    #[serde(default)]
    fail_on_unlisted_driver_dirs: bool,
    /// New schema: unified driver list containing both required and optional drivers.
    #[serde(default)]
    drivers: Vec<DriverSpec>,
    /// Legacy schema: required drivers only. We treat these as `required = true`
    /// entries and merge them into `drivers`.
    #[serde(default)]
    required_drivers: Vec<LegacyRequiredDriver>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRequiredDriver {
    name: String,
    #[serde(default)]
    expected_hardware_ids: Vec<String>,
    #[serde(default)]
    expected_hardware_ids_from_devices_cmd_var: Option<String>,
}

impl From<PackagingSpecRaw> for PackagingSpec {
    fn from(raw: PackagingSpecRaw) -> Self {
        let require_optional_drivers_on_all_arches = raw.require_optional_drivers_on_all_arches;
        let fail_on_unlisted_driver_dirs = raw.fail_on_unlisted_driver_dirs;
        // Merge legacy `required_drivers` into the unified `drivers` list while
        // preserving the (already stable) JSON ordering:
        // - entries from `drivers` first
        // - then any additional entries from `required_drivers`
        //
        // If a driver appears in both lists, treat it as required and merge
        // expected HWID patterns.
        let mut out = Vec::new();
        let mut index_by_name = std::collections::HashMap::<String, usize>::new();

        for mut drv in raw.drivers {
            drv.name = normalize_driver_name(&drv.name);
            // Treat driver names as case-insensitive for merge purposes. This
            // matches our packaging-time validation (which rejects duplicates
            // case-insensitively) and avoids surprising failures if a spec
            // temporarily includes both `drivers` and legacy `required_drivers`
            // with different capitalization.
            index_by_name.insert(drv.name.to_ascii_lowercase(), out.len());
            out.push(drv);
        }

        for legacy in raw.required_drivers {
            let name = normalize_driver_name(&legacy.name);
            if let Some(idx) = index_by_name.get(&name.to_ascii_lowercase()).copied() {
                let existing = &mut out[idx];
                existing.required = true;
                for hwid in legacy.expected_hardware_ids {
                    if !existing.expected_hardware_ids.contains(&hwid) {
                        existing.expected_hardware_ids.push(hwid);
                    }
                }
                if existing
                    .expected_hardware_ids_from_devices_cmd_var
                    .is_none()
                {
                    existing.expected_hardware_ids_from_devices_cmd_var =
                        legacy.expected_hardware_ids_from_devices_cmd_var;
                }
                continue;
            }

            index_by_name.insert(name.to_ascii_lowercase(), out.len());
            out.push(DriverSpec {
                name,
                required: true,
                expected_inf_files: Vec::new(),
                expected_add_services: Vec::new(),
                expected_add_services_from_devices_cmd_var: None,
                expected_hardware_ids: legacy.expected_hardware_ids,
                expected_hardware_ids_from_devices_cmd_var: legacy
                    .expected_hardware_ids_from_devices_cmd_var,
                allow_extensions: Vec::new(),
                allow_path_regexes: Vec::new(),
            });
        }

        PackagingSpec {
            require_optional_drivers_on_all_arches,
            drivers: out,
            fail_on_unlisted_driver_dirs,
        }
    }
}

impl PackagingSpec {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
    }

    pub fn load_with_bytes(path: &Path) -> Result<(Self, Vec<u8>)> {
        let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let spec = serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok((spec, bytes))
    }
}
