use aero_devices::pci::profile::{
    PciDeviceProfile, PCI_VENDOR_ID_VIRTIO, VIRTIO_BLK, VIRTIO_INPUT_KEYBOARD, VIRTIO_INPUT_MOUSE,
    VIRTIO_NET, VIRTIO_SND,
};
use std::collections::BTreeMap;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_windows_device_contract_json() -> serde_json::Value {
    // Be tolerant of UTF-8 BOMs produced by some editors/tools.
    let contract = include_str!("../../../docs/windows-device-contract.json");
    let contract = contract.strip_prefix('\u{feff}').unwrap_or(contract);
    serde_json::from_str(contract).expect("parse windows-device-contract.json")
}

fn parse_hex_u16(value: &str) -> u16 {
    let value = value
        .trim()
        .strip_prefix("0x")
        .or_else(|| value.trim().strip_prefix("0X"))
        .unwrap_or(value.trim());
    u16::from_str_radix(value, 16).expect("invalid u16 hex string")
}

fn find_contract_device<'a>(devices: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    devices
        .iter()
        .find(|d| d.get("device").and_then(|v| v.as_str()) == Some(name))
        .unwrap_or_else(|| panic!("windows-device-contract.json missing device entry {name:?}"))
}

fn assert_has_pattern(patterns: &[String], expected: &str) {
    assert!(
        patterns.iter().any(|p| p == expected),
        "windows-device-contract.json missing hardware_id_patterns entry {expected:?}.\n\
         Found:\n{patterns:#?}"
    );
}

fn parse_subsys(pattern: &str) -> Option<(u16, u16)> {
    // Format: PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4 (subsys device ID first, then subsystem vendor).
    let idx = pattern.to_ascii_uppercase().find("&SUBSYS_")?;
    let start = idx + "&SUBSYS_".len();
    let hex = pattern.get(start..start + 8)?;
    let subsys_device = parse_hex_u16(&hex[0..4]);
    let subsys_vendor = parse_hex_u16(&hex[4..8]);
    Some((subsys_vendor, subsys_device))
}

fn assert_contract_matches_profile(profile: PciDeviceProfile, contract: &serde_json::Value) {
    let pci_vendor_id = contract
        .get("pci_vendor_id")
        .and_then(|v| v.as_str())
        .expect("device entry missing pci_vendor_id");
    let pci_device_id = contract
        .get("pci_device_id")
        .and_then(|v| v.as_str())
        .expect("device entry missing pci_device_id");

    assert_eq!(
        parse_hex_u16(pci_vendor_id),
        profile.vendor_id,
        "{}",
        profile.name
    );
    assert_eq!(
        parse_hex_u16(pci_device_id),
        profile.device_id,
        "{}",
        profile.name
    );

    let patterns: Vec<String> = contract
        .get("hardware_id_patterns")
        .and_then(|v| v.as_array())
        .expect("device entry missing hardware_id_patterns")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("hardware_id_patterns must be strings")
                .to_string()
        })
        .collect();

    let expected_ven_dev = format!(
        "PCI\\VEN_{:04X}&DEV_{:04X}",
        profile.vendor_id, profile.device_id
    );
    assert_has_pattern(&patterns, &expected_ven_dev);

    let subsys: Vec<(u16, u16)> = patterns.iter().filter_map(|p| parse_subsys(p)).collect();
    assert!(
        !subsys.is_empty(),
        "expected at least one SUBSYS-qualified HWID pattern for {}",
        profile.name
    );
    assert!(
        subsys
            .iter()
            .any(|(vendor, device)| *vendor == profile.subsystem_vendor_id && *device == profile.subsystem_id),
        "expected a SUBSYS-qualified HWID pattern matching {:04X}:{:04X} for {}.\nFound:\n{subsys:#?}",
        profile.subsystem_vendor_id,
        profile.subsystem_id,
        profile.name,
    );
}

fn inf_installs_service(contents: &str, expected_service: &str) -> bool {
    let expected_service = expected_service.to_ascii_lowercase();

    contents.lines().any(|line| {
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            return false;
        }

        let mut parts = line.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        if key != "addservice" {
            return false;
        }

        let value = parts.next().unwrap_or("").trim();
        let installed_service = value
            .split(|c: char| c == ',' || c.is_whitespace())
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();

        installed_service == expected_service
    })
}

