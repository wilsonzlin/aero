mod guest_tools_config;
mod iso9660;
mod iso_from_dir;
mod manifest;
mod spec;
mod windows_device_contract;
mod zip_util;

use anyhow::{bail, Context, Result};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub use guest_tools_config::generate_guest_tools_devices_cmd_bytes;
pub use guest_tools_config::generate_guest_tools_devices_cmd_bytes_with_overrides;
pub use guest_tools_config::GuestToolsDevicesCmdServiceOverrides;
pub use iso9660::read_joliet_file_entries;
pub use iso9660::read_joliet_tree;
pub use iso9660::{IsoFileEntry, IsoFileTree};
pub use iso_from_dir::write_iso9660_joliet_from_dir;
pub use manifest::{
    Manifest, ManifestFileEntry, ManifestInputFile, ManifestInputs,
    ManifestWindowsDeviceContractInput, SigningPolicy,
};
pub use spec::{DriverSpec, PackagingSpec};

const CANONICAL_WINDOWS_DEVICE_CONTRACT_NAME: &str = "aero-windows-pci-device-contract";

/// Configuration for producing the distributable "Aero Drivers / Guest Tools" media.
#[derive(Debug, Clone)]
pub struct PackageConfig {
    pub drivers_dir: PathBuf,
    pub guest_tools_dir: PathBuf,
    pub windows_device_contract_path: PathBuf,
    pub out_dir: PathBuf,
    pub spec_path: PathBuf,
    pub version: String,
    pub build_id: String,
    pub volume_id: String,
    pub signing_policy: SigningPolicy,
    /// Seconds since Unix epoch used for timestamps inside the ISO/zip.
    pub source_date_epoch: i64,
}

#[derive(Debug, Clone)]
pub struct PackageOutputs {
    pub iso_path: PathBuf,
    pub zip_path: PathBuf,
    pub manifest_path: PathBuf,
}

#[derive(Debug, Clone)]
struct FileToPackage {
    /// Package-relative path using `/` separators.
    rel_path: String,
    bytes: Vec<u8>,
}

fn bail_if_symlink(entry: &walkdir::DirEntry, packaged_root: &str) -> Result<()> {
    if entry.file_type().is_symlink() {
        bail!(
            "refusing to package symlink {} (found under {packaged_root}); replace the symlink with a real file or remove it",
            entry.path().display(),
        );
    }
    Ok(())
}

fn guest_tools_devices_cmd_service_overrides_for_spec(
    contract: &windows_device_contract::WindowsDeviceContract,
    spec: &PackagingSpec,
) -> GuestToolsDevicesCmdServiceOverrides {
    // The Windows device contract is the source of truth for `config/devices.cmd` service
    // names. Only apply spec-based overrides for legacy/back-compat behaviour when using
    // the canonical in-repo contract.
    //
    // Virtio-win Guest Tools builds should pass `docs/windows-device-contract-virtio-win.json`
    // (or a derived override) so the contract itself controls service naming.
    if !contract
        .contract_name
        .trim()
        .eq_ignore_ascii_case(CANONICAL_WINDOWS_DEVICE_CONTRACT_NAME)
    {
        return GuestToolsDevicesCmdServiceOverrides::default();
    }

    let driver_names: HashSet<String> = spec
        .drivers
        .iter()
        .map(|d| d.name.trim().to_ascii_lowercase())
        .collect();

    // Legacy/back-compat:
    //
    // Some historical virtio-win packaging flows used the canonical in-repo device contract
    // (`docs/windows-device-contract.json`) and relied on *spec-based* overrides to swap
    // `AERO_VIRTIO_*_SERVICE` values to the upstream virtio-win service names.
    //
    // Modern virtio-win packaging should instead pass the virtio-win contract variant
    // (`docs/windows-device-contract-virtio-win.json`) (or a derived override generated from real
    // INFs) so the contract remains the source of truth for `config/devices.cmd`.
    let mut service_overrides = GuestToolsDevicesCmdServiceOverrides::default();
    if driver_names.contains("viostor") {
        service_overrides.virtio_blk_service = Some("viostor".to_string());
    }
    if driver_names.contains("netkvm") {
        service_overrides.virtio_net_service = Some("netkvm".to_string());
    }
    if driver_names.contains("vioinput") {
        service_overrides.virtio_input_service = Some("vioinput".to_string());
    }
    if driver_names.contains("viosnd") {
        service_overrides.virtio_snd_service = Some("viosnd".to_string());
    }

    service_overrides
}

/// Create `aero-guest-tools.iso`, `aero-guest-tools.zip`, and `manifest.json` in `out_dir`.
pub fn package_guest_tools(config: &PackageConfig) -> Result<PackageOutputs> {
    fs::create_dir_all(&config.out_dir)
        .with_context(|| format!("create output dir {}", config.out_dir.display()))?;

    let (spec, spec_bytes) =
        PackagingSpec::load_with_bytes(&config.spec_path).with_context(|| "load packaging spec")?;
    let spec_sha256 = sha256_hex(
        &canonicalize_json_bytes(&spec_bytes)
            .with_context(|| format!("canonicalize JSON {}", config.spec_path.display()))?,
    );

    let (contract, contract_bytes) = windows_device_contract::load_windows_device_contract_with_bytes(
        &config.windows_device_contract_path,
    )
    .with_context(|| {
        format!(
            "load windows device contract {}",
            config.windows_device_contract_path.display()
        )
    })?;
    let contract_sha256 = sha256_hex(
        &canonicalize_json_bytes(&contract_bytes).with_context(|| {
            format!(
                "canonicalize JSON {}",
                config.windows_device_contract_path.display()
            )
        })?,
    );

    let service_overrides = guest_tools_devices_cmd_service_overrides_for_spec(&contract, &spec);
    let devices_cmd_bytes = generate_guest_tools_devices_cmd_bytes_with_overrides(
        &config.windows_device_contract_path,
        &service_overrides,
    )
    .with_context(|| {
        format!(
            "generate config/devices.cmd from {}",
            config.windows_device_contract_path.display()
        )
    })?;
    let generated_devices_cmd_vars =
        parse_devices_cmd_vars(String::from_utf8_lossy(&devices_cmd_bytes).as_ref());

    let devices_cmd_path = config.guest_tools_dir.join("config").join("devices.cmd");
    let mut devices_cmd_vars = if devices_cmd_path.is_file() {
        read_devices_cmd_vars(&devices_cmd_path)
            .with_context(|| "read guest-tools/config/devices.cmd")?
    } else {
        // config/devices.cmd is packaged from the Windows device contract, but we still allow
        // optional spec fixtures to define additional variables in the on-disk file.
        HashMap::new()
    };
    // Use the contract-generated devices.cmd values as the source of truth for any overlapping
    // variables, while still allowing test fixtures to define extra variables.
    devices_cmd_vars.extend(generated_devices_cmd_vars);

    let driver_plan = validate_drivers(&spec, &config.drivers_dir, &devices_cmd_vars)
        .with_context(|| "validate driver artifacts")?;

    let mut files = collect_files(config, &driver_plan, devices_cmd_bytes)?;
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Ensure all packaged paths are safe to unpack and use on Windows (target guest),
    // even when packaging on Unix hosts where file naming rules differ.
    for f in &files {
        validate_windows_safe_rel_path(&f.rel_path)
            .with_context(|| format!("validate package path {}", f.rel_path))?;
    }

    // Hash all files that will be included, except the manifest which is generated below.
    let file_entries: Vec<ManifestFileEntry> = files
        .iter()
        .map(|f| ManifestFileEntry {
            path: f.rel_path.clone(),
            sha256: sha256_hex(&f.bytes),
            size: f.bytes.len() as u64,
        })
        .collect();

    let manifest = Manifest::new(
        config.version.clone(),
        config.build_id.clone(),
        config.source_date_epoch,
        config.signing_policy,
        file_entries,
    );
    let mut manifest = manifest;
    manifest.inputs = Some(ManifestInputs {
        packaging_spec: Some(ManifestInputFile {
            path: manifest_input_path(&config.spec_path)?,
            sha256: spec_sha256,
        }),
        windows_device_contract: Some(ManifestWindowsDeviceContractInput {
            path: manifest_input_path(&config.windows_device_contract_path)?,
            sha256: contract_sha256,
            contract_name: contract.contract_name.clone(),
            contract_version: contract.contract_version.clone(),
            schema_version: contract.schema_version,
        }),
        aero_packager_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    });

    let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest.json")?;
    let manifest_file = FileToPackage {
        rel_path: "manifest.json".to_string(),
        bytes: manifest_bytes.clone(),
    };
    files.push(manifest_file);
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    ensure_no_case_insensitive_path_collisions(&files)?;

    let iso_path = config.out_dir.join("aero-guest-tools.iso");
    let zip_path = config.out_dir.join("aero-guest-tools.zip");
    let manifest_path = config.out_dir.join("manifest.json");

    fs::write(&manifest_path, &manifest_bytes)
        .with_context(|| format!("write {}", manifest_path.display()))?;

    zip_util::write_deterministic_zip(&zip_path, config.source_date_epoch, &files)
        .with_context(|| format!("write {}", zip_path.display()))?;

    iso9660::write_iso9660_joliet(
        &iso_path,
        &config.volume_id,
        config.source_date_epoch,
        &files,
    )
    .with_context(|| format!("write {}", iso_path.display()))?;

    Ok(PackageOutputs {
        iso_path,
        zip_path,
        manifest_path,
    })
}

