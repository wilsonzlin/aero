use std::collections::HashMap;
use std::path::PathBuf;
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

    Ok(())
}