fn inf_functional_text(contents: &str) -> &str {
    // Return the "functional" region of an INF: from the first section header (typically
    // `[Version]`) onward.
    //
    // This intentionally ignores the leading comment/banner block so legacy alias INFs can use a
    // different filename banner while still enforcing byte-for-byte equality of all functional
    // sections/keys.
    const TRIM_LEADING: &[char] = &['\0', ' ', '\t', '\u{feff}'];
    let mut offset = 0usize;
    for line in contents.split_inclusive('\n') {
        let logical = line.trim_end_matches(['\r', '\n']);
        let trimmed = logical.trim_start_matches(TRIM_LEADING);
        if trimmed.is_empty() {
            offset += line.len();
            continue;
        }
        if trimmed.starts_with(';') {
            offset += line.len();
            continue;
        }
        if trimmed.starts_with('[') {
            return &contents[offset..];
        }
        // Unexpected functional content before any section header: treat it as functional to avoid
        // masking drift.
        return &contents[offset..];
    }
    panic!("INF did not contain a section header (e.g. [Version])");
}

fn inf_model_entry_for_hwid(
    contents: &str,
    section_name: &str,
    expected_hwid: &str,
) -> Option<(String, String)> {
    // Parse a single model entry within `section_name` and return:
    //   (device_desc_token, install_section)
    //
    // Example line:
    //   %AeroVirtioKeyboard.DeviceDesc% = AeroVirtioInput_Install.NTx86, PCI\VEN_...
    let expected_hwid_upper = expected_hwid.to_ascii_uppercase();
    let mut current_section = String::new();

    for raw in contents.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case(section_name) {
            continue;
        }
        let mut parts = line.splitn(2, '=');
        let device_desc = parts.next().unwrap_or("").trim();
        let rhs = parts.next().unwrap_or("").trim();
        if device_desc.is_empty() || rhs.is_empty() {
            continue;
        }
        let rhs_parts: Vec<&str> = rhs
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if rhs_parts.len() < 2 {
            continue;
        }
        let install_section = rhs_parts[0];
        let hwid = rhs_parts
            .iter()
            .rev()
            .copied()
            .find(|p| p.to_ascii_uppercase().starts_with("PCI\\VEN_"))?;
        if hwid.to_ascii_uppercase() != expected_hwid_upper {
            continue;
        }
        return Some((device_desc.to_string(), install_section.to_string()));
    }
    None
}

fn inf_model_entries_for_hwid(
    contents: &str,
    section_name: &str,
    expected_hwid: &str,
) -> Vec<(String, String)> {
    let expected_hwid_upper = expected_hwid.to_ascii_uppercase();
    let mut current_section = String::new();
    let mut matches = Vec::new();

    for raw in contents.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case(section_name) {
            continue;
        }
        let mut parts = line.splitn(2, '=');
        let device_desc = parts.next().unwrap_or("").trim();
        let rhs = parts.next().unwrap_or("").trim();
        if device_desc.is_empty() || rhs.is_empty() {
            continue;
        }
        let rhs_parts: Vec<&str> = rhs
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if rhs_parts.len() < 2 {
            continue;
        }
        let install_section = rhs_parts[0];
        let Some(hwid) = rhs_parts
            .iter()
            .rev()
            .copied()
            .find(|p| p.to_ascii_uppercase().starts_with("PCI\\VEN_"))
        else {
            continue;
        };
        if hwid.to_ascii_uppercase() != expected_hwid_upper {
            continue;
        }
        matches.push((device_desc.to_string(), install_section.to_string()));
    }

    matches
}

fn resolve_inf_device_desc(desc: &str, strings: &BTreeMap<String, String>) -> String {
    let d = desc.trim();
    if d.starts_with('%') && d.ends_with('%') && d.len() >= 3 {
        let key = d[1..d.len() - 1].trim().to_ascii_lowercase();
        let value = strings.get(&key).unwrap_or_else(|| {
            panic!("undefined [Strings] token referenced by models section: {desc:?}")
        });
        return value.clone();
    }
    d.to_string()
}

fn inf_strings(contents: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current_section = String::new();
    for raw in contents.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case("Strings") {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let mut value = value.trim();
        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = &value[1..value.len() - 1];
        }
        if !key.is_empty() {
            out.insert(key.to_ascii_lowercase(), value.to_string());
        }
    }
    out
}