fn ensure_no_case_insensitive_path_collisions(files: &[FileToPackage]) -> Result<()> {
    // Windows (and many zip extractors targeting Windows semantics) treat file paths as
    // case-insensitive. If we emit two entries that differ only by case, extraction can silently
    // drop/overwrite one of them.
    //
    // Additionally, zip archives contain explicit directory entries. Two different directory
    // casings (e.g. `Foo/` vs `foo/`) can also cause extraction issues, so include implied
    // directories in the collision check as well.
    let mut by_lower: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for f in files {
        let rel_path = f.rel_path.as_str();
        by_lower
            .entry(rel_path.to_ascii_lowercase())
            .or_default()
            .insert(rel_path.to_string());

        let parts: Vec<&str> = rel_path.split('/').collect();
        if parts.len() > 1 {
            for i in 1..parts.len() {
                let dir = parts[..i].join("/");
                let key = dir.to_ascii_lowercase();
                by_lower
                    .entry(key)
                    .or_default()
                    .insert(format!("{dir}/"));
            }
        }
    }

    let collisions: Vec<(String, Vec<String>)> = by_lower
        .into_iter()
        .filter_map(|(lower, paths)| {
            if paths.len() > 1 {
                Some((lower, paths.into_iter().collect()))
            } else {
                None
            }
        })
        .collect();

    if collisions.is_empty() {
        return Ok(());
    }

    let mut msg = String::new();
    msg.push_str("case-insensitive path collision(s) detected in packaged payload:\n");
    for (lower, mut paths) in collisions {
        paths.sort();
        msg.push_str(&format!("  {lower}:\n"));
        for p in paths {
            msg.push_str(&format!("    - {p}\n"));
        }
    }
    msg.push_str(
        "\nRemediation: rename or remove one of the colliding files/directories so that all \
         packaged paths are unique when compared case-insensitively.\n",
    );

    bail!("{msg}");
}

#[derive(Debug, Clone)]
struct DriverToInclude {
    spec: DriverSpec,
    dir: PathBuf,
}

#[derive(Debug, Clone)]
struct DriverPlan {
    x86: Vec<DriverToInclude>,
    amd64: Vec<DriverToInclude>,
}

fn collect_files(
    config: &PackageConfig,
    driver_plan: &DriverPlan,
    devices_cmd_bytes: Vec<u8>,
) -> Result<Vec<FileToPackage>> {
    let mut out = Vec::new();

    // Guest tools top-level scripts/doc.
    //
    // Keep this list in sync with the published Guest Tools ISO root.
    for file_name in [
        "setup.cmd",
        "uninstall.cmd",
        "verify.cmd",
        "verify.ps1",
        "README.md",
        "THIRD_PARTY_NOTICES.md",
    ] {
        let src = config.guest_tools_dir.join(file_name);
        if !src.is_file() {
            bail!(
                "guest tools missing required file: {}",
                format!("{src:?}")
            );
        }
        out.push(FileToPackage {
            rel_path: file_name.to_string(),
            bytes: fs::read(&src).with_context(|| format!("read {}", src.display()))?,
        });
    }

    let config_dir = config.guest_tools_dir.join("config");
    if !config_dir.is_dir() {
        bail!(
            "guest tools missing required directory: {}",
            format!("{config_dir:?}")
        );
    }

    out.push(FileToPackage {
        rel_path: "config/devices.cmd".to_string(),
        bytes: devices_cmd_bytes,
    });
    for entry in walkdir::WalkDir::new(&config_dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        bail_if_symlink(&entry, "guest_tools/config")?;
        if !entry.file_type().is_file() {
            continue;
        }

        let ext = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !ext.is_empty() && is_private_key_extension(&ext) {
            bail!(
                "refusing to package private key material in config directory: {}",
                entry.path().display()
            );
        }

        let rel = entry
            .path()
            .strip_prefix(&config.guest_tools_dir)
            .expect("walkdir under guest_tools_dir");
        let rel_str = path_to_slash(rel, entry.path())?;
        if !should_include_guest_tools_tree_file(entry.path(), &rel_str) {
            continue;
        }
        if rel_str.eq_ignore_ascii_case("config/devices.cmd") {
            continue;
        }
        out.push(FileToPackage {
            rel_path: rel_str,
            bytes: fs::read(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        });
    }

    // Optional: third-party license texts / attribution files.
    // Keep this optional so local packager runs can still succeed with repo-only assets.
    let licenses_dir = config.guest_tools_dir.join("licenses");
    if licenses_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&licenses_dir)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = entry?;
            bail_if_symlink(&entry, "guest_tools/licenses")?;
            if !entry.file_type().is_file() {
                continue;
            }

            let ext = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !ext.is_empty() && is_private_key_extension(&ext) {
                bail!(
                    "refusing to package private key material in licenses directory: {}",
                    entry.path().display()
                );
            }

            let rel = entry
                .path()
                .strip_prefix(&config.guest_tools_dir)
                .expect("walkdir under guest_tools_dir");
            let rel_str = path_to_slash(rel, entry.path())?;
            if !should_include_guest_tools_tree_file(entry.path(), &rel_str) {
                continue;
            }
            out.push(FileToPackage {
                rel_path: rel_str,
                bytes: fs::read(entry.path())
                    .with_context(|| format!("read {}", entry.path().display()))?,
            });
        }
    }

    // Certificates.
    let certs_dir = config.guest_tools_dir.join("certs");
    if certs_dir.is_dir() {
        let mut certs = Vec::new();
        let mut found_cert = false;
        for entry in walkdir::WalkDir::new(&certs_dir)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = entry?;
            bail_if_symlink(&entry, "guest_tools/certs")?;
            if !entry.file_type().is_file() {
                continue;
            }

            let ext = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !ext.is_empty() && is_private_key_extension(&ext) {
                bail!(
                    "refusing to package private key material in certs directory: {}",
                    entry.path().display()
                );
            }
            let rel = entry
                .path()
                .strip_prefix(&certs_dir)
                .expect("walkdir under certs_dir");
            let rel_str = path_to_slash(rel, entry.path())?;
            let lower = rel_str.to_ascii_lowercase();
            // Include only public certificate artifacts and docs (no private keys).
            let is_cert =
                lower.ends_with(".cer") || lower.ends_with(".crt") || lower.ends_with(".p7b");
            let is_doc = lower == "readme.md";
            if !(is_cert || is_doc) {
                continue;
            }
            if is_cert {
                found_cert = true;
                if !config.signing_policy.certs_required() {
                    bail!(
                        "refusing to package certificate file for signing_policy={}: certs/{}.\n\
                         Guest Tools media built with signing_policy=production/none must not ship trust anchors.\n\
                         Remediation: remove all *.cer/*.crt/*.p7b files from {} (keep certs/README.md if needed), \
                         or re-run with --signing-policy test.",
                        config.signing_policy,
                        rel_str,
                        certs_dir.display(),
                    );
                }
            }
            certs.push(FileToPackage {
                rel_path: format!("certs/{}", rel_str),
                bytes: fs::read(entry.path())
                    .with_context(|| format!("read {}", entry.path().display()))?,
            });
        }
        if config.signing_policy.certs_required() && !found_cert {
            bail!(
                "guest tools certs directory contains no certificate files (*.cer/*.crt/*.p7b), \
                 but signing_policy={} requires at least one: {}",
                config.signing_policy,
                format!("{certs_dir:?}"),
            );
        }
        out.extend(certs);
    } else if config.signing_policy.certs_required() {
        bail!(
            "guest tools missing required directory: {}",
            format!("{certs_dir:?}")
        );
    }
    // Optional: include documentation alongside the packaged driver tree.
    // (Driver binaries themselves come from `drivers_dir`.)
    let drivers_readme = config.guest_tools_dir.join("drivers").join("README.md");
    if drivers_readme.is_file() {
        out.push(FileToPackage {
            rel_path: "drivers/README.md".to_string(),
            bytes: fs::read(&drivers_readme)
                .with_context(|| format!("read {}", drivers_readme.display()))?,
        });
    }

    // Optional: extra guest-side tools / utilities (e.g. debug/selftest helpers).
    //
    // These are packaged verbatim under `tools/...` so they can ship alongside Guest Tools media
    // without being part of any driver package directory.
    let tools_dir = config.guest_tools_dir.join("tools");
    if tools_dir.is_dir() {
        let allowlist = DriverFileAllowlist::default();
        for entry in walkdir::WalkDir::new(&tools_dir)
            .follow_links(false)
            .sort_by_file_name()
        {
            let entry = entry?;
            bail_if_symlink(&entry, "guest_tools/tools")?;
            if !entry.file_type().is_file() {
                continue;
            }

            // Keep the same safety filters as the driver tree:
            // - refuse private key material
            // - skip hidden files/dirs + host metadata
            // - exclude build artifacts by default
            let rel = entry
                .path()
                .strip_prefix(&config.guest_tools_dir)
                .expect("walkdir under guest_tools_dir");
            let rel_str = path_to_slash(rel, entry.path())?;
            if !should_include_driver_file(entry.path(), &rel_str, &allowlist)
                .with_context(|| format!("filter guest tools file {}", entry.path().display()))?
            {
                continue;
            }

            out.push(FileToPackage {
                rel_path: rel_str,
                bytes: fs::read(entry.path())
                    .with_context(|| format!("read {}", entry.path().display()))?,
            });
        }
    }

    // Drivers.
    for (arch_out, drivers) in [("x86", &driver_plan.x86), ("amd64", &driver_plan.amd64)] {
        for driver in drivers {
            let packaged_root = format!("drivers/{arch_out}/{}", &driver.spec.name);
            let allowlist =
                DriverFileAllowlist::from_driver_spec(&driver.spec).with_context(|| {
                    format!(
                        "load file allowlist overrides for driver {}",
                        &driver.spec.name
                    )
                })?;

            for entry in walkdir::WalkDir::new(&driver.dir)
                .follow_links(false)
                .sort_by_file_name()
            {
                let entry = entry?;
                bail_if_symlink(&entry, &packaged_root)?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&driver.dir)
                    .expect("walkdir under driver dir");
                let rel_str = path_to_slash(rel, entry.path())?;
                if !should_include_driver_file(entry.path(), &rel_str, &allowlist)
                    .with_context(|| format!("filter driver file {}", entry.path().display()))?
                {
                    continue;
                }

                out.push(FileToPackage {
                    rel_path: format!("drivers/{}/{}/{}", arch_out, &driver.spec.name, rel_str),
                    bytes: fs::read(entry.path())
                        .with_context(|| format!("read {}", entry.path().display()))?,
                });
            }
        }
    }

    Ok(out)
}

