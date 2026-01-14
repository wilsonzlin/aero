use std::path::{Path, PathBuf};

use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_file(path: &Path) -> String {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{}: failed to read file: {err}", path.display()));
    // Be tolerant of UTF-8 BOMs produced by some editors/tools.
    text.strip_prefix('\u{feff}').unwrap_or(&text).to_string()
}

fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn hex_u32_grouped_by_3(v: u32) -> String {
    // Format as `0x.._..._...` grouping from the right in 3-hex-digit chunks. This matches the
    // common literal style used in docs/tests for values like `0x40_000`.
    let hex = format!("{v:x}");
    let mut groups: Vec<&str> = Vec::new();
    let mut idx = hex.len();
    while idx > 3 {
        groups.push(&hex[idx - 3..idx]);
        idx -= 3;
    }
    groups.push(&hex[..idx]);
    groups.reverse();
    format!("0x{}", groups.join("_"))
}

fn assert_doc_mentions_bar1_vbe_lfb_offset(path: &Path) {
    let text = read_file(path);
    assert!(
        contains_case_insensitive(&text, "AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES"),
        "{} must reference AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES",
        path.display()
    );

    let plain = format!("0x{:x}", AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES);
    let grouped = hex_u32_grouped_by_3(AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES);
    assert!(
        contains_case_insensitive(&text, &plain) || contains_case_insensitive(&text, &grouped),
        "{} must mention the canonical BAR1 VBE LFB offset ({plain} / {grouped})",
        path.display()
    );
}

#[test]
fn docs_aerogpu_bar1_vbe_lfb_offset_matches_protocol_constant() {
    // Keep the key boot-display docs in sync with the canonical protocol constant.
    for rel in [
        "docs/16-aerogpu-vga-vesa-compat.md",
        "docs/abi/aerogpu-pci-identity.md",
    ] {
        let path = repo_root().join(rel);
        assert!(path.is_file(), "expected doc file at {}", path.display());
        assert_doc_mentions_bar1_vbe_lfb_offset(&path);
    }
}
