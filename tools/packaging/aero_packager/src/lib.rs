mod iso9660;
mod manifest;
mod spec;
mod zip_util;

use anyhow::{bail, Context, Result};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub use iso9660::read_joliet_tree;
pub use iso9660::{IsoFileEntry, IsoFileTree};
pub use manifest::{Manifest, ManifestFileEntry};
pub use spec::{PackagingSpec, RequiredDriver};

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

    validate_drivers(&spec, &config.drivers_dir).with_context(|| "validate driver artifacts")?;

    let mut files = collect_files(config, &spec)?;
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

fn collect_files(config: &PackageConfig, _spec: &PackagingSpec) -> Result<Vec<FileToPackage>> {
    let mut out = Vec::new();

    // Guest tools top-level scripts/doc.
    //
    // Keep this list in sync with the published Guest Tools ISO root.
    for file_name in ["setup.cmd", "uninstall.cmd", "verify.cmd", "verify.ps1", "README.md"] {
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
    let config_dir = config.guest_tools_dir.join("config");
    if !config_dir.is_dir() {
        bail!(
            "guest tools missing required directory: {}",
            config_dir.to_string_lossy()
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
        let rel = entry
            .path()
            .strip_prefix(&config_dir)
            .expect("walkdir under config_dir");
        let rel_str = path_to_slash(rel);
        out.push(FileToPackage {
            rel_path: format!("config/{}", rel_str),
            bytes: fs::read(entry.path()).with_context(|| format!("read {}", entry.path().display()))?,
        });
    }

    // Certificates.
    let certs_dir = config.guest_tools_dir.join("certs");
    if !certs_dir.is_dir() {
        bail!(
            "guest tools missing required directory: {}",
            certs_dir.to_string_lossy()
        );
    }
    let mut certs = Vec::new();
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
        if !(lower.ends_with(".cer")
            || lower.ends_with(".crt")
            || lower.ends_with(".p7b")
            || lower == "readme.md")
        {
            continue;
        }
        certs.push(FileToPackage {
            rel_path: format!("certs/{}", rel_str),
            bytes: fs::read(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        });
    }
    if certs.is_empty() {
        bail!(
            "guest tools certs directory contains no .cer/.crt/.p7b files: {}",
            certs_dir.to_string_lossy()
        );
    }
    out.extend(certs);

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
    for (arch_in, arch_out) in [("x86", "x86"), ("amd64", "amd64")] {
        let arch_dir = config.drivers_dir.join(arch_in);
        if !arch_dir.is_dir() {
            bail!(
                "drivers dir missing required architecture directory: {}",
                arch_dir.to_string_lossy()
            );
        }

        // Only include files underneath each driver directory, preserving hierarchy.
        let mut driver_dirs = Vec::new();
        for entry in
            fs::read_dir(&arch_dir).with_context(|| format!("read {}", arch_dir.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                driver_dirs.push(entry.path());
            }
        }
        driver_dirs.sort();

        for driver_dir in driver_dirs {
            let driver_name = driver_dir
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow::anyhow!("non-utf8 driver directory name"))?
                .to_string();

            for entry in walkdir::WalkDir::new(&driver_dir)
                .follow_links(false)
                .sort_by_file_name()
            {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let rel = entry
                    .path()
                    .strip_prefix(&driver_dir)
                    .expect("walkdir under driver_dir");
                let rel_str = path_to_slash(rel);
                let rel_str_lower = rel_str.to_ascii_lowercase();
                if !(rel_str_lower.ends_with(".inf")
                    || rel_str_lower.ends_with(".sys")
                    || rel_str_lower.ends_with(".cat"))
                {
                    continue;
                }

                out.push(FileToPackage {
                    rel_path: format!("drivers/{}/{}/{}", arch_out, driver_name, rel_str),
                    bytes: fs::read(entry.path())
                        .with_context(|| format!("read {}", entry.path().display()))?,
                });
            }
        }
    }

    Ok(out)
}

fn validate_drivers(spec: &PackagingSpec, drivers_dir: &Path) -> Result<()> {
    for required in &spec.required_drivers {
        for arch in ["x86", "amd64"] {
            let driver_dir = drivers_dir.join(arch).join(&required.name);
            if !driver_dir.is_dir() {
                bail!(
                    "required driver directory missing: {}",
                    driver_dir.to_string_lossy()
                );
            }

            let mut found_inf = false;
            let mut found_sys = false;
            let mut found_cat = false;
            let mut inf_texts = Vec::new();

            for entry in walkdir::WalkDir::new(&driver_dir)
                .follow_links(false)
                .sort_by_file_name()
            {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".inf") {
                    found_inf = true;
                    inf_texts.push(
                        read_inf_text(path).with_context(|| {
                            format!("read INF for {} ({})", required.name, arch)
                        })?,
                    );
                } else if lower.ends_with(".sys") {
                    found_sys = true;
                } else if lower.ends_with(".cat") {
                    found_cat = true;
                }
            }

            if !found_inf || !found_sys || !found_cat {
                bail!(
                    "required driver {} ({}) is incomplete: expected at least one .inf, .sys, and .cat",
                    required.name,
                    arch
                );
            }

            for hwid_re in &required.expected_hardware_ids {
                let re = regex::RegexBuilder::new(hwid_re)
                    .case_insensitive(true)
                    .build()
                    .with_context(|| format!("compile regex for hardware ID: {hwid_re}"))?;
                if !inf_texts.iter().any(|t| re.is_match(t)) {
                    bail!(
                        "required driver {} ({}) INF files missing expected hardware ID pattern: {hwid_re}",
                        required.name,
                        arch
                    );
                }
            }
        }
    }

    Ok(())
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