fn validate_drivers(
    spec: &PackagingSpec,
    drivers_dir: &Path,
    devices_cmd_vars: &HashMap<String, String>,
) -> Result<DriverPlan> {
    if spec.drivers.is_empty() {
        bail!("packaging spec contains no drivers");
    }

    let mut seen_driver_names = HashSet::<String>::new();
    for drv in &spec.drivers {
        let name = drv.name.trim();
        if drv.name != name {
            bail!(
                "packaging spec driver name must not contain leading/trailing whitespace: {:?}",
                &drv.name
            );
        }
        if name.is_empty() {
            bail!("packaging spec contains a driver with an empty name");
        }
        if name == "." || name == ".." {
            bail!(
                "packaging spec contains an invalid driver name: {}",
                drv.name
            );
        }
        if name.contains('/') || name.contains('\\') {
            bail!(
                "packaging spec contains an invalid driver name containing path separators: {}",
                drv.name
            );
        }

        // Deduplicate case-insensitively to avoid surprises on Windows hosts.
        let key = name.to_ascii_lowercase();
        if !seen_driver_names.insert(key) {
            bail!(
                "packaging spec lists the same driver multiple times (case-insensitive): {}",
                drv.name
            );
        }
    }

    let drivers_x86_dir = resolve_input_arch_dir(drivers_dir, "x86")
        .with_context(|| "resolve driver input directory for x86")?;
    let drivers_amd64_dir = resolve_input_arch_dir(drivers_dir, "amd64")
        .with_context(|| "resolve driver input directory for amd64")?;

    if spec.fail_on_unlisted_driver_dirs {
        fn is_known_non_driver_entry(name: &str) -> bool {
            // Ignore hidden metadata dirs/files (`.git`, `.DS_Store`, `._*`, etc) and
            // common macOS zip extraction artifacts.
            if name.starts_with('.') {
                return true;
            }
            name.eq_ignore_ascii_case("__MACOSX")
        }

        let listed_driver_names: HashSet<String> = spec
            .drivers
            .iter()
            .map(|d| spec::normalize_driver_name(d.name.trim()).to_ascii_lowercase())
            .collect();

        let mut extras: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (arch, arch_dir) in [("x86", &drivers_x86_dir), ("amd64", &drivers_amd64_dir)] {
            for entry in fs::read_dir(arch_dir)
                .with_context(|| format!("read driver input directory {}", arch_dir.display()))?
            {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if !file_type.is_dir() {
                    continue;
                }
                let name_os = entry.file_name();
                let name = name_os.to_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "drivers_dir contains a directory name that is not valid UTF-8 ({}): {:?}",
                        arch_dir.display(),
                        entry.path()
                    )
                })?;
                if is_known_non_driver_entry(name) {
                    continue;
                }
                let normalized = spec::normalize_driver_name(name).to_ascii_lowercase();
                if !listed_driver_names.contains(&normalized) {
                    extras
                        .entry(normalized)
                        .or_default()
                        .insert(arch.to_string());
                }
            }
        }

        if !extras.is_empty() {
            let mut msg = format!(
                "drivers_dir contains driver directories that are not listed in the packaging spec \
                 (fail_on_unlisted_driver_dirs=true).\n\
                 \n\
                 This usually means `--drivers-dir` points at the wrong root (for example, a parent \
                 directory containing multiple driver bundles) and the extra folders would be \
                 silently ignored by the spec.\n\
                 \n\
                 drivers_dir: {}\n\
                 x86 driver input dir: {}\n\
                 amd64 driver input dir: {}\n\
                 \n\
                 Unlisted driver directories:",
                drivers_dir.display(),
                drivers_x86_dir.display(),
                drivers_amd64_dir.display(),
            );

            for (name, archs) in extras {
                let archs = archs.into_iter().collect::<Vec<_>>().join(", ");
                msg.push_str(&format!("\n- {name} (found under: {archs})"));
            }
            bail!("{msg}");
        }
    }

    let mut plan = DriverPlan {
        x86: Vec::new(),
        amd64: Vec::new(),
    };

    fn legacy_driver_dir_aliases(name: &str) -> &'static [&'static str] {
        if name.eq_ignore_ascii_case("aerogpu") {
            // Guest Tools historically used `aero-gpu` as the AeroGPU driver directory name.
            // Accept the legacy dashed form as an input alias for one release cycle so old
            // driver bundles can still be repackaged without renaming.
            &["aero-gpu"]
        } else {
            &[]
        }
    }

    fn driver_dir_candidates(arch_dir: &Path, name: &str) -> Vec<PathBuf> {
        let mut out = vec![arch_dir.join(name)];
        for alias in legacy_driver_dir_aliases(name) {
            out.push(arch_dir.join(alias));
        }
        out
    }

    fn resolve_driver_dir(arch: &str, arch_dir: &Path, name: &str) -> Result<Option<PathBuf>> {
        let mut matches: Vec<(&'static str, PathBuf)> = Vec::new();
        let primary = arch_dir.join(name);
        if primary.is_dir() {
            matches.push(("", primary));
        }
        for alias in legacy_driver_dir_aliases(name) {
            let p = arch_dir.join(alias);
            if p.is_dir() {
                matches.push((alias, p));
            }
        }

        if matches.is_empty() {
            return Ok(None);
        }
        if matches.len() > 1 {
            let paths = matches
                .iter()
                .map(|(alias, p)| {
                    if alias.is_empty() {
                        p.display().to_string()
                    } else {
                        format!("{} (legacy alias)", p.display())
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "multiple driver directories found for {} ({}); remove/rename one of: {}",
                name,
                arch,
                paths
            );
        }

        let (alias, path) = matches.pop().expect("non-empty");
        if !alias.is_empty() {
            eprintln!(
                "warning: using legacy driver directory name '{}' for '{}' ({})",
                alias, name, arch
            );
        }

        Ok(Some(path))
    }

    for drv in &spec.drivers {
        let x86_driver_dir = resolve_driver_dir("x86", &drivers_x86_dir, &drv.name)?;
        let amd64_driver_dir = resolve_driver_dir("amd64", &drivers_amd64_dir, &drv.name)?;

        if !drv.required
            && spec.require_optional_drivers_on_all_arches
            && (x86_driver_dir.is_some() ^ amd64_driver_dir.is_some())
        {
            let (present_arch, missing_arch, missing_arch_dir) = if x86_driver_dir.is_some() {
                ("x86", "amd64", &drivers_amd64_dir)
            } else {
                ("amd64", "x86", &drivers_x86_dir)
            };
            let tried = driver_dir_candidates(missing_arch_dir, &drv.name)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "optional driver directory is present for {present_arch} but missing for {missing_arch}: {} (require_optional_drivers_on_all_arches=true); tried: {tried}",
                drv.name
            );
        }

        // x86
        if let Some(driver_dir) = x86_driver_dir {
            validate_driver_dir(drv, "x86", &driver_dir, devices_cmd_vars)?;
            plan.x86.push(DriverToInclude {
                spec: drv.clone(),
                dir: driver_dir,
            });
        } else if drv.required {
            let tried = driver_dir_candidates(&drivers_x86_dir, &drv.name)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "required driver directory missing: {} (x86); tried: {}",
                drv.name,
                tried
            );
        } else {
            eprintln!("warning: optional driver directory missing: {} (x86)", drv.name);
        }

        // amd64
        if let Some(driver_dir) = amd64_driver_dir {
            validate_driver_dir(drv, "amd64", &driver_dir, devices_cmd_vars)?;
            plan.amd64.push(DriverToInclude {
                spec: drv.clone(),
                dir: driver_dir,
            });
        } else if drv.required {
            let tried = driver_dir_candidates(&drivers_amd64_dir, &drv.name)
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "required driver directory missing: {} (amd64); tried: {}",
                drv.name,
                tried
            );
        } else {
            eprintln!("warning: optional driver directory missing: {} (amd64)", drv.name);
        }
    }

    Ok(plan)
}

