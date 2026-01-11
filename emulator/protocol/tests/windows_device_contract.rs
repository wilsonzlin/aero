use std::path::{Path, PathBuf};
use std::process::Command;

use aero_protocol::aerogpu::aerogpu_pci::{
    AEROGPU_PCI_BAR0_SIZE_BYTES, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID,
    AEROGPU_PCI_PROG_IF, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID,
    AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, AEROGPU_PCI_VENDOR_ID,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_hex_u16(input: &str) -> u16 {
    let trimmed = input.trim();
    let no_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    u16::from_str_radix(no_prefix, 16).unwrap_or_else(|err| panic!("bad hex u16 {input:?}: {err}"))
}

fn require_json_str<'a>(v: &'a serde_json::Value, field: &str) -> &'a str {
    v.get(field)
        .unwrap_or_else(|| panic!("missing {field}"))
        .as_str()
        .unwrap_or_else(|| panic!("{field} must be a JSON string"))
}

fn require_json_array<'a>(v: &'a serde_json::Value, field: &str) -> &'a [serde_json::Value] {
    v.get(field)
        .unwrap_or_else(|| panic!("missing {field}"))
        .as_array()
        .unwrap_or_else(|| panic!("{field} must be a JSON array"))
}

fn contains_case_insensitive(haystack: &[String], needle: &str) -> bool {
    haystack
        .iter()
        .any(|value| value.eq_ignore_ascii_case(needle))
}

