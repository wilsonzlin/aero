#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

fn should_scan(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext,
        // Rust + docs.
        "rs" | "md" |
        // Web runtime.
        "ts" | "tsx" | "js" | "jsx" | "html" |
        // Config/metadata.
        "toml" | "json" | "yml" | "yaml"
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

#[test]
fn no_lingering_legacy_net_stack_4cc_references_in_docs_and_sources() {
    // Older snapshots used an accidental 4CC for the user-space network stack snapshot blob.
    // The canonical 4CC is `NETS`, and we intentionally avoid mentioning the legacy spelling in
    // docs/tests to prevent confusion.
    const LEGACY_4CC: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let roots = [
        repo_root.join("docs"),
        repo_root.join("crates"),
        repo_root.join("web"),
        repo_root.join("tests"),
        repo_root.join("instructions"),
    ];

    let mut hits = Vec::new();
    for root in roots {
        scan_dir(&root, &LEGACY_4CC, &mut hits);
    }

    assert!(
        hits.is_empty(),
        "lingering legacy net-stack 4CC reference(s) found: {hits:?}",
    );
}
