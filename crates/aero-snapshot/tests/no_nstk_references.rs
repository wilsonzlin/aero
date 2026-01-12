#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn should_skip_dir(path: &Path) -> bool {
    // Avoid scanning build artifacts / third-party dependencies that may exist in local dev or some
    // CI jobs, and which are not part of the repo's source-of-truth.
    //
    // This guard is intended to keep our *source* tree free of the legacy 4CC spelling; scanning
    // generated output is both expensive and may introduce accidental false positives.
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some(".git" | "target" | "node_modules")
    )
}

fn should_scan(path: &Path) -> bool {
    // Skip binary fuzz corpuses (tracked) to avoid accidental false positives.
    if path.components().any(|c| c.as_os_str() == "fuzz")
        && path.components().any(|c| c.as_os_str() == "corpus")
    {
        return false;
    }

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        // Extensionless tracked files in this repo are typically plaintext (Makefile, Dockerfile,
        // etc). Scanning them helps ensure the legacy net-stack 4CC spelling can't reappear in
        // build scripts/manifests.
        return true;
    };

    matches!(
        ext,
        // Rust + docs.
        "rs" | "md" | "txt" |
        // Web runtime.
        "ts" | "tsx" | "mts" | "js" | "jsx" | "mjs" | "cjs" | "html" | "css" |
        // Scripts.
        "py" | "sh" | "ps1" | "psm1" | "cmd" |
        // Go (proxy services).
        "go" | "mod" | "sum" |
        // Assembly / boot sectors.
        "asm" | "S" | "s" |
        // Firmware tables.
        "asl" |
        // Native/driver sources.
        "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "hlsl" | "wgsl" | "glsl" |
        // Driver/build metadata (plaintext).
        "inf" | "sln" | "vcxproj" | "filters" | "props" | "def" | "reg" | "disabled" | "cat" |
        // Config/metadata.
        "toml"
            | "json"
            | "jsonc"
            | "yml"
            | "yaml"
            | "dict"
            | "lock"
            | "xml"
            | "conf"
            | "tf"
            | "hcl"
            | "example"
            | "tmpl"
            | "template"
            | "tftpl"
            | "tpl" |
        // Common dotfile config.
        "gitignore" | "dockerignore" | "gitattributes" | "nvmrc" | "helmignore" |
        // Misc text-like.
        "keep" | "gitkeep" | "pem" | "snap" | "wat" | "dat"
    )
}

fn scan_dir(dir: &Path, legacy: &[u8], hits: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(v) => v,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            scan_dir(&path, legacy, hits);
            continue;
        }

        if !should_scan(&path) {
            continue;
        }

        let len = match entry.metadata().map(|m| m.len()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Keep the test cheap and avoid pulling large files into memory. (The codebase's `.rs`/`.md`
        // files are expected to be small.)
        const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;
        if len > MAX_SCAN_BYTES {
            continue;
        }

        let bytes = match std::fs::read(&path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if bytes.windows(legacy.len()).any(|w| w == legacy) {
            hits.push(path);
        }
    }
}

fn scan_tracked_files_with_git(repo_root: &Path, legacy: &[u8], hits: &mut Vec<PathBuf>) -> bool {
    let output = match Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-files", "-z"])
        .output()
    {
        Ok(v) => v,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }

    for entry in output.stdout.split(|b| *b == 0) {
        if entry.is_empty() {
            continue;
        }
        let rel = match std::str::from_utf8(entry) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = repo_root.join(rel);
        if !should_scan(&path) {
            continue;
        }

        let len = match std::fs::metadata(&path).map(|m| m.len()) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Keep the test cheap and avoid pulling large files into memory.
        const MAX_SCAN_BYTES: u64 = 4 * 1024 * 1024;
        if len > MAX_SCAN_BYTES {
            continue;
        }

        let bytes = match std::fs::read(&path) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if bytes.windows(legacy.len()).any(|w| w == legacy) {
            hits.push(path);
        }
    }

    true
}

#[test]
fn no_lingering_legacy_net_stack_4cc_references_in_repo() {
    // Older snapshots used an accidental 4CC for the user-space network stack snapshot blob.
    // The canonical 4CC is `NETS`, and we intentionally avoid mentioning the legacy spelling in
    // docs/tests to prevent confusion.
    const LEGACY_4CC: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");

    let mut hits = Vec::new();
    if !scan_tracked_files_with_git(&repo_root, &LEGACY_4CC, &mut hits) {
        // Fallback for environments that don't have a git checkout (or lack the `git` binary).
        // This intentionally scans only the project's source roots, not the entire repo root,
        // to avoid picking up untracked developer scratch files.
        let roots = [
            repo_root.join(".github"),
            repo_root.join("bench"),
            repo_root.join("backend"),
            repo_root.join("ci"),
            repo_root.join("crates"),
            repo_root.join("deploy"),
            repo_root.join("docs"),
            repo_root.join("drivers"),
            repo_root.join("emulator"),
            repo_root.join("instructions"),
            repo_root.join("proxy"),
            repo_root.join("scripts"),
            repo_root.join("src"),
            repo_root.join("tests"),
            repo_root.join("tools"),
            repo_root.join("web"),
            repo_root.join("xtask"),
        ];
        for root in roots {
            scan_dir(&root, &LEGACY_4CC, &mut hits);
        }
    }

    assert!(
        hits.is_empty(),
        "lingering legacy net-stack 4CC reference(s) found: {hits:?}",
    );
}