fn inf_section_has_token(
    contents: &str,
    section_name: &str,
    key: &str,
    expected_token: &str,
) -> bool {
    let mut current_section = String::new();
    for raw in contents.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case(section_name) {
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        if !lhs.trim().eq_ignore_ascii_case(key) {
            continue;
        }
        for token in rhs.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
            if token.eq_ignore_ascii_case(expected_token) {
                return true;
            }
        }
    }
    false
}

fn parse_inf_addreg_dword(contents: &str, section_name: &str, value_name: &str) -> Option<u32> {
    let mut current_section = String::new();
    for raw in contents.lines() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            current_section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if !current_section.eq_ignore_ascii_case(section_name) {
            continue;
        }

        let parts: Vec<&str> = line.split(',').map(|p| p.trim()).collect();
        if parts.len() < 5 {
            continue;
        }

        // Typical AddReg entry:
        //   HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
        let name = parts[2];
        if !name.eq_ignore_ascii_case(value_name) {
            continue;
        }

        let mut value = parts[4].trim();
        value = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);

        let parsed = if parts[4].trim().starts_with("0x") || parts[4].trim().starts_with("0X") {
            u32::from_str_radix(value, 16).ok()?
        } else {
            value.parse::<u32>().ok()?
        };

        return Some(parsed);
    }

    None
}

#[test]
fn windows_device_contract_virtio_input_matches_pci_profile() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let input = find_contract_device(devices, "virtio-input");

    assert_contract_matches_profile(VIRTIO_INPUT_KEYBOARD, input);
    assert_contract_matches_profile(VIRTIO_INPUT_MOUSE, input);

    let patterns: Vec<String> = input
        .get("hardware_id_patterns")
        .and_then(|v| v.as_array())
        .expect("device entry missing hardware_id_patterns")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("hardware_id_patterns must be strings")
                .to_string()
        })
        .collect();

    assert_has_pattern(&patterns, "PCI\\VEN_1AF4&DEV_1052");
    assert_has_pattern(&patterns, "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4");
    assert_has_pattern(&patterns, "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00111AF4");

    assert_eq!(VIRTIO_INPUT_KEYBOARD.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        input.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aero_virtio_input")
    );
    assert_eq!(
        input.get("inf_name").and_then(|v| v.as_str()),
        Some("aero_virtio_input.inf")
    );
    assert_eq!(
        input.get("virtio_device_type").and_then(|v| v.as_u64()),
        Some(18)
    );
}

#[test]
fn windows_device_contract_virtio_input_inf_installs_declared_service() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let input = find_contract_device(devices, "virtio-input");
    let expected_service = input
        .get("driver_service_name")
        .and_then(|v| v.as_str())
        .expect("device entry missing driver_service_name");
    let inf_name = input
        .get("inf_name")
        .and_then(|v| v.as_str())
        .expect("device entry missing inf_name");

    let inf_path = repo_root()
        .join("drivers/windows7/virtio-input/inf")
        .join(inf_name);
    assert!(
        inf_path.exists(),
        "expected INF referenced by the windows device contract to exist at {}",
        inf_path.display()
    );

    let inf_contents =
        std::fs::read_to_string(&inf_path).expect("read virtio-input INF from repository");
    assert!(
        inf_installs_service(&inf_contents, expected_service),
        "expected {} to install service {expected_service:?} via an AddService directive",
        inf_path.display()
    );
}

