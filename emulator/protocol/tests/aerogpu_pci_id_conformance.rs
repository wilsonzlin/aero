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

fn assert_file_contains_noncomment_line(path: &Path, needle: &str) {
    let content = read_file(path);
    let has = content.lines().any(|raw_line| {
        let line = raw_line.trim_start();
        if line.is_empty() {
            return false;
        }
        // INF comments start with ';'. Batch file comments start with `rem` (case-insensitive) or `::`.
        if path.extension().and_then(|ext| ext.to_str()) == Some("inf") {
            if line.starts_with(';') {
                return false;
            }
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("cmd") {
            if line.starts_with("::") {
                return false;
            }
            if line.len() >= 3 && line[..3].eq_ignore_ascii_case("rem") {
                return false;
            }
        }
        line.contains(needle)
    });
    assert!(
        has,
        "{} is out of sync: expected a non-comment line to contain `{needle}`",
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
    let legacy_header_path =
        repo_root.join("drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h");
    let legacy_header = read_file(&legacy_header_path);
    let legacy_vendor_id =
        parse_c_define_u16(&legacy_header, &legacy_header_path, "AEROGPU_PCI_VENDOR_ID");
    let legacy_device_id =
        parse_c_define_u16(&legacy_header, &legacy_header_path, "AEROGPU_PCI_DEVICE_ID");
    let legacy_hwid = format!("PCI\\VEN_{legacy_vendor_id:04X}&DEV_{legacy_device_id:04X}");

    // The shipped driver package targets the canonical, versioned ABI (A3A0:0001) by default.
    // The legacy bring-up device model still exists behind the legacy emulator device model feature
    // (`emulator/aerogpu-legacy`) and uses the legacy INFs under `drivers/aerogpu/packaging/win7/legacy/`.
    //
    // The Windows device contract + Guest Tools config intentionally target the canonical binding
    // only; legacy bring-up installs are out of scope for Guest Tools.
    for relative_path in [
        "drivers/aerogpu/packaging/win7/aerogpu.inf",
        "drivers/aerogpu/packaging/win7/aerogpu_dx11.inf",
    ] {
        let path = repo_root.join(relative_path);
        assert_file_contains_noncomment_line(&path, &new_hwid);
        assert_file_not_contains(&path, &legacy_hwid);
    }

    for relative_path in [
        "drivers/aerogpu/packaging/win7/legacy/aerogpu.inf",
        "drivers/aerogpu/packaging/win7/legacy/aerogpu_dx11.inf",
        "drivers/aerogpu/legacy/aerogpu.inf",
        "drivers/aerogpu/legacy/aerogpu_dx11.inf",
    ] {
        let path = repo_root.join(relative_path);
        assert_file_contains_noncomment_line(&path, &legacy_hwid);
        assert_file_not_contains(&path, &new_hwid);
    }

    // Guest Tools config is generated from the Windows device contract and intentionally targets
    // only the canonical (versioned ABI) AeroGPU PCI HWID.
    let devices_cmd_path = repo_root.join("guest-tools/config/devices.cmd");
    assert_file_contains_noncomment_line(&devices_cmd_path, &new_hwid);
    assert_file_not_contains(&devices_cmd_path, &legacy_hwid);

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

#[test]
fn aerogpu_ci_package_manifest_includes_wow64_umds_for_dx11_inf() {
    let repo_root = repo_root();

    let manifest_path = repo_root.join("drivers/aerogpu/ci-package.json");
    let manifest_text = read_file(&manifest_path);
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest_text)
        .unwrap_or_else(|err| panic!("{}: failed to parse JSON: {err}", manifest_path.display()));

    let stages_dx11_inf = {
        let mut staged = false;

        // `infFiles` is the canonical mechanism for staging a DX11-capable INF at the package
        // root (see drivers/aerogpu/ci-package.json and packaging/win7/README.md).
        staged |= match manifest_json.get("infFiles") {
            Some(value) => value
                .as_array()
                .unwrap_or_else(|| panic!("{}: expected `infFiles` array", manifest_path.display()))
                .iter()
                .map(|value| {
                    value.as_str().unwrap_or_else(|| {
                        panic!(
                            "{}: infFiles entries must be strings; found {value:?}",
                            manifest_path.display()
                        )
                    })
                })
                .any(|path| path.ends_with("aerogpu_dx11.inf")),
            None => {
                // If `infFiles` is omitted, CI will attempt to stage all .inf files under the
                // driver directory. Treat the existence of any DX11-capable INFs as a signal that
                // WOW64 payloads are required.
                repo_root
                    .join("drivers/aerogpu/packaging/win7/aerogpu_dx11.inf")
                    .is_file()
                    || repo_root
                        .join("drivers/aerogpu/packaging/win7/legacy/aerogpu_dx11.inf")
                        .is_file()
                    || repo_root
                        .join("drivers/aerogpu/legacy/aerogpu_dx11.inf")
                        .is_file()
            }
        };

        // The legacy DX11 INF lives under legacy/ and is staged via `additionalFiles`.
        if let Some(value) = manifest_json.get("additionalFiles") {
            staged |= value
                .as_array()
                .unwrap_or_else(|| {
                    panic!(
                        "{}: expected `additionalFiles` array",
                        manifest_path.display()
                    )
                })
                .iter()
                .map(|value| {
                    value.as_str().unwrap_or_else(|| {
                        panic!(
                            "{}: additionalFiles entries must be strings; found {value:?}",
                            manifest_path.display()
                        )
                    })
                })
                .any(|path| path.ends_with("aerogpu_dx11.inf"));
        }

        staged
    };

    if !stages_dx11_inf {
        return;
    }

    let wow64_files: Vec<&str> = manifest_json
        .get("wow64Files")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{}: expected `wow64Files` array", manifest_path.display()))
        .iter()
        .map(|value| {
            value.as_str().unwrap_or_else(|| {
                panic!(
                    "{}: wow64Files entries must be strings; found {value:?}",
                    manifest_path.display()
                )
            })
        })
        .collect();

    // DX11-capable AeroGPU INFs reference both 32-bit UMDs in their amd64 file lists; CI must
    // stage them into the x64 package directory via `wow64Files` so `Inf2Cat` and Win7 x64
    // installs succeed.
    for required in ["aerogpu_d3d9.dll", "aerogpu_d3d10.dll"] {
        assert!(
            wow64_files.contains(&required),
            "{}: wow64Files missing `{required}` required by DX11-capable AeroGPU INFs. Found: {wow64_files:?}",
            manifest_path.display()
        );
    }
}

#[test]
fn aerogpu_ci_package_manifest_stages_only_dx11_inf_at_package_root() {
    let repo_root = repo_root();

    let manifest_path = repo_root.join("drivers/aerogpu/ci-package.json");
    let manifest_text = read_file(&manifest_path);
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest_text)
        .unwrap_or_else(|err| panic!("{}: failed to parse JSON: {err}", manifest_path.display()));

    let inf_files: Vec<String> = manifest_json
        .get("infFiles")
        .and_then(|value| value.as_array())
        .unwrap_or_else(|| panic!("{}: expected `infFiles` array", manifest_path.display()))
        .iter()
        .map(|value| {
            value.as_str().unwrap_or_else(|| {
                panic!(
                    "{}: infFiles entries must be strings; found {value:?}",
                    manifest_path.display()
                )
            })
        })
        .map(str::to_string)
        .collect();

    // CI staging policy: keep the AeroGPU package root unambiguous by staging only the DX11-capable
    // INF there. (Staging both aerogpu.inf and aerogpu_dx11.inf at the root means both match the same
    // HWID, which can confuse installs and driver selection unless carefully ranked.)
    assert_eq!(
        inf_files.len(),
        1,
        "{path}: expected infFiles to contain exactly one INF (single-INF policy). Found: {inf_files:?}",
        path = manifest_path.display()
    );

    let basename = inf_files[0].replace('\\', "/");
    let basename = basename.rsplit('/').next().unwrap_or(&basename);
    assert!(
        basename.eq_ignore_ascii_case("aerogpu_dx11.inf"),
        "{path}: expected the sole staged INF to be aerogpu_dx11.inf. Found: {inf_files:?}",
        path = manifest_path.display()
    );
}

#[test]
fn aerogpu_install_script_prefers_dx11_inf_in_ci_layout() {
    let repo_root = repo_root();

    // `install.cmd` is shipped in CI packages under packaging\win7\ but should default to the
    // DX11-capable INF at the *package root* (two levels above the script) when it is present.
    //
    // This keeps the default install path unambiguous for anyone copying `out/packages/aerogpu/<arch>/`
    // into a Win7 VM.
    let install_cmd = repo_root.join("drivers/aerogpu/packaging/win7/install.cmd");
    assert_file_contains_noncomment_line(&install_cmd, "INF_FILE=aerogpu_dx11.inf");
    assert_file_contains_noncomment_line(&install_cmd, r"..\..\aerogpu_dx11.inf");
}

#[test]
fn aerogpu_sign_test_script_detects_ci_package_root_when_dx11_inf_is_staged() {
    let repo_root = repo_root();

    // `sign_test.cmd` is shipped alongside CI packages under packaging\win7\. The CI package root
    // uses the single-INF layout, staging only aerogpu_dx11.inf at the package root, so the helper
    // must detect `..\..\aerogpu_dx11.inf` and operate from the package root.
    let sign_test_cmd = repo_root.join("drivers/aerogpu/packaging/win7/sign_test.cmd");
    assert_file_contains_noncomment_line(&sign_test_cmd, r#"..\..\aerogpu_dx11.inf"#);
}
