use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str;

fn parse_devices_cmd_vars(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        let lower = l.to_ascii_lowercase();
        if lower == "rem"
            || lower.starts_with("rem ")
            || lower.starts_with("::")
            || lower.starts_with("@echo")
        {
            continue;
        }

        // Support:
        //   set VAR=value
        //   set "VAR=value"
        if !(lower.starts_with("set ") || lower.starts_with("set\t") || lower == "set") {
            continue;
        }

        let rest = l.get(3..).unwrap_or("").trim_start();
        if rest.is_empty() {
            continue;
        }

        let (name, value) = if rest.starts_with('"') {
            let inner = rest
                .strip_prefix('"')
                .and_then(|s| s.split_once('"'))
                .map(|(inner, _after)| inner)
                .unwrap_or("");
            let Some((k, v)) = inner.split_once('=') else {
                continue;
            };
            (k.trim().to_string(), v.to_string())
        } else {
            let Some((k, v)) = rest.split_once('=') else {
                continue;
            };
            (k.trim().to_string(), v.trim().to_string())
        };

        if name.is_empty() {
            continue;
        }
        vars.insert(name.to_ascii_uppercase(), value);
    }

    vars
}

fn load_contract_json(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
}

fn contract_device<'a>(
    contract_path: &Path,
    contract: &'a serde_json::Value,
    name: &str,
) -> &'a serde_json::Value {
    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{}: expected top-level devices[] array",
                contract_path.display()
            )
        });
    devices
        .iter()
        .find(|d| d.get("device").and_then(|v| v.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("{}: missing device entry {name:?}", contract_path.display()))
}

fn json_str<'a>(contract_path: &Path, v: &'a serde_json::Value, field: &str) -> &'a str {
    v.get(field).and_then(|x| x.as_str()).unwrap_or_else(|| {
        panic!(
            "{}: missing/invalid string field {field}",
            contract_path.display()
        )
    })
}

fn json_array_strings(contract_path: &Path, v: &serde_json::Value, field: &str) -> Vec<String> {
    v.get(field)
        .and_then(|x| x.as_array())
        .unwrap_or_else(|| {
            panic!(
                "{}: missing/invalid array field {field}",
                contract_path.display()
            )
        })
        .iter()
        .map(|x| {
            x.as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "{}: {field} entries must be strings",
                        contract_path.display()
                    )
                })
                .to_string()
        })
        .collect()
}

fn json_u64(contract_path: &Path, v: &serde_json::Value, field: &str) -> Option<u64> {
    v.get(field).and_then(|x| x.as_u64()).or_else(|| {
        if v.get(field).is_some() {
            panic!(
                "{}: {field} must be a number when present",
                contract_path.display()
            );
        }
        None
    })
}

fn json_opt_str(contract_path: &Path, v: &serde_json::Value, field: &str) -> Option<String> {
    let raw = v.get(field)?;
    Some(
        raw.as_str()
            .unwrap_or_else(|| {
                panic!(
                    "{}: {field} must be a string when present",
                    contract_path.display()
                )
            })
            .to_string(),
    )
}