#[test]
fn windows_device_contract_virtio_input_inf_uses_distinct_keyboard_mouse_device_descs() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");
    let input = find_contract_device(devices, "virtio-input");
    let inf_name = input
        .get("inf_name")
        .and_then(|v| v.as_str())
        .expect("device entry missing inf_name");

    let inf_path = repo_root()
        .join("drivers/windows7/virtio-input/inf")
        .join(inf_name);
    let inf_contents =
        std::fs::read_to_string(&inf_path).expect("read virtio-input INF from repository");

    let hwid_kbd = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01";
    let hwid_mouse = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01";
    let hwid_fallback = "PCI\\VEN_1AF4&DEV_1052&REV_01";
    let hwid_fallback_revisionless = "PCI\\VEN_1AF4&DEV_1052";
    let hwid_tablet = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01";

    let strings = inf_strings(&inf_contents);

    for section in ["Aero.NTx86", "Aero.NTamd64"] {
        let kbd_entries = inf_model_entries_for_hwid(&inf_contents, section, hwid_kbd);
        assert_eq!(
            kbd_entries.len(),
            1,
            "expected exactly one {hwid_kbd} model entry in [{section}] (found {})",
            kbd_entries.len()
        );
        let (kbd_desc, kbd_install) = kbd_entries[0].clone();

        let mouse_entries = inf_model_entries_for_hwid(&inf_contents, section, hwid_mouse);
        assert_eq!(
            mouse_entries.len(),
            1,
            "expected exactly one {hwid_mouse} model entry in [{section}] (found {})",
            mouse_entries.len()
        );
        let (mouse_desc, mouse_install) = mouse_entries[0].clone();

        let fallback_entries = inf_model_entries_for_hwid(&inf_contents, section, hwid_fallback);
        assert_eq!(
            fallback_entries.len(),
            1,
            "expected exactly one {hwid_fallback} model entry in [{section}] (found {})",
            fallback_entries.len()
        );
        let (fallback_desc, fallback_install) = fallback_entries[0].clone();

        assert_eq!(
            kbd_install, mouse_install,
            "{section}: install section mismatch"
        );
        assert_eq!(
            fallback_install, kbd_install,
            "{section}: generic fallback install section mismatch"
        );

        assert!(
            !kbd_desc.eq_ignore_ascii_case(&mouse_desc),
            "{section}: keyboard/mouse DeviceDesc tokens must be distinct"
        );
        let kbd_desc_str = resolve_inf_device_desc(&kbd_desc, &strings);
        let mouse_desc_str = resolve_inf_device_desc(&mouse_desc, &strings);
        let fallback_desc_str = resolve_inf_device_desc(&fallback_desc, &strings);
        assert_ne!(
            fallback_desc_str.to_ascii_lowercase(),
            kbd_desc_str.to_ascii_lowercase(),
            "{section}: fallback DeviceDesc must be generic (must not equal keyboard)"
        );
        assert_ne!(
            fallback_desc_str.to_ascii_lowercase(),
            mouse_desc_str.to_ascii_lowercase(),
            "{section}: fallback DeviceDesc must be generic (must not equal mouse)"
        );
        assert!(
            inf_model_entry_for_hwid(&inf_contents, section, hwid_fallback_revisionless).is_none(),
            "{section}: canonical INF must not contain revision-less generic fallback model entry {hwid_fallback_revisionless}"
        );
        assert!(
            inf_model_entry_for_hwid(&inf_contents, section, hwid_tablet).is_none(),
            "{section}: virtio-input INF must not contain tablet subsystem model entry {hwid_tablet} (binds via aero_virtio_tablet.inf)"
        );

        // The canonical INF is expected to use these tokens (kept in sync with docs/tests).
        assert_eq!(kbd_desc, "%AeroVirtioKeyboard.DeviceDesc%");
        assert_eq!(mouse_desc, "%AeroVirtioMouse.DeviceDesc%");
        assert_eq!(fallback_desc, "%AeroVirtioInput.DeviceDesc%");
    }
    let kbd_name = strings
        .get("aerovirtiokeyboard.devicedesc")
        .expect("missing AeroVirtioKeyboard.DeviceDesc in [Strings]");
    let mouse_name = strings
        .get("aerovirtiomouse.devicedesc")
        .expect("missing AeroVirtioMouse.DeviceDesc in [Strings]");
    let generic_name = strings
        .get("aerovirtioinput.devicedesc")
        .expect("missing AeroVirtioInput.DeviceDesc in [Strings]");

    assert!(
        !kbd_name.eq_ignore_ascii_case(mouse_name),
        "keyboard and mouse DeviceDesc strings must be distinct"
    );
    assert!(
        !generic_name.eq_ignore_ascii_case(kbd_name),
        "generic fallback DeviceDesc string must not equal keyboard DeviceDesc string"
    );
    assert!(
        !generic_name.eq_ignore_ascii_case(mouse_name),
        "generic fallback DeviceDesc string must not equal mouse DeviceDesc string"
    );
}

