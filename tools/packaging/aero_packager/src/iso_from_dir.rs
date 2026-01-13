use crate::iso9660;
use crate::FileToPackage;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

/// Write a deterministic ISO9660 + Joliet image from a directory tree.
///
/// This is intended for packaging already-staged directory payloads (e.g. CI driver bundles),
/// not for the full Guest Tools packager which performs additional validation.
///
/// Rules for determinism/stability:
/// - Only regular files are included (no symlinks).
/// - Paths are normalized to `/` separators.
/// - Files are sorted by their package-relative paths.
/// - Common host-generated metadata files are excluded:
///   - Hidden files/dirs (`.*`)
///   - `__MACOSX`
///   - `Thumbs.db` / `ehthumbs.db`
///   - `desktop.ini`
pub fn write_iso9660_joliet_from_dir(
    in_dir: &Path,
    out_iso: &Path,
    volume_id: &str,
    source_date_epoch: i64,
) -> Result<()> {
    if !in_dir.is_dir() {
        bail!("in-dir is not a directory: {}", in_dir.display());
    }

    // Collect files first so we can sort deterministically regardless of filesystem enumeration.
    let mut files: Vec<FileToPackage> = Vec::new();
    for entry in walkdir::WalkDir::new(in_dir)
        .follow_links(false)
        .into_iter()
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(in_dir)
            .expect("walkdir yields paths under in_dir");
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel_str = path_to_slash(rel)?;
        if !should_include_rel_path(&rel_str) {
            continue;
        }

        files.push(FileToPackage {
            rel_path: rel_str,
            bytes: fs::read(entry.path())
                .with_context(|| format!("read {}", entry.path().display()))?,
        });
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    if files.is_empty() {
        bail!(
            "no files found under {} (after filtering hidden/metadata files)",
            in_dir.display()
        );
    }

    iso9660::write_iso9660_joliet(out_iso, volume_id, source_date_epoch, &files)
        .with_context(|| format!("write {}", out_iso.display()))?;

    Ok(())
}

fn should_include_rel_path(rel_path: &str) -> bool {
    // Skip hidden directories (e.g. `.git/`, `.vs/`) and macOS resource forks.
    if rel_path
        .split('/')
        .any(|c| c.starts_with('.') || c.eq_ignore_ascii_case("__MACOSX"))
    {
        return false;
    }

    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    if file_name.starts_with('.') {
        return false;
    }

    let lower = file_name.to_ascii_lowercase();
    if matches!(lower.as_str(), "thumbs.db" | "ehthumbs.db" | "desktop.ini") {
        return false;
    }

    true
}

fn path_to_slash(path: &Path) -> Result<String> {
    // Require UTF-8 so output paths are stable and independent of platform encoding details.
    let mut components = Vec::<String>::new();
    for c in path.components() {
        let s = c
            .as_os_str()
            .to_str()
            .with_context(|| format!("path component is not valid UTF-8: {}", path.display()))?;
        if s.is_empty() {
            continue;
        }
        components.push(s.to_string());
    }

    if components.is_empty() {
        bail!("unexpected empty relative path for {}", path.display());
    }

    Ok(components.join("/"))
}