#[test]
fn windows_device_contract_aerogpu_matches_protocol_constants() {
    // These are part of the stable Win7 driver ABI (`drivers/aerogpu/protocol/aerogpu_pci.h`).
    assert_eq!(AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, 0x03);
    assert_eq!(AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, 0x00);
    assert_eq!(AEROGPU_PCI_PROG_IF, 0x00);
    assert_eq!(AEROGPU_PCI_BAR0_SIZE_BYTES, 64u32 * 1024u32);

    assert_eq!(AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, AEROGPU_PCI_VENDOR_ID);
    assert_eq!(AEROGPU_PCI_SUBSYSTEM_ID, 0x0001);

    let root = repo_root();
    let contract_path = root.join("docs/windows-device-contract.json");
    let contract_text = std::fs::read_to_string(&contract_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", contract_path.display()));
    let contract_json: serde_json::Value = serde_json::from_str(&contract_text)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", contract_path.display()));

    let devices = contract_json
        .get("devices")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("contract JSON must contain devices[] array"));

    let aerogpu = devices
        .iter()
        .find(|device| device.get("device").and_then(|d| d.as_str()) == Some("aero-gpu"))
        .unwrap_or_else(|| panic!("contract JSON is missing the aero-gpu device entry"));

    let vendor_id = parse_hex_u16(require_json_str(aerogpu, "pci_vendor_id"));
    let device_id = parse_hex_u16(require_json_str(aerogpu, "pci_device_id"));
    assert_eq!(vendor_id, AEROGPU_PCI_VENDOR_ID);
    assert_eq!(device_id, AEROGPU_PCI_DEVICE_ID);

    assert_eq!(require_json_str(aerogpu, "driver_service_name"), "aerogpu");
    assert_eq!(require_json_str(aerogpu, "inf_name"), "aerogpu.inf");

    let expected_hwid_with_subsys = format!(
        "PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}&SUBSYS_{subsys_id:04X}{subsys_vendor:04X}",
        subsys_id = AEROGPU_PCI_SUBSYSTEM_ID,
        subsys_vendor = AEROGPU_PCI_SUBSYSTEM_VENDOR_ID
    );
    let expected_hwid_short = format!("PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}");

    let patterns: Vec<String> = require_json_array(aerogpu, "hardware_id_patterns")
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("hardware_id_patterns entries must be strings"))
                .to_string()
        })
        .collect();

    assert!(
        contains_case_insensitive(&patterns, &expected_hwid_with_subsys),
        "hardware_id_patterns for aero-gpu must include {expected_hwid_with_subsys:?} (got {patterns:?})",
    );
    assert!(
        contains_case_insensitive(&patterns, &expected_hwid_short),
        "hardware_id_patterns for aero-gpu must include {expected_hwid_short:?} (got {patterns:?})",
    );
    assert!(
        !contains_case_insensitive(&patterns, "PCI\\VEN_1AED&DEV_0001"),
        "hardware_id_patterns for aero-gpu must not include legacy bring-up HWID PCI\\VEN_1AED&DEV_0001; the canonical Windows device contract is A3A0-only (got {patterns:?})",
    );

    let aerogpu_inf_path = root.join("drivers/aerogpu/packaging/win7/aerogpu.inf");
    assert!(
        aerogpu_inf_path.is_file(),
        "expected AeroGPU Win7 INF to exist at {}",
        aerogpu_inf_path.display()
    );
    assert_eq!(
        aerogpu_inf_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        require_json_str(aerogpu, "inf_name"),
        "windows-device-contract.json inf_name must match the in-tree INF filename"
    );

    let aerogpu_inf_text = std::fs::read_to_string(&aerogpu_inf_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", aerogpu_inf_path.display()));
    assert!(
        aerogpu_inf_text.contains(&expected_hwid_short),
        "aerogpu.inf must contain {expected_hwid_short:?}"
    );

    // Keep the human-readable contract document in sync too (at least for the AeroGPU row).
    let contract_md_path = root.join("docs/windows-device-contract.md");
    let contract_md_text = std::fs::read_to_string(&contract_md_path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", contract_md_path.display()));
    let aerogpu_row = contract_md_text
        .lines()
        .find(|line| line.trim_start().starts_with("| Aero GPU |"))
        .unwrap_or_else(|| panic!("missing Aero GPU row in {}", contract_md_path.display()));
    let aerogpu_cells: Vec<&str> = aerogpu_row
        .trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect();
    assert_eq!(
        aerogpu_cells.len(),
        6,
        "expected 6 columns in Aero GPU markdown table row, got {aerogpu_cells:?}"
    );
    assert_eq!(aerogpu_cells[0], "Aero GPU");
    assert_eq!(aerogpu_cells[1], "`A3A0:0001`");
    assert_eq!(aerogpu_cells[2], "`A3A0:0001`");
    assert!(
        aerogpu_cells[3].contains("`03/00/00`"),
        "Aero GPU class code must contain `03/00/00` (got: {:?})",
        aerogpu_cells[3]
    );
    assert_eq!(aerogpu_cells[4], "`aerogpu`");
    assert_eq!(aerogpu_cells[5], "`aerogpu.inf`");

    // Make sure we don't keep stale contract text around under a different name.
    assert!(!contains_needle(&contract_text, "A0E0"));
    assert!(!contains_needle(&contract_md_text, "A0E0"));
    // This repository previously had an early prototype AeroGPU Windows stack using vendor 1AE0.
    // That vendor ID is deprecated and must never appear in the canonical binding contract.
    let legacy_vendor = concat!("VEN_", "1AE0");
    assert!(!contains_needle(&contract_text, legacy_vendor));
    assert!(!contains_needle(&contract_md_text, legacy_vendor));
    // Historical contract drafts used a different INF name; keep the canonical contract pinned to
    // `drivers/aerogpu/packaging/win7/aerogpu.inf`.
    assert!(!contains_needle(&contract_text, "aero-gpu.inf"));
    assert!(!contains_needle(&contract_md_text, "aero-gpu.inf"));
    // The contract must only reference the canonical driver packages under `drivers/` (not the
    // removed `guest/windows/` prototype tree).
    assert!(!contains_needle(&contract_text, "guest/windows"));
    assert!(!contains_needle(&contract_md_text, "guest/windows"));
    assert!(!contains_needle(&contract_text, "guest\\\\windows"));
    assert!(!contains_needle(&contract_md_text, "guest\\\\windows"));
}

fn contains_needle(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_uppercase()
        .contains(&needle.to_ascii_uppercase())
}

#[test]
fn windows_device_contract_is_located_in_docs() {
    let root = repo_root();
    assert!(
        Path::new(&root)
            .join("docs/windows-device-contract.json")
            .is_file(),
        "docs/windows-device-contract.json must exist relative to repo root"
    );
}

#[test]
fn no_aerogpu_1ae0_tokens_outside_archived_prototype_tree() {
    // Guard against accidentally reintroducing the deprecated AeroGPU 1AE0 PCI identity into the
    // active codebase/docs. The archived prototype lives under:
    //   prototype/legacy-win7-aerogpu-1ae0/
    //
    // Keep this in sync with the task requirement:
    //   Searching for the deprecated vendor-id tokens (e.g. `VEN_` + `1AE0`, `0x` + `1AE0`)
    //   should only match inside archived/legacy locations.
    let root = repo_root();

    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files", "-z"])
        .output()
        .expect("failed to run git ls-files");
    assert!(
        output.status.success(),
        "git ls-files failed with status {}",
        output.status
    );

    let files = output.stdout;
    let archive_prefix = b"prototype/legacy-win7-aerogpu-1ae0/";

    // Build forbidden needles without embedding the full token in the source, so this file
    // doesn't itself trip the repo-wide grep rule we're trying to enforce.
    let forbidden_vendor = format!("VEN_{}", "1AE0");
    let forbidden_hex = format!("0x{}", "1AE0");
    let forbidden_vendor = forbidden_vendor.as_bytes();
    let forbidden_hex = forbidden_hex.as_bytes();

    let mut hits: Vec<String> = Vec::new();
    for rel in files.split(|b| *b == 0) {
        if rel.is_empty() {
            continue;
        }
        if rel.starts_with(archive_prefix) {
            continue;
        }
        let rel_str = String::from_utf8_lossy(rel);
        let path = root.join(rel_str.as_ref());
        let Ok(buf) = std::fs::read(&path) else {
            // Skip unreadable files (shouldn't happen for tracked files, but keep this robust).
            continue;
        };

        if buf.windows(forbidden_vendor.len()).any(|w| w == forbidden_vendor)
            || buf.windows(forbidden_hex.len()).any(|w| w == forbidden_hex)
        {
            hits.push(rel_str.into_owned());
        }
    }

    assert!(
        hits.is_empty(),
        "found deprecated AeroGPU 1AE0 tokens outside archive tree: {hits:#?}"
    );
}
