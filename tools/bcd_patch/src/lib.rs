use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub fn as_bool(self) -> bool {
        match self {
            Toggle::On => true,
            Toggle::Off => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchOptions {
    pub testsigning: Toggle,
    pub nointegritychecks: Toggle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFileResult {
    pub path: PathBuf,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win7TreePatchReport {
    pub patched: Vec<PatchFileResult>,
    pub missing: Vec<String>,
}

pub fn resolve_case_insensitive_path(root: &Path, segments: &[&str]) -> anyhow::Result<Option<PathBuf>> {
    let mut current = root.to_path_buf();
    for (idx, seg) in segments.iter().enumerate() {
        if !current.is_dir() {
            return Ok(None);
        }

        let mut matches = Vec::new();
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name.eq_ignore_ascii_case(seg) {
                matches.push(entry.path());
            }
        }

        match matches.len() {
            0 => return Ok(None),
            1 => current = matches.remove(0),
            _ => {
                let display_root = if idx == 0 { root } else { &current };
                anyhow::bail!(
                    "ambiguous case-insensitive match for path segment {seg:?} under {}",
                    display_root.display()
                );
            }
        }
    }

    Ok(Some(current))
}

pub fn patch_win7_tree(root: &Path, opts: PatchOptions, strict: bool) -> anyhow::Result<Win7TreePatchReport> {
    if !root.is_dir() {
        anyhow::bail!("root is not a directory: {}", root.display());
    }

    let targets: [(&str, &[&str]); 3] = [
        ("boot/BCD", &["boot", "BCD"]),
        ("efi/microsoft/boot/BCD", &["efi", "microsoft", "boot", "BCD"]),
        (
            "Windows/System32/Config/BCD-Template",
            &["Windows", "System32", "Config", "BCD-Template"],
        ),
    ];

    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for (label, segments) in targets {
        match resolve_case_insensitive_path(root, segments)? {
            Some(path) => resolved.push((label.to_string(), path)),
            None => missing.push(label.to_string()),
        }
    }

    if strict && !missing.is_empty() {
        anyhow::bail!("missing {} required BCD store(s): {}", missing.len(), missing.join(", "));
    }

    let mut patched = Vec::new();
    for (_label, path) in resolved {
        let changed = patch_bcd_store_file(&path, opts)?;
        patched.push(PatchFileResult { path, changed });
    }

    Ok(Win7TreePatchReport { patched, missing })
}

fn parse_toggle(value: &str) -> Option<Toggle> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(Toggle::On),
        "off" | "0" | "false" | "no" => Some(Toggle::Off),
        _ => None,
    }
}

fn format_toggle(toggle: Toggle) -> &'static str {
    match toggle {
        Toggle::On => "on",
        Toggle::Off => "off",
    }
}

/// Patch a single BCD store file in-place.
///
/// This project treats test fixtures as "synthetic BCD hives": simple key/value text files.
/// Real Windows BCD stores are registry hives (`regf`) and require proper hive mutation logic.
///
/// The wrapper CLI (`bcd_patch win7-tree`) intentionally calls this library function so that
/// all mutation logic stays in one place.
pub fn patch_bcd_store_file(path: &Path, opts: PatchOptions) -> anyhow::Result<bool> {
    let data = fs::read(path)?;
    let content = String::from_utf8_lossy(&data);

    let mut lines: Vec<String> = Vec::new();
    let mut seen_testsigning = false;
    let mut seen_nointegritychecks = false;
    let mut changed = false;

    for raw_line in content.lines() {
        let mut line = raw_line.to_string();
        if let Some((k, v)) = raw_line.split_once('=') {
            let key = k.trim();
            let val = v.trim();
            if key.eq_ignore_ascii_case("testsigning") {
                seen_testsigning = true;
                let existing = parse_toggle(val)
                    .ok_or_else(|| anyhow::anyhow!("invalid testsigning value {val:?} in {}", path.display()))?;
                if existing != opts.testsigning {
                    changed = true;
                }
                line = format!("testsigning={}", format_toggle(opts.testsigning));
            } else if key.eq_ignore_ascii_case("nointegritychecks") {
                seen_nointegritychecks = true;
                let existing = parse_toggle(val).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid nointegritychecks value {val:?} in {}",
                        path.display()
                    )
                })?;
                if existing != opts.nointegritychecks {
                    changed = true;
                }
                line = format!("nointegritychecks={}", format_toggle(opts.nointegritychecks));
            }
        }

        lines.push(line);
    }

    if !seen_testsigning {
        changed = true;
        lines.push(format!("testsigning={}", format_toggle(opts.testsigning)));
    }
    if !seen_nointegritychecks {
        changed = true;
        lines.push(format!(
            "nointegritychecks={}",
            format_toggle(opts.nointegritychecks)
        ));
    }

    // Always end the file with a trailing newline for stable diffs.
    let mut out = lines.join("\n");
    out.push('\n');

    if changed {
        fs::write(path, out.as_bytes())?;
    }

    Ok(changed)
}
