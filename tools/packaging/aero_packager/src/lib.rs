mod iso9660;
mod manifest;
mod spec;
mod zip_util;

use anyhow::{bail, Context, Result};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub use iso9660::read_joliet_file_entries;
pub use iso9660::read_joliet_tree;
pub use iso9660::{IsoFileEntry, IsoFileTree};
pub use manifest::{Manifest, ManifestFileEntry, SigningPolicy};
pub use spec::{DriverSpec, PackagingSpec};

/// Configuration for producing the distributable "Aero Drivers / Guest Tools" media.
#[derive(Debug, Clone)]
pub struct PackageConfig {
    pub drivers_dir: PathBuf,
    pub guest_tools_dir: PathBuf,
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

/// Create `aero-guest-tools.iso`, `aero-guest-tools.zip`, and `manifest.json` in `out_dir`.
pub fn package_guest_tools(config: &PackageConfig) -> Result<PackageOutputs> {
    fs::create_dir_all(&config.out_dir)
        .with_context(|| format!("create output dir {}", config.out_dir.display()))?;

    let spec = PackagingSpec::load(&config.spec_path).with_context(|| "load packaging spec")?;

    let driver_plan = validate_drivers(&spec, &config.drivers_dir)
        .with_context(|| "validate driver artifacts")?;

    let mut files = collect_files(config, &driver_plan)?;
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

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

    let manifest_bytes = serde_json::to_vec_pretty(&manifest).context("serialize manifest.json")?;
    let manifest_file = FileToPackage {
        rel_path: "manifest.json".to_string(),
        bytes: manifest_bytes.clone(),
    };
    files.push(manifest_file);
    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

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

fn collect_files(config: &PackageConfig, driver_plan: &DriverPlan) -> Result<Vec<FileToPackage>> {
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
                src.to_string_lossy()
            );
        }
        out.push(FileToPackage {
            rel_path: file_name.to_string(),
            bytes: fs::read(&src).with_context(|| format!("read {}", src.display()))?,
        });
    }

    // Guest tools config (expected device IDs / service names).
    // setup.cmd requires this for boot-critical virtio-blk pre-seeding.
    let config_devices = config.guest_tools_dir.join("config").join("devices.cmd");
    if !config_devices.is_file() {
        bail!(
            "guest tools missing required file: {}",
            config_devices.to_string_lossy()
        );
    }
    let config_dir = config.guest_tools_dir.join("config");
    if !config_dir.is_dir() {
        bail!(
            "guest tools missing required directory: {}",
            config_dir.to_string_lossy()
        );
    }
    let devices_cmd = config_dir.join("devices.cmd");
    if !devices_cmd.is_file() {
        bail!(
            "guest tools missing required config file: {}",
            devices_cmd.to_string_lossy()
        );
    }
    for entry in walkdir::WalkDir::new(&config_dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        // Skip hidden files such as `.DS_Store` to keep outputs stable across hosts.
        let file_name = entry.file_name().to_string_lossy();
        if file_name.starts_with('.') {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(&config.guest_tools_dir)
            .expect("walkdir under guest_tools_dir");
        let rel_str = path_to_slash(rel);
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

            // Skip hidden files such as `.DS_Store` (and placeholder `.keep`) to keep outputs
            // stable across hosts.
            let file_name = entry.file_name().to_string_lossy();
            if file_name.starts_with('.') {
                continue;
            }

            let rel = entry
                .path()
                .strip_prefix(&config.guest_tools_dir)
                .expect("walkdir under guest_tools_dir");
            let rel_str = path_to_slash(rel);
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
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(&certs_dir)
                .expect("walkdir under certs_dir");
            let rel_str = path_to_slash(rel);
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
                certs_dir.to_string_lossy(),
            );
        }
        out.extend(certs);
    } else if config.signing_policy.certs_required() {
        bail!(
            "guest tools missing required directory: {}",
            certs_dir.to_string_lossy()
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

    // Drivers.
    for (arch_out, drivers) in [("x86", &driver_plan.x86), ("amd64", &driver_plan.amd64)] {
        for driver in drivers {
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
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&driver.dir)
                    .expect("walkdir under driver dir");
                let rel_str = path_to_slash(rel);
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

fn validate_drivers(spec: &PackagingSpec, drivers_dir: &Path) -> Result<DriverPlan> {
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

    let mut plan = DriverPlan {
        x86: Vec::new(),
        amd64: Vec::new(),
    };

    for drv in &spec.drivers {
        for (arch, arch_dir, out) in [
            ("x86", &drivers_x86_dir, &mut plan.x86),
            ("amd64", &drivers_amd64_dir, &mut plan.amd64),
        ] {
            let driver_dir = arch_dir.join(&drv.name);
            if !driver_dir.is_dir() {
                if drv.required {
                    bail!(
                        "required driver directory missing: {}",
                        driver_dir.to_string_lossy()
                    );
                }
                eprintln!(
                    "warning: optional driver directory missing: {} ({})",
                    drv.name, arch
                );
                continue;
            }

            validate_driver_dir(drv, arch, &driver_dir)?;
            out.push(DriverToInclude {
                spec: drv.clone(),
                dir: driver_dir,
            });
        }
    }

    Ok(plan)
}

fn validate_driver_dir(driver: &DriverSpec, arch: &str, driver_dir: &Path) -> Result<()> {
    let allowlist = DriverFileAllowlist::from_driver_spec(driver)
        .with_context(|| format!("load file allowlist overrides for driver {}", driver.name))?;

    let mut found_inf = false;
    let mut found_sys = false;
    let mut found_cat = false;
    let mut infs = Vec::<(String, String)>::new();

    // Collect a view of the files that would be packaged for this driver, so we can
    // validate that any INF-referenced payloads are actually present.
    let mut packaged_rel_paths = HashSet::<String>::new();
    let mut packaged_base_names = HashSet::<String>::new();

    for entry in walkdir::WalkDir::new(driver_dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let rel = path
            .strip_prefix(driver_dir)
            .expect("walkdir under driver_dir");
        let rel_str = path_to_slash(rel);
        let include = should_include_driver_file(path, &rel_str, &allowlist)
            .with_context(|| format!("filter driver file {}", path.display()))?;

        if include {
            packaged_rel_paths.insert(rel_str.to_ascii_lowercase());
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                packaged_base_names.insert(name.to_ascii_lowercase());
            }

            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".inf") {
                found_inf = true;
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

    for hwid_re in &driver.expected_hardware_ids {
        let re = regex::RegexBuilder::new(hwid_re)
            .case_insensitive(true)
            .build()
            .with_context(|| format!("compile regex for hardware ID: {hwid_re}"))?;
        if !infs.iter().any(|(_path, text)| re.is_match(text)) {
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
        for (_key, (display, infs)) in &missing {
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

fn is_private_key_extension(ext: &str) -> bool {
    matches!(ext, "pfx" | "pvk" | "snk" | "key" | "pem")
}

fn is_default_excluded_driver_extension(ext: &str) -> bool {
    matches!(
        ext,
        // Debug symbols.
        "pdb" | "ipdb" | "iobj"
        // Build metadata.
        | "obj" | "lib" | "exp" | "ilk" | "tlog" | "log"
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
                    let token = normalize_inf_path_token(token);
                    if token.is_empty() {
                        continue;
                    }
                    let token = token.trim_start_matches('@').to_string();
                    if token.contains('.') || token.contains('/') {
                        referenced.insert(token);
                    } else {
                        copyfile_sections.insert(token.to_ascii_lowercase());
                    }
                }
            } else if key.trim().eq_ignore_ascii_case("copyinf") {
                // Best-effort: `CopyINF` can be used to pull additional INF files into the driver
                // package; `pnputil -a` expects these to exist relative to the staging directory.
                for token in value.split(',') {
                    let token = normalize_inf_path_token(token).trim_start_matches('@').to_string();
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

    // Minimum coinstaller sanity: if the INF mentions WdfCoInstaller, ensure the referenced
    // DLL(s) actually exist in the packaged driver directory.
    let inf_lower = inf_text.to_ascii_lowercase();
    if inf_lower.contains("wdfcoinstaller") {
        let re = regex::RegexBuilder::new(r"wdfcoinstaller[0-9a-z_]*\.dll")
            .case_insensitive(true)
            .build()
            .expect("valid regex");
        let mut found = false;
        for m in re.find_iter(inf_text) {
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
                current = Some(section_name.to_ascii_lowercase());
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
        decode_utf16(&bytes[2..], true)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        decode_utf16(&bytes[2..], false)
    } else {
        String::from_utf8_lossy(&bytes).to_string()
    };
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

fn path_to_slash(path: &Path) -> String {
    // We only ever create package paths from in-repo artifacts, so require UTF-8.
    let mut components = Vec::new();
    for c in path.components() {
        let s = c.as_os_str().to_str().unwrap_or("");
        if s.is_empty() {
            continue;
        }
        components.push(s);
    }
    components.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn collect_dirs_for_zip(files: &[FileToPackage]) -> BTreeSet<String> {
        let mut dirs = BTreeSet::new();
        for f in files {
            let mut path = Path::new(&f.rel_path);
            while let Some(parent) = path.parent() {
                if parent.as_os_str().is_empty() {
                    break;
                }
                let mut s = parent.to_string_lossy().replace('\\', "/");
                if !s.ends_with('/') {
                    s.push('/');
                }
                dirs.insert(s);
                path = parent;
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
}