#[test]
fn windows_device_contract_virtio_input_alias_inf_is_strict_filename_alias() {
    // `virtio-input.inf.disabled` is a legacy filename alias for the canonical
    // `aero_virtio_input.inf`, kept for compatibility with older tooling/workflows that still
    // reference `virtio-input.inf`.
    //
    // Contract: the alias INF is a *filename alias only*. If it exists, it must match the
    // canonical INF byte-for-byte from the first section header (`[Version]`) onward (only the
    // leading banner/comments may differ).

    let inf_dir = repo_root().join("drivers/windows7/virtio-input/inf");
    let alias_enabled = inf_dir.join("virtio-input.inf");
    let alias_disabled = inf_dir.join("virtio-input.inf.disabled");
    let alias_path = if alias_enabled.exists() {
        alias_enabled.clone()
    } else {
        alias_disabled.clone()
    };

    if !alias_path.exists() {
        panic!(
            "expected virtio-input legacy alias INF to exist at {} or {}",
            alias_enabled.display(),
            alias_disabled.display(),
        );
    }

    let canonical_path = inf_dir.join("aero_virtio_input.inf");
    let canonical_contents =
        std::fs::read_to_string(&canonical_path).expect("read canonical virtio-input INF");
    let alias_contents =
        std::fs::read_to_string(&alias_path).expect("read virtio-input alias INF from repository");

    // The alias may have a different banner/comment block (different filename), but
    // from `[Version]` onward it must be identical.
    assert_eq!(
        inf_functional_text(&alias_contents),
        inf_functional_text(&canonical_contents),
        "virtio-input alias INF must be byte-identical to the canonical INF from [Version] onward.\ncanonical: {}\nalias: {}",
        canonical_path.display(),
        alias_path.display(),
    );
}

#[test]
fn windows_device_contract_aero_virtio_input_tablet_contract_and_inf_are_consistent() {
    let contract = parse_windows_device_contract_json();
    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");
    let tablet = find_contract_device(devices, "aero-virtio-input-tablet");

    assert_eq!(
        parse_hex_u16(
            tablet
                .get("pci_vendor_id")
                .and_then(|v| v.as_str())
                .expect("device entry missing pci_vendor_id"),
        ),
        PCI_VENDOR_ID_VIRTIO
    );
    assert_eq!(
        parse_hex_u16(
            tablet
                .get("pci_device_id")
                .and_then(|v| v.as_str())
                .expect("device entry missing pci_device_id"),
        ),
        0x1052
    );
    assert_eq!(
        tablet.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aero_virtio_input")
    );
    assert_eq!(
        tablet.get("inf_name").and_then(|v| v.as_str()),
        Some("aero_virtio_tablet.inf")
    );

    let patterns: Vec<String> = tablet
        .get("hardware_id_patterns")
        .and_then(|v| v.as_array())
        .expect("device entry missing hardware_id_patterns")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("hardware_id_patterns must be strings")
                .to_string()
        })
        .collect();
    assert_has_pattern(&patterns, "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01");
    assert_has_pattern(&patterns, "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00121AF4");

    let inf_path = repo_root()
        .join("drivers/windows7/virtio-input/inf")
        .join("aero_virtio_tablet.inf");
    assert!(
        inf_path.exists(),
        "expected aero_virtio_tablet.inf to exist at {}",
        inf_path.display()
    );
    let inf_contents =
        std::fs::read_to_string(&inf_path).expect("read aero_virtio_tablet.inf from repository");

    assert!(
        inf_installs_service(&inf_contents, "aero_virtio_input"),
        "expected {} to install service \"aero_virtio_input\" via an AddService directive",
        inf_path.display()
    );

    // Tablet binding is intentionally SUBSYS-only: it must not also include the no-SUBSYS strict
    // fallback HWID (`...&REV_01`), since that fallback is provided by the keyboard/mouse INF.
    // The tablet HWID is more specific, so it wins over the fallback when both packages are
    // present.
    let hwid_tablet = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01";
    let hwid_kbd = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01";
    let hwid_mouse = "PCI\\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01";
    let hwid_fallback = "PCI\\VEN_1AF4&DEV_1052&REV_01";

    for section in ["Aero.NTx86", "Aero.NTamd64"] {
        let (tablet_desc, _tablet_install) =
            inf_model_entry_for_hwid(&inf_contents, section, hwid_tablet)
                .unwrap_or_else(|| panic!("missing {hwid_tablet} model entry in [{section}]"));
        assert_eq!(
            tablet_desc, "%AeroVirtioTablet.DeviceDesc%",
            "{section}: unexpected DeviceDesc token for tablet model entry"
        );

        assert!(
            inf_model_entry_for_hwid(&inf_contents, section, hwid_kbd).is_none(),
            "{section}: aero_virtio_tablet.inf must not include keyboard model entry {hwid_kbd}"
        );
        assert!(
            inf_model_entry_for_hwid(&inf_contents, section, hwid_mouse).is_none(),
            "{section}: aero_virtio_tablet.inf must not include mouse model entry {hwid_mouse}"
        );
        assert!(
            inf_model_entry_for_hwid(&inf_contents, section, hwid_fallback).is_none(),
            "{section}: aero_virtio_tablet.inf must not include generic fallback model entry {hwid_fallback}"
        );
    }

    let strings = inf_strings(&inf_contents);
    assert!(
        strings.contains_key("aerovirtiotablet.devicedesc"),
        "expected AeroVirtioTablet.DeviceDesc in [Strings]"
    );
}