fn validate_driver_dir(
    driver: &DriverSpec,
    arch: &str,
    driver_dir: &Path,
    devices_cmd_vars: &HashMap<String, String>,
) -> Result<()> {
    let allowlist = DriverFileAllowlist::from_driver_spec(driver)
        .with_context(|| format!("load file allowlist overrides for driver {}", driver.name))?;

    let mut found_inf = false;
    let mut found_sys = false;
    let mut found_cat = false;
    let mut infs = Vec::<(String, String)>::new();
    let mut all_inf_paths = Vec::<String>::new();
    let mut all_inf_base_names = HashSet::<String>::new();
    let mut packaged_inf_base_names = HashSet::<String>::new();

    // Collect a view of the files that would be packaged for this driver, so we can
    // validate that any INF-referenced payloads are actually present.
    let mut packaged_rel_paths = HashSet::<String>::new();
    let mut packaged_base_names = HashSet::<String>::new();

    let packaged_root = format!("drivers/{arch}/{}", driver.name);
    for entry in walkdir::WalkDir::new(driver_dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        bail_if_symlink(&entry, &packaged_root)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(driver_dir)
            .expect("walkdir under driver_dir");
        let rel_str = path_to_slash(rel, path)?;
        let include = should_include_driver_file(path, &rel_str, &allowlist)
            .with_context(|| format!("filter driver file {}", path.display()))?;

        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".inf") {
                all_inf_paths.push(rel_str.clone());
                all_inf_base_names.insert(lower);
            }
        }

        if include {
            packaged_rel_paths.insert(rel_str.to_ascii_lowercase());
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                packaged_base_names.insert(name.to_ascii_lowercase());
            }

            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".inf") {
                found_inf = true;
                packaged_inf_base_names.insert(lower);
                let text = read_inf_text(path)
                    .with_context(|| format!("read INF for {} ({})", driver.name, arch))?;
                infs.push((rel_str, text));
            } else if lower.ends_with(".sys") {
                found_sys = true;
            } else if lower.ends_with(".cat") {
                found_cat = true;
            }
        }
    }

    if !found_inf || !found_sys || !found_cat {
        bail!(
            "driver {} ({}) is incomplete: expected at least one .inf, .sys, and .cat",
            driver.name,
            arch
        );
    }

    let scanned_packaged_inf_paths = || -> String {
        let mut paths: Vec<String> = infs.iter().map(|(p, _)| p.clone()).collect();
        paths.sort();
        if paths.is_empty() {
            "<none>".to_string()
        } else {
            paths.join(", ")
        }
    };

    if !driver.expected_inf_files.is_empty() {
        let mut missing = Vec::<String>::new();
        let mut excluded = Vec::<String>::new();

        for raw_expected in &driver.expected_inf_files {
            let expected = raw_expected.trim();
            if expected.is_empty() {
                bail!(
                    "driver {} ({}) expected_inf_files contains an empty entry",
                    driver.name,
                    arch
                );
            }
            if expected.contains('/') || expected.contains('\\') {
                bail!(
                    "driver {} ({}) expected_inf_files entry must be a filename without paths, got: {expected}",
                    driver.name,
                    arch
                );
            }
            if !expected.to_ascii_lowercase().ends_with(".inf") {
                bail!(
                    "driver {} ({}) expected_inf_files entry must end with .inf, got: {expected}",
                    driver.name,
                    arch
                );
            }

            let expected_lower = expected.to_ascii_lowercase();
            if !all_inf_base_names.contains(&expected_lower) {
                missing.push(expected.to_string());
                continue;
            }
            if !packaged_inf_base_names.contains(&expected_lower) {
                excluded.push(expected.to_string());
            }
        }

        if !missing.is_empty() || !excluded.is_empty() {
            let mut msg = format!(
                "driver {} ({}) INF file validation failed ({}):",
                driver.name,
                arch,
                driver_dir.display()
            );
            if !missing.is_empty() {
                msg.push_str(&format!(
                    "\n- missing expected INF files: {}",
                    missing.join(", ")
                ));
            }
            if !excluded.is_empty() {
                msg.push_str(&format!(
                    "\n- expected INF files are present but would be excluded from packaged output: {}",
                    excluded.join(", ")
                ));
            }
            msg.push_str(&format!(
                "\nexpected_inf_files: {}",
                driver.expected_inf_files.join(", ")
            ));
            msg.push_str(&format!(
                "\nscanned packaged INF files: {}",
                scanned_packaged_inf_paths()
            ));
            if all_inf_paths.is_empty() {
                msg.push_str("\nscanned INF files under driver directory: <none>");
            } else {
                msg.push_str(&format!(
                    "\nscanned INF files under driver directory: {}",
                    all_inf_paths.join(", ")
                ));
            }
            bail!("{msg}");
        }
    }

    // Similar to `expected_hardware_ids`: ignore comment-only matches so shipping a commented-out
    // AddService line does not satisfy the packaging validation.
    fn inf_text_matches_expected_add_service(text: &str, expected_service_name: &str) -> bool {
        for raw_line in text.lines() {
            let line = raw_line
                .split_once(';')
                .map(|(before, _comment)| before)
                .unwrap_or(raw_line)
                .trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("addservice") {
                continue;
            }

            let first = value.split(',').next().unwrap_or("");
            let svc = first.trim().trim_matches(|c| c == '"' || c == '\'');
            if svc.eq_ignore_ascii_case(expected_service_name) {
                return true;
            }
        }
        false
    }

    let mut expected_add_services = Vec::<String>::new();
    for raw in &driver.expected_add_services {
        let svc = raw.trim();
        if svc.is_empty() {
            bail!(
                "driver {} ({}) expected_add_services contains an empty entry",
                driver.name,
                arch
            );
        }
        if !expected_add_services.iter().any(|s| s.eq_ignore_ascii_case(svc)) {
            expected_add_services.push(svc.to_string());
        }
    }
    let mut expected_add_services_var_value = None::<(String, String)>;
    if let Some(var) = &driver.expected_add_services_from_devices_cmd_var {
        let key = var.to_ascii_uppercase();
        let raw = devices_cmd_vars.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "driver {} ({}) references missing devices.cmd variable: {}",
                driver.name,
                arch,
                var
            )
        })?;
        let value = raw.trim();
        if value.is_empty() {
            bail!(
                "driver {} ({}) devices.cmd variable {} is empty",
                driver.name,
                arch,
                var
            );
        }
        expected_add_services_var_value = Some((var.clone(), value.to_string()));
        if !expected_add_services
            .iter()
            .any(|s| s.eq_ignore_ascii_case(value))
        {
            expected_add_services.push(value.to_string());
        }
    }

    if !expected_add_services.is_empty() {
        let mut missing = Vec::<String>::new();
        for svc in &expected_add_services {
            if !infs
                .iter()
                .any(|(_path, text)| inf_text_matches_expected_add_service(text, svc))
            {
                missing.push(svc.clone());
            }
        }

        if !missing.is_empty() {
            let mut msg = format!(
                "driver {} ({}) INF files missing expected AddService directive(s): {}",
                driver.name,
                arch,
                missing.join(", ")
            );
            msg.push_str(&format!(
                "\nexpected_add_services: {}",
                expected_add_services.join(", ")
            ));
            if let Some((var, value)) = expected_add_services_var_value {
                msg.push_str(&format!(
                    "\nexpected_add_services_from_devices_cmd_var: {var}={value}",
                ));
            }
            msg.push_str(&format!(
                "\nscanned packaged INF files: {}",
                scanned_packaged_inf_paths()
            ));
            msg.push_str(&format!("\ndriver directory: {}", driver_dir.display()));
            bail!("{msg}");
        }
    }

    // INF files often contain commented-out HWID lines from previous iterations or debugging.
    // When validating expected HWID patterns, ignore comment-only matches (anything after `;`)
    // so packaging fails if the HWID only appears in comments.
    fn inf_text_matches_expected_hwid(text: &str, re: &regex::Regex) -> bool {
        for raw_line in text.lines() {
            let line = raw_line
                .split_once(';')
                .map(|(before, _comment)| before)
                .unwrap_or(raw_line)
                .trim();
            if line.is_empty() {
                continue;
            }
            if re.is_match(line) {
                return true;
            }
        }
        false
    }

    let mut expected_hardware_ids = driver.expected_hardware_ids.clone();
    if let Some(var) = &driver.expected_hardware_ids_from_devices_cmd_var {
        let key = var.to_ascii_uppercase();
        let raw = devices_cmd_vars.get(&key).ok_or_else(|| {
            anyhow::anyhow!(
                "driver {} references missing devices.cmd variable: {}",
                driver.name,
                var
            )
        })?;
        let hwids = parse_devices_cmd_token_list(raw);
        if hwids.is_empty() {
            bail!(
                "driver {} devices.cmd variable {} is empty",
                driver.name,
                var
            );
        }
        // devices.cmd tends to list the full set of enumerated HWIDs (including SUBSYS/REV
        // qualifiers) because setup.cmd uses them for CriticalDeviceDatabase seeding.
        //
        // However, INFs typically only match the base vendor/device pair (Windows will also
        // enumerate a less-specific `PCI\VEN_....&DEV_....` HWID even when SUBSYS/REV are
        // present). To keep the packager's INF validation aligned with real-world INF matching,
        // normalize each devices.cmd HWID token down to the base `PCI\VEN_....&DEV_....` form
        // before requiring it to appear in the INF.
        let base_re = regex::RegexBuilder::new(r"(?i)PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4}")
            .build()
            .expect("valid PCI HWID regex");
        for hwid in hwids {
            let base = base_re
                .find(&hwid)
                .map(|m| m.as_str())
                .unwrap_or(hwid.as_str());
            let pat = regex::escape(base);
            if !expected_hardware_ids.contains(&pat) {
                expected_hardware_ids.push(pat);
            }
        }
    }

    for hwid_re in &expected_hardware_ids {
        let re = regex::RegexBuilder::new(hwid_re)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("compile regex for hardware ID: {hwid_re}"))?;
        if !infs
            .iter()
            .any(|(_path, text)| inf_text_matches_expected_hwid(text, &re))
        {
            bail!(
                "driver {} ({}) INF files missing expected hardware ID pattern: {hwid_re}",
                driver.name,
                arch
            );
        }
    }

    // Best-effort: ensure all commonly referenced payload items are actually present in the
    // packaged driver directory. This catches missing coinstallers and other auxiliary payloads
    // that `pnputil -a <inf>` expects to find next to the INF.
    let mut missing: BTreeMap<String, (String, BTreeSet<String>)> = BTreeMap::new();
    let has_any_wdf_coinstaller = packaged_base_names
        .iter()
        .any(|n| n.starts_with("wdfcoinstaller") && n.ends_with(".dll"));

    for (inf_rel_path, inf_text) in &infs {
        let (referenced, needs_any_wdf) = collect_inf_references(inf_text);
        for token in referenced {
            let token = normalize_inf_path_token(&token);
            if token.is_empty() || token.contains('%') {
                continue;
            }
            let token_lower = token.to_ascii_lowercase().replace('\\', "/");
            if token_lower.contains('/') && packaged_rel_paths.contains(&token_lower) {
                continue;
            }

            // Prefer path matches when a path was provided, but fall back to basename (best-effort)
            // to avoid false negatives when the INF uses slightly different relative paths.
            let base = token_lower
                .rsplit_once('/')
                .map(|(_, b)| b)
                .unwrap_or(token_lower.as_str());
            if packaged_base_names.contains(base) {
                continue;
            }

            missing
                .entry(token_lower)
                .and_modify(|(_display, infs)| {
                    infs.insert(inf_rel_path.clone());
                })
                .or_insert_with(|| {
                    let mut infs = BTreeSet::new();
                    infs.insert(inf_rel_path.clone());
                    (token, infs)
                });
        }

        if needs_any_wdf && !has_any_wdf_coinstaller {
            missing
                .entry("wdfcoinstaller*.dll".to_string())
                .and_modify(|(_display, infs)| {
                    infs.insert(inf_rel_path.clone());
                })
                .or_insert_with(|| {
                    let mut infs = BTreeSet::new();
                    infs.insert(inf_rel_path.clone());
                    ("WdfCoInstaller*.dll".to_string(), infs)
                });
        }
    }

    if !missing.is_empty() {
        let mut msg = format!(
            "driver {} ({}) INF referenced files are missing from the packaged driver directory ({}):",
            driver.name,
            arch,
            driver_dir.display()
        );
        for (display, infs) in missing.values() {
            let inf_list = infs.iter().cloned().collect::<Vec<_>>().join(", ");
            msg.push_str(&format!("\n- {display} (referenced by: {inf_list})"));
        }
        if missing.values().any(|(display, _)| {
            display.to_ascii_lowercase().starts_with("wdfcoinstaller")
                && display.to_ascii_lowercase().ends_with(".dll")
        }) {
            msg.push_str(
                "\n\nKMDF drivers often require the KMDF coinstaller DLL (WdfCoInstaller*.dll) to be present alongside the INF.",
            );
        }
        bail!("{msg}");
    }

    Ok(())
}

