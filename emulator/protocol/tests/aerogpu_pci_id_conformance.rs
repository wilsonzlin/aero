use std::fs;
use std::path::{Path, PathBuf};

use aero_protocol::aerogpu::aerogpu_pci::{AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_VENDOR_ID};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("failed to locate repo root")
}

fn read_file(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{}: failed to read file: {err}", path.display()))
}

fn assert_file_contains(path: &Path, needle: &str) {
    let content = read_file(path);
    assert!(
        content.contains(needle),
        "{} is out of sync: expected to contain `{needle}`",
        path.display()
    );
}

fn assert_file_not_contains(path: &Path, needle: &str) {
    let content = read_file(path);
    assert!(
        !content.contains(needle),
        "{} is out of sync: expected NOT to contain `{needle}`",
        path.display()
    );
}

fn file_contains_in_non_comment_lines(path: &Path, needle: &str) -> bool {
    read_file(path)
        .lines()
        .filter(|line| !line.trim_start().starts_with(';'))
        .any(|line| line.contains(needle))
}

fn parse_u16_literal(lit: &str, path: &Path, define: &str) -> u16 {
    let lit = lit.trim();
    let lit = lit.trim_end_matches(&['u', 'U', 'l', 'L'][..]);
    if let Some(hex) = lit.strip_prefix("0x").or_else(|| lit.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).unwrap_or_else(|err| {
            panic!(
                "{}: failed to parse {} literal `{}` as hex u16: {err}",
                path.display(),
                define,
                lit
            )
        })
    } else {
        lit.parse::<u16>().unwrap_or_else(|err| {
            panic!(
                "{}: failed to parse {} literal `{}` as decimal u16: {err}",
                path.display(),
                define,
                lit
            )
        })
    }
}

fn parse_c_define_u16(contents: &str, path: &Path, define: &str) -> u16 {
    for (line_no, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(tag) = parts.next() else { continue };
        if tag != "#define" {
            continue;
        }

        let Some(name) = parts.next() else { continue };
        if name != define {
            continue;
        }

        let Some(value) = parts.next() else {
            panic!(
                "{}:{}: malformed #define (missing value): {raw_line}",
                path.display(),
                line_no + 1
            );
        };

        return parse_u16_literal(value, path, define);
    }

    panic!("{}: missing `#define {define} ...`", path.display());
}

#[test]
fn aerogpu_pci_ids_match_repo_contracts() {
    let repo_root = repo_root();

    // Canonical "new ABI" PCI identity lives in `aero-protocol`.
    let new_vendor_id = AEROGPU_PCI_VENDOR_ID;
    let new_device_id = AEROGPU_PCI_DEVICE_ID;
    let new_hwid = format!("PCI\\VEN_{new_vendor_id:04X}&DEV_{new_device_id:04X}");

    // Legacy bring-up ABI PCI identity is defined in the Windows driver header.
    let legacy_header_path = repo_root.join("drivers/aerogpu/protocol/aerogpu_protocol.h");
    let legacy_header = read_file(&legacy_header_path);
    let legacy_vendor_id =
        parse_c_define_u16(&legacy_header, &legacy_header_path, "AEROGPU_PCI_VENDOR_ID");
    let legacy_device_id =
        parse_c_define_u16(&legacy_header, &legacy_header_path, "AEROGPU_PCI_DEVICE_ID");
    let legacy_hwid = format!("PCI\\VEN_{legacy_vendor_id:04X}&DEV_{legacy_device_id:04X}");

    // The shipped driver package + guest-tools target the canonical, versioned ABI (A3A0:0001).
    // The legacy bring-up device model still exists for debugging, but requires a custom INF.
    for relative_path in [
        "drivers/aerogpu/packaging/win7/aerogpu.inf",
        "drivers/aerogpu/packaging/win7/aerogpu_dx11.inf",
    ] {
        let path = repo_root.join(relative_path);
        assert!(
            file_contains_in_non_comment_lines(&path, &new_hwid),
            "{} is out of sync: expected to bind to `{new_hwid}` (in a non-comment line)",
            path.display()
        );

        assert_file_not_contains(&path, &legacy_hwid);
    }

    // Guest Tools config is generated from the canonical Windows device contract, which binds only
    // to the versioned ("AGPU") device by default.
    let devices_cmd_path = repo_root.join("guest-tools/config/devices.cmd");
    assert_file_contains(&devices_cmd_path, &new_hwid);
    let devices_cmd_text = read_file(&devices_cmd_path);
    assert!(
        !devices_cmd_text.contains(&legacy_hwid),
        "{} is out of sync: must not contain legacy bring-up HWID `{legacy_hwid}`",
        devices_cmd_path.display()
    );

    let contract_path = repo_root.join("docs/windows-device-contract.json");
    let contract_text = read_file(&contract_path);
    let contract_json: serde_json::Value = serde_json::from_str(&contract_text)
        .unwrap_or_else(|err| panic!("{}: failed to parse JSON: {err}", contract_path.display()));

    let devices = contract_json
        .get("devices")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{}: expected top-level `devices` array",
                contract_path.display()
            )
        });

    let aero_gpu = devices
        .iter()
        .find(|device| device.get("device").and_then(|v| v.as_str()) == Some("aero-gpu"))
        .unwrap_or_else(|| {
            panic!(
                "{}: missing device entry for `aero-gpu`",
                contract_path.display()
            )
        });

    let expected_vendor_id = format!("0x{new_vendor_id:04X}");
    let expected_device_id = format!("0x{new_device_id:04X}");

    let contract_vendor_id = aero_gpu
        .get("pci_vendor_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "{}: aero-gpu entry missing `pci_vendor_id` string",
                contract_path.display()
            )
        });
    assert_eq!(
        contract_vendor_id,
        expected_vendor_id,
        "{}: aero-gpu pci_vendor_id is `{contract_vendor_id}`, expected `{expected_vendor_id}`",
        contract_path.display()
    );

    let contract_device_id = aero_gpu
        .get("pci_device_id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!(
                "{}: aero-gpu entry missing `pci_device_id` string",
                contract_path.display()
            )
        });
    assert_eq!(
        contract_device_id,
        expected_device_id,
        "{}: aero-gpu pci_device_id is `{contract_device_id}`, expected `{expected_device_id}`",
        contract_path.display()
    );

    let patterns: Vec<String> = aero_gpu
        .get("hardware_id_patterns")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{}: aero-gpu entry missing `hardware_id_patterns` array",
                contract_path.display()
            )
        })
        .iter()
        .map(|value| {
            value.as_str().unwrap_or_else(|| {
                panic!(
                    "{}: aero-gpu hardware_id_patterns must be strings; found {value:?}",
                    contract_path.display()
                )
            })
        })
        .map(str::to_string)
        .collect();

    assert!(
        patterns.contains(&new_hwid),
        "{}: aero-gpu hardware_id_patterns missing `{new_hwid}`. Found: {patterns:?}",
        contract_path.display()
    );
    assert!(
        !patterns.contains(&legacy_hwid),
        "{}: aero-gpu hardware_id_patterns must not include legacy bring-up HWID `{legacy_hwid}`. Found: {patterns:?}",
        contract_path.display()
    );
}
