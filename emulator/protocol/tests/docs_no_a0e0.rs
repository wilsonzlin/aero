use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn contains_needle(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_uppercase()
        .contains(&needle.to_ascii_uppercase())
}

fn collect_text_docs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_dir() {
            collect_text_docs(&path, out);
            continue;
        }

        if !file_type.is_file() {
            continue;
        }

        match path.extension().and_then(|ext| ext.to_str()) {
            Some("md") | Some("json") => out.push(path),
            _ => {}
        }
    }
}

#[test]
fn docs_do_not_reference_retired_a0e0_abi() {
    let docs_root = repo_root().join("docs");
    assert!(
        docs_root.is_dir(),
        "expected docs directory at {}",
        docs_root.display()
    );

    let mut docs = Vec::new();
    collect_text_docs(&docs_root, &mut docs);

    let mut offenders = Vec::new();
    for path in docs {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };

        if contains_needle(&text, "A0E0") {
            offenders.push(path);
        }
    }

    assert!(
        offenders.is_empty(),
        "docs must not reference the retired A0E0 AeroGPU ABI; offending files:\n{}",
        offenders
            .iter()
            .map(|p| format!("- {}", p.strip_prefix(repo_root()).unwrap_or(p).display()))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