#[test]
fn virtio_win_device_contract_only_overrides_service_names() -> anyhow::Result<()> {
    let packager_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = packager_root.join("..").join("..").join("..");

    let base_contract = repo_root.join("docs").join("windows-device-contract.json");
    let virtio_contract = repo_root
        .join("docs")
        .join("windows-device-contract-virtio-win.json");

    let base_cmd = aero_packager::generate_guest_tools_devices_cmd_bytes(&base_contract)?;
    let virtio_cmd = aero_packager::generate_guest_tools_devices_cmd_bytes(&virtio_contract)?;

    let base_cmd = str::from_utf8(&base_cmd)?;
    let virtio_cmd = str::from_utf8(&virtio_cmd)?;

    let base_vars = parse_devices_cmd_vars(base_cmd);
    let virtio_vars = parse_devices_cmd_vars(virtio_cmd);

    for required in [
        "AERO_VIRTIO_BLK_SERVICE",
        "AERO_VIRTIO_NET_SERVICE",
        "AERO_VIRTIO_INPUT_SERVICE",
        "AERO_VIRTIO_SND_SERVICE",
        "AERO_GPU_SERVICE",
        "AERO_VIRTIO_BLK_HWIDS",
        "AERO_VIRTIO_NET_HWIDS",
        "AERO_VIRTIO_INPUT_HWIDS",
        "AERO_VIRTIO_SND_HWIDS",
        "AERO_GPU_HWIDS",
    ] {
        assert!(
            base_vars.contains_key(required),
            "base devices.cmd is missing {required}"
        );
        assert!(
            virtio_vars.contains_key(required),
            "virtio-win devices.cmd is missing {required}"
        );
    }

    // Ensure the virtio-win contract stays aligned with the emulator-presented HWIDs in the
    // canonical contract; it should differ only in service/INF naming.
    for hwids_var in [
        "AERO_VIRTIO_BLK_HWIDS",
        "AERO_VIRTIO_NET_HWIDS",
        "AERO_VIRTIO_INPUT_HWIDS",
        "AERO_VIRTIO_SND_HWIDS",
        "AERO_GPU_HWIDS",
    ] {
        assert_eq!(
            base_vars.get(hwids_var),
            virtio_vars.get(hwids_var),
            "virtio-win contract should not change {hwids_var}"
        );
    }

    assert_eq!(virtio_vars["AERO_VIRTIO_BLK_SERVICE"], "viostor");
    assert_eq!(virtio_vars["AERO_VIRTIO_NET_SERVICE"], "netkvm");
    assert_eq!(virtio_vars["AERO_VIRTIO_INPUT_SERVICE"], "vioinput");
    assert_eq!(virtio_vars["AERO_VIRTIO_SND_SERVICE"], "viosnd");
    assert_eq!(virtio_vars["AERO_GPU_SERVICE"], "aerogpu");

    // Additionally validate that the virtio-win contract JSON does not drift in fields that
    // drive Guest Tools and packager validation (PCI IDs / HWIDs / virtio_device_type).
    //
    // The virtio-win variant should differ only in the driver naming surface (service + INF).
    let base_json = load_contract_json(&base_contract);
    let virtio_json = load_contract_json(&virtio_contract);

    assert_eq!(
        json_str(&base_contract, &base_json, "contract_version"),
        json_str(&virtio_contract, &virtio_json, "contract_version"),
        "virtio-win contract_version should match the canonical contract_version so drift is visible"
    );

    // Contract names are intentionally distinct so packaged devices.cmd can identify which
    // contract was used as the source of truth for generated config.
    assert_ne!(
        json_str(&base_contract, &base_json, "contract_name"),
        json_str(&virtio_contract, &virtio_json, "contract_name"),
        "virtio-win contract_name should differ from the canonical contract_name"
    );

    let expected_overrides = [
        ("virtio-blk", "viostor", "viostor.inf", Some(2u64)),
        ("virtio-net", "netkvm", "netkvm.inf", Some(1u64)),
        ("virtio-input", "vioinput", "vioinput.inf", Some(18u64)),
        ("virtio-snd", "viosnd", "viosnd.inf", Some(25u64)),
        // AeroGPU is not part of virtio-win; it should remain unchanged.
        ("aero-gpu", "aerogpu", "aerogpu_dx11.inf", None),
    ];

    for (device_name, virtio_service, virtio_inf, virtio_type) in expected_overrides {
        let base_dev = contract_device(&base_contract, &base_json, device_name);
        let virtio_dev = contract_device(&virtio_contract, &virtio_json, device_name);

        assert_eq!(
            json_str(&base_contract, base_dev, "pci_vendor_id").to_ascii_lowercase(),
            json_str(&virtio_contract, virtio_dev, "pci_vendor_id").to_ascii_lowercase(),
            "{device_name}: pci_vendor_id drift between canonical and virtio-win contract"
        );
        assert_eq!(
            json_str(&base_contract, base_dev, "pci_device_id").to_ascii_lowercase(),
            json_str(&virtio_contract, virtio_dev, "pci_device_id").to_ascii_lowercase(),
            "{device_name}: pci_device_id drift between canonical and virtio-win contract"
        );
        assert_eq!(
            json_opt_str(&base_contract, base_dev, "pci_device_id_transitional")
                .map(|s| s.to_ascii_lowercase()),
            json_opt_str(&virtio_contract, virtio_dev, "pci_device_id_transitional")
                .map(|s| s.to_ascii_lowercase()),
            "{device_name}: pci_device_id_transitional drift between canonical and virtio-win contract"
        );
        assert_eq!(
            json_array_strings(&base_contract, base_dev, "hardware_id_patterns"),
            json_array_strings(&virtio_contract, virtio_dev, "hardware_id_patterns"),
            "{device_name}: hardware_id_patterns drift between canonical and virtio-win contract"
        );
        assert_eq!(
            json_u64(&base_contract, base_dev, "virtio_device_type"),
            json_u64(&virtio_contract, virtio_dev, "virtio_device_type"),
            "{device_name}: virtio_device_type drift between canonical and virtio-win contract"
        );

        // Naming surface expected for the virtio-win variant.
        assert_eq!(
            json_str(&virtio_contract, virtio_dev, "driver_service_name"),
            virtio_service,
            "{device_name}: unexpected virtio-win driver_service_name"
        );
        assert_eq!(
            json_str(&virtio_contract, virtio_dev, "inf_name"),
            virtio_inf,
            "{device_name}: unexpected virtio-win inf_name"
        );
        assert_eq!(
            json_u64(&virtio_contract, virtio_dev, "virtio_device_type"),
            virtio_type,
            "{device_name}: unexpected virtio-win virtio_device_type"
        );

        if device_name == "aero-gpu" {
            // AeroGPU should not change between contract variants.
            assert_eq!(
                json_str(&virtio_contract, virtio_dev, "driver_service_name"),
                json_str(&base_contract, base_dev, "driver_service_name"),
                "{device_name}: driver_service_name should match canonical contract"
            );
            assert_eq!(
                json_str(&virtio_contract, virtio_dev, "inf_name"),
                json_str(&base_contract, base_dev, "inf_name"),
                "{device_name}: inf_name should match canonical contract"
            );
        }
    }

    Ok(())
}