#[test]
fn windows_device_contract_virtio_snd_matches_pci_profile() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let snd = find_contract_device(devices, "virtio-snd");

    assert_contract_matches_profile(VIRTIO_SND, snd);

    // Additional sanity checks for the virtio-snd driver binding contract.
    assert_eq!(VIRTIO_SND.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        snd.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aero_virtio_snd")
    );
    assert_eq!(
        snd.get("inf_name").and_then(|v| v.as_str()),
        Some("aero_virtio_snd.inf")
    );
    assert_eq!(
        snd.get("virtio_device_type").and_then(|v| v.as_u64()),
        Some(25)
    );
}

#[test]
fn windows_device_contract_virtio_snd_inf_opts_into_msi() {
    // The Aero virtio-snd driver supports INTx as a contract-v1 baseline, and opts into
    // message-signaled interrupts (MSI/MSI-X) via INF registry keys on Windows 7.
    let inf_path = repo_root()
        .join("drivers/windows7/virtio-snd/inf")
        .join("aero_virtio_snd.inf");
    assert!(
        inf_path.exists(),
        "expected virtio-snd INF to exist at {}",
        inf_path.display()
    );

    let inf_contents =
        std::fs::read_to_string(&inf_path).expect("read virtio-snd INF from repository");

    assert!(
        inf_section_has_token(
            &inf_contents,
            "AeroVirtioSnd_Install.NT.HW",
            "AddReg",
            "AeroVirtioSnd_InterruptManagement_AddReg"
        ),
        "expected {} to reference AeroVirtioSnd_InterruptManagement_AddReg from [AeroVirtioSnd_Install.NT.HW]",
        inf_path.display()
    );

    let msi_supported = parse_inf_addreg_dword(
        &inf_contents,
        "AeroVirtioSnd_InterruptManagement_AddReg",
        "MSISupported",
    )
    .expect("expected MSI opt-in (MSISupported) to be present in AeroVirtioSnd_InterruptManagement_AddReg");
    assert_eq!(msi_supported, 1, "MSISupported must be set to 1");

    let msg_limit = parse_inf_addreg_dword(
        &inf_contents,
        "AeroVirtioSnd_InterruptManagement_AddReg",
        "MessageNumberLimit",
    )
    .expect(
        "expected MSI opt-in (MessageNumberLimit) to be present in AeroVirtioSnd_InterruptManagement_AddReg",
    );
    assert!(
        msg_limit >= 5,
        "MessageNumberLimit must be >= 5 (config + 4 queues); got {msg_limit}"
    );
}

#[test]
fn windows_device_contract_virtio_blk_matches_pci_profile() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let blk = find_contract_device(devices, "virtio-blk");

    assert_contract_matches_profile(VIRTIO_BLK, blk);

    assert_eq!(VIRTIO_BLK.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        blk.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aero_virtio_blk")
    );
    assert_eq!(
        blk.get("inf_name").and_then(|v| v.as_str()),
        Some("aero_virtio_blk.inf")
    );
    assert_eq!(
        blk.get("virtio_device_type").and_then(|v| v.as_u64()),
        Some(2)
    );
}

#[test]
fn windows_device_contract_virtio_net_matches_pci_profile() {
    let contract = parse_windows_device_contract_json();

    let devices = contract
        .get("devices")
        .and_then(|v| v.as_array())
        .expect("windows-device-contract.json missing devices array");

    let net = find_contract_device(devices, "virtio-net");

    assert_contract_matches_profile(VIRTIO_NET, net);

    assert_eq!(VIRTIO_NET.vendor_id, PCI_VENDOR_ID_VIRTIO);
    assert_eq!(
        net.get("driver_service_name").and_then(|v| v.as_str()),
        Some("aero_virtio_net")
    );
    assert_eq!(
        net.get("inf_name").and_then(|v| v.as_str()),
        Some("aero_virtio_net.inf")
    );
    assert_eq!(
        net.get("virtio_device_type").and_then(|v| v.as_u64()),
        Some(1)
    );
}
