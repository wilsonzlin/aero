use std::collections::BTreeSet;
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

fn inf_add_service_names(inf_text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in inf_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        if !line.to_ascii_lowercase().starts_with("addservice") {
            continue;
        }
        let Some((_, rhs)) = line.split_once('=') else {
            continue;
        };
        let name = rhs.split(',').next().unwrap_or("").trim();
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

fn inf_strings_section(inf_text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut in_strings = false;
    for raw_line in inf_text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_strings = trimmed.eq_ignore_ascii_case("[Strings]");
            continue;
        }
        if !in_strings {
            continue;
        }

        let line = trimmed.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let key = lhs.trim();
        let value = rhs.trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        out.insert(key.to_string(), value.to_string());
    }
    out
}

fn inf_has_line_containing_all(inf_text: &str, needles: &[&str]) -> bool {
    for raw_line in inf_text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with(';') {
            continue;
        }
        let line = trimmed.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let upper = line.to_ascii_uppercase();
        if needles
            .iter()
            .all(|needle| upper.contains(&needle.to_ascii_uppercase()))
        {
            return true;
        }
    }
    false
}

fn inf_contains_any_hardware_id_pattern(inf_text: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }

    let patterns_upper: Vec<String> = patterns.iter().map(|p| p.to_ascii_uppercase()).collect();
    for raw_line in inf_text.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        let line_upper = line.to_ascii_uppercase();
        if patterns_upper.iter().any(|p| line_upper.contains(p)) {
            return true;
        }
    }

    false
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
    assert_eq!(
        require_json_str(aerogpu, "inf_name"),
        "aerogpu_dx11.inf"
    );

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
    // Avoid embedding the exact legacy HWID literal in this source file so repo-wide greps for
    // deprecated AeroGPU IDs can stay focused on legacy/archived locations.
    let legacy_vendor_id = "1AED";
    let legacy_vendor = format!("VEN_{legacy_vendor_id}");
    let legacy_hwid = format!("PCI\\{legacy_vendor}&DEV_0001");
    assert!(
        !contains_case_insensitive(&patterns, &legacy_hwid),
        "hardware_id_patterns for aero-gpu must not include legacy bring-up HWID {legacy_hwid}; the canonical Windows device contract is A3A0-only (got {patterns:?})",
    );

    let aerogpu_inf_path = root.join("drivers/aerogpu/packaging/win7/aerogpu_dx11.inf");
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
        "aerogpu_dx11.inf must contain {expected_hwid_short:?}"
    );
    let expected_add_service = format!(
        "AddService = {}",
        require_json_str(aerogpu, "driver_service_name")
    );
    assert!(
        contains_needle(&aerogpu_inf_text, &expected_add_service),
        "aerogpu_dx11.inf must contain {expected_add_service:?} (case-insensitive)"
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
    assert_eq!(aerogpu_cells[5], "`aerogpu_dx11.inf`");

    // Make sure we don't keep stale contract text around under a different name.
    assert!(!contains_needle(&contract_text, "A0E0"));
    assert!(!contains_needle(&contract_md_text, "A0E0"));
    // This repository previously had an early prototype AeroGPU Windows stack using vendor 1AE0.
    // That vendor ID is deprecated and must never appear in the canonical binding contract.
    let legacy_vendor_id = "1AE0";
    assert!(!contains_needle(&contract_text, legacy_vendor_id));
    assert!(!contains_needle(&contract_md_text, legacy_vendor_id));
    // Historical contract drafts used a different INF name; keep the canonical contract pinned to
    // `drivers/aerogpu/packaging/win7/aerogpu_dx11.inf`.
    assert!(!contains_needle(&contract_text, "aero-gpu.inf"));
    assert!(!contains_needle(&contract_md_text, "aero-gpu.inf"));
    // The contract must only reference the canonical driver packages under `drivers/` (not the
    // removed legacy prototype tree that used to live under the top-level `guest` directory).
    //
    // Avoid embedding the deprecated path literal directly in this source file so repo-wide grep
    // checks can enforce its absence in docs without tripping on this test itself.
    let legacy_guest_windows_slash = format!("{}/{}", "guest", "windows");
    let legacy_guest_windows_backslash = format!("{}\\{}", "guest", "windows");
    assert!(!contains_needle(
        &contract_text,
        &legacy_guest_windows_slash
    ));
    assert!(!contains_needle(
        &contract_md_text,
        &legacy_guest_windows_slash
    ));
    assert!(!contains_needle(
        &contract_text,
        &legacy_guest_windows_backslash
    ));
    assert!(!contains_needle(
        &contract_md_text,
        &legacy_guest_windows_backslash
    ));
}