#[derive(Debug, Default)]
struct DriverFileAllowlist {
    allowed_exts: HashSet<String>,
    allowed_path_res: Vec<regex::Regex>,
}

impl DriverFileAllowlist {
    fn from_driver_spec(driver: &DriverSpec) -> Result<Self> {
        let mut out = DriverFileAllowlist::default();

        for ext in &driver.allow_extensions {
            let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
            if ext.is_empty() {
                continue;
            }
            out.allowed_exts.insert(ext);
        }

        for pat in &driver.allow_path_regexes {
            let re = regex::RegexBuilder::new(pat)
                .case_insensitive(true)
                .build()
                .with_context(|| {
                    format!(
                        "compile allow_path_regexes pattern for driver {}: {}",
                        driver.name, pat
                    )
                })?;
            out.allowed_path_res.push(re);
        }

        Ok(out)
    }

    fn is_allowed(&self, rel_path: &str, ext: Option<&str>) -> bool {
        if let Some(ext) = ext {
            if self.allowed_exts.contains(ext) {
                return true;
            }
        }
        self.allowed_path_res.iter().any(|re| re.is_match(rel_path))
    }
}

fn should_include_driver_file(
    path: &Path,
    rel_path: &str,
    allowlist: &DriverFileAllowlist,
) -> Result<bool> {
    // Refuse to ship any file that looks like signing key material, even if it would otherwise
    // be excluded (e.g. hidden files/dirs). Drivers are often distributed as directory trees and
    // we want to fail fast if a key accidentally ends up in the payload.
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ext_opt = if ext.is_empty() {
        None
    } else {
        Some(ext.as_str())
    };
    if let Some(ext) = ext_opt {
        if is_private_key_extension(ext) {
            bail!(
                "refusing to package private key material (.{ext}): {} (Guest Tools must not ship signing keys or other secret material)",
                path.display(),
            );
        }
    }

    // Skip hidden directories (e.g. `.vs/`) to keep outputs stable across hosts.
    // `walkdir` will still traverse them unless we filter at the file level.
    if rel_path
        .split('/')
        .any(|c| c.starts_with('.') || c == "__MACOSX")
    {
        return Ok(false);
    }

    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    // Keep outputs stable across hosts (e.g. ignore `.DS_Store`).
    if file_name.starts_with('.') {
        return Ok(false);
    }
    // Also ignore common Windows shell metadata files to keep outputs stable for
    // local builds on Windows.
    let file_name_lower = file_name.to_ascii_lowercase();
    if matches!(
        file_name_lower.as_str(),
        "thumbs.db" | "ehthumbs.db" | "desktop.ini"
    ) {
        return Ok(false);
    }

    if let Some(ext) = ext_opt {
        if is_default_excluded_driver_extension(ext) && !allowlist.is_allowed(rel_path, Some(ext)) {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Returns false for files that should never be packaged into Guest Tools, regardless of the
/// source directory.
///
/// The primary goal is to keep ISO/zip outputs stable across hosts, even when input trees were
/// previously extracted on macOS/Windows and contain OS metadata artifacts.
fn should_include_guest_tools_tree_file(path: &Path, rel_path: &str) -> bool {
    // Skip hidden directories (e.g. `.vs/`) and macOS archive extraction artifacts to keep outputs
    // stable across hosts. `walkdir` will still traverse them unless we filter at the file level.
    if rel_path
        .split('/')
        .any(|c| c.starts_with('.') || c == "__MACOSX")
    {
        return false;
    }

    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    // Skip hidden files such as `.DS_Store` to keep outputs stable across hosts.
    if file_name.starts_with('.') {
        return false;
    }
    // Also ignore common Windows shell metadata files to keep outputs stable for
    // local builds on Windows.
    let file_name_lower = file_name.to_ascii_lowercase();
    if matches!(
        file_name_lower.as_str(),
        "thumbs.db" | "ehthumbs.db" | "desktop.ini"
    ) {
        return false;
    }

    true
}

fn is_private_key_extension(ext: &str) -> bool {
    matches!(
        ext,
        // Common Windows signing key container formats.
        "pfx" | "p12" | "pvk" | "snk"
        // Common PEM/DER private key encodings.
        | "key" | "pem" | "der"
        // PKCS#8 private key encodings.
        | "p8" | "pk8"
        // Certificate signing requests may include key-related material and should never ship.
        | "csr"
    )
}

fn is_default_excluded_driver_extension(ext: &str) -> bool {
    matches!(
        ext,
        // Debug symbols.
        "pdb" | "ipdb" | "iobj" | "dbg" | "map" | "cod"
        // Build metadata.
        | "obj" | "lib" | "exp" | "ilk" | "tlog" | "log" | "tmp" | "lastbuildstate" | "idb"
        // Source / project files.
        | "c" | "cc" | "cpp" | "cxx" | "h" | "hh" | "hpp" | "hxx" | "idl" | "inl" | "rc" | "s" | "asm"
        | "sln" | "vcxproj" | "props" | "targets"
    )
}

/// Returns a best-effort set of files referenced by common INF directives, plus a flag indicating
/// that the INF references KMDF coinstallers but does not name a specific `WdfCoInstaller*.dll`.
fn collect_inf_references(inf_text: &str) -> (BTreeSet<String>, bool) {
    let sections = parse_inf_sections(inf_text);
    let mut referenced = BTreeSet::<String>::new();
    let mut needs_any_wdf = false;

    // [Version] CatalogFile directives define which catalog (*.cat) file is used for the driver
    // package. Ensure these references exist in the packaged directory so we fail early if the
    // driver directory contains a mismatched or missing catalog.
    if let Some(lines) = sections.get("version") {
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if !key.trim().to_ascii_lowercase().starts_with("catalogfile") {
                continue;
            }
            let token = value.split(',').next().unwrap_or("");
            let token = normalize_inf_path_token(token);
            if token.to_ascii_lowercase().ends_with(".cat") {
                referenced.insert(token);
            }
        }
    }

    // Validate `SourceDisksFiles*` entries if present; these are intended to list every
    // payload file that ships in the driver package.
    for (name, lines) in &sections {
        if !name.starts_with("sourcedisksfiles") {
            continue;
        }
        for line in lines {
            let Some((lhs, _rhs)) = line.split_once('=') else {
                continue;
            };
            let token = normalize_inf_path_token(lhs);
            if !token.is_empty() {
                referenced.insert(token);
            }
        }
    }

    // Best-effort: follow CopyFiles directives into any file list sections they reference.
    let mut copyfile_sections = BTreeSet::<String>::new();
    for lines in sections.values() {
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim().eq_ignore_ascii_case("copyfiles") {
                for token in value.split(',') {
                    let token = token.trim();
                    if token.is_empty() {
                        continue;
                    }
                    if let Some(file) = token.strip_prefix('@') {
                        let file = normalize_inf_path_token(file);
                        if file.is_empty() || file.contains('%') {
                            continue;
                        }
                        referenced.insert(file);
                        continue;
                    }

                    let token = normalize_inf_path_token(token);
                    if token.is_empty() || token.contains('%') {
                        continue;
                    }

                    // Most values are file-list section names (which commonly contain `.NT...`
                    // suffixes). Treat it as a section if it exists; otherwise, fall back to
                    // treating it as a direct file reference.
                    let section_key = token.to_ascii_lowercase();
                    if sections.contains_key(&section_key) {
                        copyfile_sections.insert(section_key);
                    } else {
                        referenced.insert(token);
                    }
                }
            } else if key.trim().eq_ignore_ascii_case("copyinf") {
                // Best-effort: `CopyINF` can be used to pull additional INF files into the driver
                // package; `pnputil -a` expects these to exist relative to the staging directory.
                for token in value.split(',') {
                    let token = normalize_inf_path_token(token)
                        .trim_start_matches('@')
                        .to_string();
                    if token.is_empty() || token.contains('%') {
                        continue;
                    }
                    if token.to_ascii_lowercase().ends_with(".inf") {
                        referenced.insert(token);
                    }
                }
            }
        }
    }

    for section_name in copyfile_sections {
        let Some(lines) = sections.get(&section_name) else {
            continue;
        };
        for line in lines {
            // File list entries are `file[,subdir][,flags][,rename]...`.
            let first = line.split(',').next().unwrap_or("").trim();
            let token = normalize_inf_path_token(first)
                .trim_start_matches('@')
                .to_string();
            if token.contains('.') || token.contains('/') {
                referenced.insert(token);
            }
        }
    }

    // Best-effort: validate `ServiceBinary` references.
    //
    // Typically, the `ServiceBinary` directive lives inside a service install section referenced
    // by an `AddService` directive. We follow that link (best-effort) and ensure any referenced
    // `*.sys` payload exists next to the INF.
    let mut service_install_sections = BTreeSet::<String>::new();
    for lines in sections.values() {
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("addservice") {
                continue;
            }
            let mut parts = value.split(',').map(|s| s.trim());
            // 1: service name (ignored)
            let _svc_name = parts.next();
            // 2: flags (ignored)
            let _flags = parts.next();
            // 3: service install section name.
            let section = parts.next().unwrap_or("").trim();
            if section.is_empty() {
                continue;
            }
            let section = section
                .trim_matches(|c| c == '"' || c == '\'')
                .to_ascii_lowercase();
            if section.is_empty() {
                continue;
            }
            service_install_sections.insert(section);
        }
    }

    for section in service_install_sections {
        let Some(lines) = sections.get(&section) else {
            continue;
        };
        for line in lines {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if !key.trim().eq_ignore_ascii_case("servicebinary") {
                continue;
            }
            let token = value.split(',').next().unwrap_or("");
            let token = normalize_inf_path_token(token);
            let base = token
                .rsplit_once('/')
                .map(|(_, b)| b)
                .unwrap_or(token.as_str())
                .trim();
            if base.to_ascii_lowercase().ends_with(".sys") {
                referenced.insert(base.to_string());
            }
        }
    }

    // Minimum coinstaller sanity: if the INF mentions WdfCoInstaller, ensure the referenced
    // DLL(s) actually exist in the packaged driver directory.
    // Strip comments first to avoid false positives from documentation/comments inside INFs.
    let mut inf_no_comments = String::new();
    for raw_line in inf_text.lines() {
        let before = raw_line.split_once(';').map(|(b, _)| b).unwrap_or(raw_line);
        inf_no_comments.push_str(before);
        inf_no_comments.push('\n');
    }

    // Best-effort: scan common single-line directives that reference files but are often omitted
    // from `SourceDisksFiles` (or do not otherwise appear in `CopyFiles` lists).
    //
    // - `CatalogFile[.<suffix>] = foo.cat` - validates the expected catalog file is present.
    // - `ServiceBinary = %12%\foo.sys` - validates the driver binary referenced by the service.
    let catalog_file_re =
        regex::RegexBuilder::new(r"^\s*CatalogFile(?:\.[^=]+)?\s*=\s*([^\s;]+)")
            .case_insensitive(true)
            .build()
            .expect("valid regex");
    let service_binary_re = regex::RegexBuilder::new(r"^\s*ServiceBinary\s*=\s*([^\s;]+)")
        .case_insensitive(true)
        .build()
        .expect("valid regex");

    for line in inf_no_comments.lines() {
        if let Some(caps) = catalog_file_re.captures(line) {
            let value = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let token = normalize_inf_path_token(value);
            if token.is_empty() {
                continue;
            }
            let base = token
                .rsplit_once('/')
                .map(|(_, b)| b)
                .unwrap_or(token.as_str());
            // Ignore unresolved string substitutions unless the basename is explicit.
            if base.is_empty() || base.contains('%') {
                continue;
            }
            referenced.insert(base.to_string());
        }

        if let Some(caps) = service_binary_re.captures(line) {
            let value = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let token = normalize_inf_path_token(value);
            if token.is_empty() {
                continue;
            }
            let base = token
                .rsplit_once('/')
                .map(|(_, b)| b)
                .unwrap_or(token.as_str());
            // Ignore unresolved string substitutions unless the basename is explicit.
            if base.is_empty() || base.contains('%') {
                continue;
            }
            referenced.insert(base.to_string());
        }
    }

    let inf_lower = inf_no_comments.to_ascii_lowercase();
    if inf_lower.contains("wdfcoinstaller") {
        let re = regex::RegexBuilder::new(r"wdfcoinstaller[0-9a-z_]*\.dll")
            .case_insensitive(true)
            .build()
            .expect("valid regex");
        let mut found = false;
        for m in re.find_iter(&inf_no_comments) {
            found = true;
            referenced.insert(m.as_str().to_string());
        }

        if !found {
            needs_any_wdf = true;
        }
    }

    (referenced, needs_any_wdf)
}