fn contains_needle(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_uppercase()
        .contains(&needle.to_ascii_uppercase())
}

#[test]
fn win7_aerogpu_infs_register_umds_with_expected_registry_types() {
    // UMD registration keys are the most common source of Win7 bring-up failures:
    // - wrong value type (REG_SZ vs REG_MULTI_SZ)
    // - missing WOW64 keys on x64
    // - wrong naming convention (base name vs filename-with-extension)
    let root = repo_root();

    let infs = [
        (
            root.join("drivers/aerogpu/packaging/win7/aerogpu.inf"),
            false,
        ),
        (
            root.join("drivers/aerogpu/packaging/win7/aerogpu_dx11.inf"),
            true,
        ),
    ];

    for (inf_path, is_dx11) in infs {
        let inf_text = std::fs::read_to_string(&inf_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", inf_path.display()));

        // Common: D3D9 registration uses base name (no .dll) and must be REG_MULTI_SZ.
        assert!(
            contains_needle(
                &inf_text,
                "HKR,,InstalledDisplayDrivers,%REG_MULTI_SZ%,\"aerogpu_d3d9\""
            ),
            "{} must register x86 D3D9 via InstalledDisplayDrivers REG_MULTI_SZ base name",
            inf_path.display()
        );
        assert!(
            contains_needle(
                &inf_text,
                "HKR,,InstalledDisplayDrivers,%REG_MULTI_SZ%,\"aerogpu_d3d9_x64\""
            ),
            "{} must register x64 D3D9 via InstalledDisplayDrivers REG_MULTI_SZ base name",
            inf_path.display()
        );
        assert!(
            contains_needle(
                &inf_text,
                "HKR,,InstalledDisplayDriversWow,%REG_MULTI_SZ%,\"aerogpu_d3d9\""
            ),
            "{} must register WOW64 D3D9 via InstalledDisplayDriversWow REG_MULTI_SZ base name",
            inf_path.display()
        );

        // Copy placement: ensure the x64 INF copies WOW64 DLLs into SysWOW64 explicitly (not by
        // filesystem redirection).
        assert!(
            inf_has_line_containing_all(&inf_text, &["AeroGPU_UMD_Wow64.CopyFiles", "10,SysWOW64"]),
            "{} must copy WOW64 UMDs into SysWOW64 via DestinationDirs",
            inf_path.display()
        );

        if is_dx11 {
            // D3D10/11 registration uses filename (with .dll) and must be REG_SZ.
            assert!(
                contains_needle(
                    &inf_text,
                    "HKR,,UserModeDriverName,%REG_SZ%,\"aerogpu_d3d10.dll\""
                ),
                "{} must register x86 D3D10/11 via UserModeDriverName REG_SZ filename",
                inf_path.display()
            );
            assert!(
                contains_needle(
                    &inf_text,
                    "HKR,,UserModeDriverName,%REG_SZ%,\"aerogpu_d3d10_x64.dll\""
                ),
                "{} must register x64 D3D10/11 via UserModeDriverName REG_SZ filename",
                inf_path.display()
            );
            assert!(
                contains_needle(
                    &inf_text,
                    "HKR,,UserModeDriverNameWow,%REG_SZ%,\"aerogpu_d3d10.dll\""
                ),
                "{} must register WOW64 D3D10/11 via UserModeDriverNameWow REG_SZ filename",
                inf_path.display()
            );
        } else {
            // D3D9-only INF should remove stale D3D10/11 registration when switching from the
            // DX11-capable package.
            assert!(
                inf_has_line_containing_all(&inf_text, &["DelReg", "AeroGPU_Device_DelReg"]),
                "{} must delete stale D3D10/11 UMD registration via DelReg",
                inf_path.display()
            );
            assert!(
                contains_needle(&inf_text, "HKR,,UserModeDriverName"),
                "{} must delete UserModeDriverName under HKR in the DelReg section",
                inf_path.display()
            );
            assert!(
                contains_needle(&inf_text, "HKR,,UserModeDriverNameWow"),
                "{} must delete UserModeDriverNameWow under HKR in the DelReg section",
                inf_path.display()
            );
        }

        // Ensure type helper tokens are pinned to the expected AddReg flag values.
        let strings = inf_strings_section(&inf_text);
        assert_eq!(
            strings.get("REG_MULTI_SZ").map(String::as_str),
            Some("0x00010000"),
            "{} must define REG_MULTI_SZ=0x00010000 in [Strings]",
            inf_path.display()
        );
        assert_eq!(
            strings.get("REG_DWORD").map(String::as_str),
            Some("0x00010001"),
            "{} must define REG_DWORD=0x00010001 in [Strings]",
            inf_path.display()
        );
        if is_dx11 {
            assert_eq!(
                strings.get("REG_SZ").map(String::as_str),
                Some("0x00000000"),
                "{} must define REG_SZ=0x00000000 in [Strings]",
                inf_path.display()
            );
        }
    }
}

#[test]
fn windows_device_contract_driver_service_names_match_driver_infs() {
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

    let contract_entry = |name: &str| -> &serde_json::Value {
        devices
            .iter()
            .find(|device| device.get("device").and_then(|d| d.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("contract JSON is missing the {name} device entry"))
    };

    let cases = [
        (
            "virtio-blk",
            root.join("drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf"),
        ),
        (
            "virtio-net",
            root.join("drivers/windows7/virtio-net/inf/aero_virtio_net.inf"),
        ),
        (
            "virtio-snd",
            root.join("drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf"),
        ),
        (
            "virtio-input",
            root.join("drivers/windows7/virtio-input/inf/aero_virtio_input.inf"),
        ),
        (
            "aero-gpu",
            root.join("drivers/aerogpu/packaging/win7/aerogpu_dx11.inf"),
        ),
    ];

    for (device_name, inf_path) in cases {
        let contract = contract_entry(device_name);
        let contract_service = require_json_str(contract, "driver_service_name");
        let contract_inf_name = require_json_str(contract, "inf_name");
        let contract_hwids: Vec<String> = require_json_array(contract, "hardware_id_patterns")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("hardware_id_patterns entries must be strings"))
                    .to_string()
            })
            .collect();

        assert!(
            inf_path.is_file(),
            "expected INF for {device_name} to exist at {}",
            inf_path.display()
        );
        assert_eq!(
            inf_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
            contract_inf_name,
            "windows-device-contract.json {device_name}.inf_name must match the in-tree INF filename"
        );

        let inf_text = std::fs::read_to_string(&inf_path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", inf_path.display()));
        assert!(
            inf_contains_any_hardware_id_pattern(&inf_text, &contract_hwids),
            "windows-device-contract.json hardware_id_patterns for {device_name} must include at least one HWID that appears in INF {}.\nContract patterns:\n{contract_hwids:#?}",
            inf_path.display()
        );

        let services: BTreeSet<String> = inf_add_service_names(&inf_text)
            .into_iter()
            .map(|service| service.to_ascii_lowercase())
            .collect();
        assert!(
            !services.is_empty(),
            "INF {} does not contain any AddService entries",
            inf_path.display()
        );
        assert_eq!(
            services.len(),
            1,
            "INF {} must install exactly one service via AddService, got: {services:?}",
            inf_path.display()
        );
        let inf_service = services.iter().next().unwrap();
        assert_eq!(
            contract_service.to_ascii_lowercase(),
            *inf_service,
            "windows-device-contract.json {device_name}.driver_service_name must match INF AddService name (INF: {})",
            inf_path.display()
        );
    }
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
    // Convert the needle to uppercase so we can match case-insensitively against the file bytes.
    let forbidden_hex = format!("0x{}", "1AE0").to_ascii_uppercase();
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
        let Ok(mut buf) = std::fs::read(&path) else {
            // Skip unreadable files (shouldn't happen for tracked files, but keep this robust).
            continue;
        };
        buf.make_ascii_uppercase();

        if buf
            .windows(forbidden_vendor.len())
            .any(|w| w == forbidden_vendor)
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

#[test]
fn no_aerogpu_1aed_tokens_outside_quarantined_legacy_locations() {
    // Guard against accidentally reintroducing the deprecated AeroGPU legacy bring-up PCI identity
    // beyond the intended compatibility surface. The legacy identity (1AED) is still supported for
    // optional compatibility testing, but it should remain confined to:
    //   - docs/abi/aerogpu-pci-identity.md (mapping doc / source-of-truth context)
    //   - drivers/aerogpu/legacy/ (quarantined legacy INF bindings)
    //   - drivers/aerogpu/protocol/legacy/
    //   - drivers/aerogpu/packaging/win7/README.md (install docs reference both HWIDs)
    //   - drivers/aerogpu/packaging/win7/legacy/
    //   - prototype/legacy-win7-aerogpu-1ae0/ (archived prototype tree)
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

    let allowed_mapping_doc = b"docs/abi/aerogpu-pci-identity.md";
    let allowed_prefixes: &[&[u8]] = &[
        b"drivers/aerogpu/legacy/",
        b"drivers/aerogpu/protocol/legacy/",
        b"drivers/aerogpu/packaging/win7/README.md",
        b"drivers/aerogpu/packaging/win7/legacy/",
        b"prototype/legacy-win7-aerogpu-1ae0/",
    ];

    // Build forbidden needles without embedding the full tokens in this source file so repo-wide
    // greps for deprecated AeroGPU IDs can stay focused on legacy/archived locations.
    let forbidden_vendor = format!("VEN_{}", "1AED");
    // Convert the needle to uppercase so we can match case-insensitively against the file bytes.
    let forbidden_hex = format!("0x{}", "1AED").to_ascii_uppercase();
    let forbidden_vendor = forbidden_vendor.as_bytes();
    let forbidden_hex = forbidden_hex.as_bytes();

    let mut hits: Vec<String> = Vec::new();
    for rel in files.split(|b| *b == 0) {
        if rel.is_empty() {
            continue;
        }
        if rel == allowed_mapping_doc
            || allowed_prefixes
                .iter()
                .any(|prefix| rel.starts_with(prefix))
        {
            continue;
        }
        let rel_str = String::from_utf8_lossy(rel);
        let path = root.join(rel_str.as_ref());
        let Ok(mut buf) = std::fs::read(&path) else {
            continue;
        };
        buf.make_ascii_uppercase();

        if buf
            .windows(forbidden_vendor.len())
            .any(|w| w == forbidden_vendor)
            || buf.windows(forbidden_hex.len()).any(|w| w == forbidden_hex)
        {
            hits.push(rel_str.into_owned());
        }
    }

    assert!(
        hits.is_empty(),
        "found deprecated AeroGPU 1AED tokens outside quarantined legacy locations: {hits:#?}"
    );
}

#[test]
fn no_legacy_aerogpu_protocol_header_references_outside_archived_prototype_tree() {
    // The legacy prototype protocol header (aerogpu_protocol + .h) was part of an archived driver
    // stack and should not be referenced by the supported in-tree driver/protocol headers. Keep
    // references confined to the archived prototype tree only.
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

    // Avoid embedding the full legacy header name token in this source file.
    let legacy_header_stem = format!("{}{}", "aerogpu_", "protocol");
    let legacy_header_name = format!("{legacy_header_stem}.{}", "h");
    // Match case-insensitively by uppercasing the file bytes before scanning.
    let needle = legacy_header_name.to_ascii_uppercase();
    let needle = needle.as_bytes();

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
        let Ok(mut buf) = std::fs::read(&path) else {
            continue;
        };
        buf.make_ascii_uppercase();

        if buf.windows(needle.len()).any(|w| w == needle) {
            hits.push(rel_str.into_owned());
        }
    }

    assert!(
        hits.is_empty(),
        "found deprecated AeroGPU prototype header references outside archive tree: {hits:#?}"
    );
}