fn parse_inf_sections(text: &str) -> HashMap<String, Vec<String>> {
    let mut sections: HashMap<String, Vec<String>> = HashMap::new();
    let mut current = None::<String>;
    for raw_line in text.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((before, _comment)) = line.split_once(';') {
            line = before.trim();
        }
        if line.is_empty() {
            continue;
        }

        if let Some(section_name) = line
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .map(|s| s.trim())
        {
            if section_name.is_empty() {
                current = None;
            } else {
                let name = section_name.to_ascii_lowercase();
                sections.entry(name.clone()).or_default();
                current = Some(name);
            }
            continue;
        }

        let Some(section) = current.as_ref() else {
            continue;
        };
        sections
            .entry(section.clone())
            .or_default()
            .push(line.to_string());
    }
    sections
}

fn normalize_inf_path_token(token: &str) -> String {
    let mut s = token
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    while s.starts_with(".\\") || s.starts_with("./") {
        s = s[2..].to_string();
    }
    s = s.replace('\\', "/");
    s.trim().to_string()
}

fn parse_devices_cmd_vars(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        let lower = l.to_ascii_lowercase();
        if lower == "rem"
            || lower.starts_with("rem ")
            || lower.starts_with("::")
            || lower.starts_with("@echo")
        {
            continue;
        }

        // Support the common forms:
        //   set VAR=value
        //   set "VAR=value"
        if !(lower.starts_with("set ") || lower.starts_with("set\t") || lower == "set") {
            continue;
        }

        let rest = l.get(3..).unwrap_or("").trim_start();
        if rest.is_empty() {
            continue;
        }

        let (name, value) = if rest.starts_with('"') {
            let inner = rest
                .strip_prefix('"')
                .and_then(|s| s.split_once('"'))
                .map(|(inner, _after)| inner)
                .unwrap_or("");
            if let Some((k, v)) = inner.split_once('=') {
                (k.trim().to_string(), v.to_string())
            } else {
                continue;
            }
        } else if let Some((k, v)) = rest.split_once('=') {
            (k.trim().to_string(), v.trim().to_string())
        } else {
            continue;
        };

        if name.is_empty() {
            continue;
        }
        vars.insert(name.to_ascii_uppercase(), value);
    }

    vars
}

fn read_devices_cmd_vars(path: &Path) -> Result<HashMap<String, String>> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(parse_devices_cmd_vars(text.as_ref()))
}

fn parse_devices_cmd_token_list(raw: &str) -> Vec<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        if bytes[i] == b'"' {
            i += 1;
            let start = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let token = raw.get(start..i).unwrap_or("").to_string();
            if !token.is_empty() {
                out.push(token);
            }
            if i < bytes.len() && bytes[i] == b'"' {
                i += 1;
            }
        } else {
            let start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let token = raw.get(start..i).unwrap_or("").to_string();
            if !token.is_empty() {
                out.push(token);
            }
        }
    }

    out
}

fn resolve_input_arch_dir(drivers_dir: &Path, arch_out: &str) -> Result<PathBuf> {
    let candidates: &[&str] = match arch_out {
        // ISO layout + setup.cmd uses `x86` and `amd64`. For convenience, accept
        // common build-output directory names too (especially `x64`).
        "x86" => &["x86", "win32", "i386"],
        "amd64" => &["amd64", "x64", "x86_64", "x86-64"],
        other => bail!("unsupported arch: {other}"),
    };

    for name in candidates {
        let p = drivers_dir.join(name);
        if p.is_dir() {
            return Ok(p);
        }
    }

    let tried = candidates
        .iter()
        .map(|n| drivers_dir.join(n).display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("drivers dir missing required architecture directory for {arch_out}; tried: {tried}");
}

fn read_inf_text(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    // INF files are often ASCII/UTF-8, but can be UTF-16LE with BOM. We only
    // need a best-effort string for regex matching expected HWIDs.
    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        // UTF-16LE with BOM.
        decode_utf16(&bytes[2..], true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16BE with BOM.
        decode_utf16(&bytes[2..], false)
    } else if bytes.len() >= 4 && bytes.len() % 2 == 0 {
        // Some Windows tooling produces UTF-16 INFs without a BOM. Detect UTF-16LE vs UTF-16BE
        // by looking for a high ratio of NUL bytes in either the odd (LE) or even (BE) byte
        // positions.
        //
        // Use a small set of prefix windows to avoid missing UTF-16 when the file contains large
        // non-ASCII string tables (which reduce the overall NUL-byte ratio).
        const NUL_RATIO_THRESHOLD: f64 = 0.30;
        const NUL_RATIO_SKEW: f64 = 0.20;

        let mut le_votes = 0usize;
        let mut be_votes = 0usize;
        for prefix_len in [128usize, 512, 2048] {
            let mut len = bytes.len().min(prefix_len);
            len -= len % 2;
            if len < 4 {
                continue;
            }

            let mut nul_even = 0usize;
            let mut nul_odd = 0usize;
            for (i, b) in bytes[..len].iter().enumerate() {
                if *b != 0 {
                    continue;
                }
                if i % 2 == 0 {
                    nul_even += 1;
                } else {
                    nul_odd += 1;
                }
            }

            let half = len / 2;
            let ratio_even = nul_even as f64 / half as f64;
            let ratio_odd = nul_odd as f64 / half as f64;

            if ratio_odd >= NUL_RATIO_THRESHOLD && ratio_odd - ratio_even >= NUL_RATIO_SKEW {
                le_votes += 1;
            } else if ratio_even >= NUL_RATIO_THRESHOLD && ratio_even - ratio_odd >= NUL_RATIO_SKEW
            {
                be_votes += 1;
            }
        }

        if le_votes == 0 && be_votes == 0 {
            String::from_utf8_lossy(&bytes).to_string()
        } else if le_votes > be_votes {
            decode_utf16(&bytes, true)
        } else if be_votes > le_votes {
            decode_utf16(&bytes, false)
        } else {
            // Ambiguous: decode both and pick the more text-like one.
            let le = decode_utf16(&bytes, true);
            let be = decode_utf16(&bytes, false);

            fn decode_score(s: &str) -> (usize, usize, usize, usize) {
                let mut replacement = 0usize;
                let mut nul = 0usize;
                let mut ascii = 0usize;
                let mut newlines = 0usize;
                let mut total = 0usize;
                for c in s.chars() {
                    total += 1;
                    if c == '\u{FFFD}' {
                        replacement += 1;
                    } else if c == '\u{0000}' {
                        nul += 1;
                    }
                    if c.is_ascii() {
                        ascii += 1;
                        if c == '\n' {
                            newlines += 1;
                        }
                    }
                }
                // Lower is better: prefer fewer replacement/NULs; then prefer decodes that yield
                // more ASCII/newlines (which strongly correlates with correct endianness for INFs).
                let ascii_penalty = total.saturating_sub(ascii);
                let newline_penalty = total.saturating_sub(newlines);
                (replacement, nul, ascii_penalty, newline_penalty)
            }

            let le_score = decode_score(&le);
            let be_score = decode_score(&be);
            if le_score <= be_score {
                // Prefer little-endian when ambiguous (Windows commonly uses UTF-16LE).
                le
            } else {
                be
            }
        }
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
    // Strip UTF-8 BOM if present.
    let stripped = text.trim_start_matches('\u{feff}');
    if stripped.len() != text.len() {
        return Ok(stripped.to_string());
    }
    Ok(text)
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let u = if little_endian {
            u16::from_le_bytes([chunk[0], chunk[1]])
        } else {
            u16::from_be_bytes([chunk[0], chunk[1]])
        };
        units.push(u);
    }
    String::from_utf16_lossy(&units)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn validate_windows_safe_rel_path(rel_path: &str) -> Result<()> {
    for component in rel_path.split('/') {
        if component.is_empty() {
            bail!("package path {rel_path:?} contains an empty component");
        }
        if component == "." || component == ".." {
            bail!(
                "package path {rel_path:?} contains an invalid component: {component:?}"
            );
        }
        if component.ends_with('.') || component.ends_with(' ') {
            bail!(
                "package path {rel_path:?} contains a Windows-invalid component (trailing '.' or space): {component:?}"
            );
        }
        if let Some(c) = component.chars().find(|c| {
            matches!(
                c,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        }) {
            bail!(
                "package path {rel_path:?} contains a Windows-invalid component (invalid character {c:?}): {component:?}"
            );
        }
        let base = component.split('.').next().unwrap_or("");
        if is_reserved_windows_device_name(base) {
            bail!(
                "package path {rel_path:?} contains a Windows-invalid reserved device name: {component:?}"
            );
        }
    }
    Ok(())
}

fn is_reserved_windows_device_name(base_name: &str) -> bool {
    let upper = base_name.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        _ => {
            if let Some(n) = upper.strip_prefix("COM") {
                return matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9");
            }
            if let Some(n) = upper.strip_prefix("LPT") {
                return matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9");
            }
            false
        }
    }
}

fn canonicalize_json_bytes(bytes: &[u8]) -> Result<Vec<u8>> {
    // Canonicalize JSON inputs before hashing so provenance hashes are stable across harmless
    // formatting differences (whitespace, indentation, key ordering) introduced by tooling (for
    // example different PowerShell/`ConvertTo-Json` versions).
    //
    // Note: `serde_json::Value` uses a deterministically-ordered map type by default (BTreeMap)
    // unless the `preserve_order` feature is enabled.
    let value = serde_json::from_slice::<serde_json::Value>(bytes)
        .context("parse JSON for canonicalization")?;
    serde_json::to_vec(&value).context("serialize canonical JSON")
}

fn manifest_input_path(path: &Path) -> Result<String> {
    let name = path.file_name().unwrap_or_else(|| path.as_os_str());
    let s = name
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("manifest input path is not valid UTF-8: {:?}", path))?;
    Ok(s.to_string())
}

fn path_to_slash(path: &Path, full_path: &Path) -> Result<String> {
    // Packaged ISO/zip paths are UTF-8 strings. On Unix hosts, filenames are raw bytes and may not
    // be valid UTF-8; refusing non-UTF8 paths avoids silently mangling/dropping path components and
    // risking collisions inside the packaged output.
    let mut components = Vec::new();
    for c in path.components() {
        let s = c
            .as_os_str()
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 path component: {:?}", full_path))?;
        components.push(s);
    }
    Ok(components.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn collect_dirs_for_zip(files: &[FileToPackage]) -> BTreeSet<String> {
        let mut dirs = BTreeSet::new();
        for f in files {
            let parts: Vec<&str> = f.rel_path.split('/').collect();
            if parts.len() <= 1 {
                continue;
            }
            for i in 1..parts.len() {
                let mut s = parts[..i].join("/");
                s.push('/');
                dirs.insert(s);
            }
        }
        dirs
    }

    #[test]
    fn zip_dir_collection_is_sorted_and_unique() {
        let files = vec![
            FileToPackage {
                rel_path: "drivers/x86/a/b.sys".into(),
                bytes: vec![],
            },
            FileToPackage {
                rel_path: "setup.cmd".into(),
                bytes: vec![],
            },
        ];

        let dirs: Vec<_> = collect_dirs_for_zip(&files).into_iter().collect();
        assert_eq!(dirs, vec!["drivers/", "drivers/x86/", "drivers/x86/a/"]);
    }

    fn write_test_driver(dir: &Path) {
        fs::create_dir_all(dir).expect("create driver dir");
        fs::write(
            dir.join("test.inf"),
            b"[Version]\nSignature=\"$Windows NT$\"\n",
        )
        .expect("write INF");
        fs::write(dir.join("test.sys"), b"").expect("write SYS");
        fs::write(dir.join("test.cat"), b"").expect("write CAT");
    }

    fn make_test_spec(fail_on_unlisted_driver_dirs: bool) -> PackagingSpec {
        PackagingSpec {
            require_optional_drivers_on_all_arches: false,
            drivers: vec![DriverSpec {
                name: "testdrv".to_string(),
                required: true,
                expected_inf_files: Vec::new(),
                expected_add_services: Vec::new(),
                expected_add_services_from_devices_cmd_var: None,
                expected_hardware_ids: Vec::new(),
                expected_hardware_ids_from_devices_cmd_var: None,
                allow_extensions: Vec::new(),
                allow_path_regexes: Vec::new(),
            }],
            fail_on_unlisted_driver_dirs,
        }
    }

    #[test]
    fn packaging_ignores_unlisted_driver_dirs_when_disabled() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let drivers_dir = tmp.path();

        write_test_driver(&drivers_dir.join("x86").join("testdrv"));
        write_test_driver(&drivers_dir.join("amd64").join("testdrv"));
        fs::create_dir_all(drivers_dir.join("x86").join("extra_driver"))
            .expect("create extra x86 driver dir");
        fs::create_dir_all(drivers_dir.join("amd64").join("extra_driver"))
            .expect("create extra amd64 driver dir");

        let spec = make_test_spec(false);
        let plan = validate_drivers(&spec, drivers_dir, &HashMap::new())
            .expect("validate_drivers succeeds");
        assert_eq!(plan.x86.len(), 1);
        assert_eq!(plan.amd64.len(), 1);
    }

    #[test]
    fn packaging_fails_on_unlisted_driver_dirs_when_enabled() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let drivers_dir = tmp.path();

        write_test_driver(&drivers_dir.join("x86").join("testdrv"));
        write_test_driver(&drivers_dir.join("amd64").join("testdrv"));
        fs::create_dir_all(drivers_dir.join("x86").join("extra_driver"))
            .expect("create extra x86 driver dir");
        fs::create_dir_all(drivers_dir.join("amd64").join("extra_driver"))
            .expect("create extra amd64 driver dir");

        let spec = make_test_spec(true);
        let err = validate_drivers(&spec, drivers_dir, &HashMap::new()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("extra_driver"), "{err_str}");
        assert!(
            err_str.contains("fail_on_unlisted_driver_dirs"),
            "{err_str}"
        );
    }
}
