use anyhow::{Context as _, Result};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct WindowsDeviceContract {
    pub schema_version: u32,
    pub contract_name: String,
    pub contract_version: String,
    pub devices: Vec<WindowsDeviceContractDevice>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WindowsDeviceContractDevice {
    pub device: String,
    pub pci_vendor_id: String,
    pub pci_device_id: String,
    pub hardware_id_patterns: Vec<String>,
    pub driver_service_name: String,
    pub inf_name: String,
    #[serde(default)]
    pub virtio_device_type: Option<u32>,
}

pub fn load_windows_device_contract(path: &Path) -> Result<WindowsDeviceContract> {
    let (contract, _bytes) = load_windows_device_contract_with_bytes(path)?;
    Ok(contract)
}

pub fn load_windows_device_contract_with_bytes(path: &Path) -> Result<(WindowsDeviceContract, Vec<u8>)> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let contract: WindowsDeviceContract =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    validate_windows_device_contract(&contract)
        .with_context(|| format!("validate {}", path.display()))?;
    Ok((contract, bytes))
}

fn validate_windows_device_contract(contract: &WindowsDeviceContract) -> Result<()> {
    if contract.schema_version != 1 {
        anyhow::bail!(
            "unsupported windows-device-contract schema_version {} (expected 1)",
            contract.schema_version
        );
    }
    if contract.contract_name.trim().is_empty() {
        anyhow::bail!("windows-device-contract contract_name must be non-empty");
    }
    if contract.contract_version.trim().is_empty() {
        anyhow::bail!("windows-device-contract contract_version must be non-empty");
    }
    if contract.devices.is_empty() {
        anyhow::bail!("windows-device-contract contains no devices");
    }

    let mut seen_device_names = HashSet::<String>::new();
    let base_pci_hwid_re =
        Regex::new(r"(?i)PCI\\VEN_[0-9A-F]{4}&DEV_[0-9A-F]{4}").expect("valid PCI HWID regex");

    for device in &contract.devices {
        let name = device.device.trim();
        if name.is_empty() {
            anyhow::bail!("windows-device-contract has a device entry with an empty device name");
        }
        let name_lower = name.to_ascii_lowercase();
        if name_lower.starts_with("virtio-") && device.virtio_device_type.is_none() {
            anyhow::bail!(
                "windows-device-contract device {name} is missing virtio_device_type (required for virtio-* devices)"
            );
        }
        if !seen_device_names.insert(name_lower.clone()) {
            anyhow::bail!("windows-device-contract contains duplicate device entry: {name}");
        }

        let pci_vendor_id = parse_hex_u16(&device.pci_vendor_id)
            .with_context(|| format!("{name}.pci_vendor_id"))?;
        let pci_device_id = parse_hex_u16(&device.pci_device_id)
            .with_context(|| format!("{name}.pci_device_id"))?;
        let expected_substr = format!("VEN_{pci_vendor_id:04X}&DEV_{pci_device_id:04X}");

        // Contract v1 (AERO-W7-VIRTIO) is strict about virtio-pci being modern-only and revision-gated
        // (REV_01). Enforce these invariants at load time so both Guest Tools packaging and CI tooling
        // fail fast if a contract edit would silently drift back to transitional IDs.
        if let Some(virtio_device_type) = device.virtio_device_type {
            if pci_vendor_id != 0x1AF4 {
                anyhow::bail!(
                    "windows-device-contract device {name} has unexpected pci_vendor_id {:#06X} (expected 0x1AF4 for devices with virtio_device_type)",
                    pci_vendor_id
                );
            }

            // Modern virtio-pci device IDs are `0x1040 + virtio_device_type`.
            //
            // Transitional virtio-pci device IDs are `0x1000..0x103F`; requiring the modern mapping
            // prevents accidental regressions to transitional IDs in the contract JSON.
            let expected_pci_device_id = 0x1040u32 + virtio_device_type;
            if expected_pci_device_id > u16::MAX as u32 {
                anyhow::bail!(
                    "windows-device-contract device {name} has virtio_device_type {virtio_device_type} which overflows the PCI device-id space"
                );
            }
            let expected_pci_device_id = expected_pci_device_id as u16;
            if pci_device_id != expected_pci_device_id {
                anyhow::bail!(
                    "windows-device-contract device {name} has pci_device_id {:#06X} which does not match virtio_device_type {virtio_device_type} (expected {:#06X} = 0x1040 + virtio_device_type)",
                    pci_device_id,
                    expected_pci_device_id
                );
            }

            // Ensure the contract explicitly includes the revision-gated HWID (REV_01). Windows also
            // enumerates less-specific HWIDs without REV_ qualifiers, but the *device* must present
            // revision 0x01 for contract v1.
            let mut has_rev_01 = false;
            for hwid in &device.hardware_id_patterns {
                let hwid = hwid.trim();
                let upper = hwid.to_ascii_uppercase();
                let mut search = upper.as_str();
                while let Some(idx) = search.find("&REV_") {
                    let start = idx + "&REV_".len();
                    if search.len() < start + 2 {
                        anyhow::bail!(
                            "windows-device-contract device {name} has malformed hardware_id_patterns entry (truncated REV_ qualifier): {hwid}"
                        );
                    }
                    let rev = &search[start..start + 2];
                    if rev != "01" {
                        anyhow::bail!(
                            "windows-device-contract device {name} has unsupported virtio PCI revision ID 0x{rev} in hardware_id_patterns (expected 0x01): {hwid}"
                        );
                    }
                    has_rev_01 = true;
                    search = &search[start + 2..];
                }
            }
            if !has_rev_01 {
                anyhow::bail!(
                    "windows-device-contract device {name} is missing a REV_01-qualified entry in hardware_id_patterns (AERO-W7-VIRTIO v1 requires PCI Revision ID 0x01)"
                );
            }
        }

        if device.driver_service_name.trim().is_empty() {
            anyhow::bail!("windows-device-contract device {name} has empty driver_service_name");
        }
        let inf_name = device.inf_name.trim();
        if inf_name.is_empty() {
            anyhow::bail!("windows-device-contract device {name} has empty inf_name");
        }
        if !inf_name.to_ascii_lowercase().ends_with(".inf") {
            anyhow::bail!(
                "windows-device-contract device {name} has inf_name that does not end with .inf: {inf_name}"
            );
        }

        if device.hardware_id_patterns.is_empty() {
            anyhow::bail!("windows-device-contract device {name} has no hardware_id_patterns");
        }
        for hwid in &device.hardware_id_patterns {
            let hwid = hwid.trim();
            if hwid.is_empty() {
                anyhow::bail!(
                    "windows-device-contract device {name} has empty hardware_id_patterns entry"
                );
            }
            if base_pci_hwid_re.find(hwid).is_none() {
                anyhow::bail!(
                    "windows-device-contract device {name} has invalid PCI hardware_id_patterns entry: {hwid}"
                );
            }
            if !hwid.to_ascii_uppercase().contains(&expected_substr) {
                anyhow::bail!(
                    "windows-device-contract device {name} hardware_id_patterns entry does not match pci_vendor_id/pci_device_id (expected to contain {expected_substr}): {hwid}"
                );
            }
        }
    }

    Ok(())
}

fn parse_hex_u16(raw: &str) -> Result<u16> {
    let s = raw.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if s.is_empty() {
        anyhow::bail!("expected hex value, got empty string");
    }
    if s.len() > 4 {
        anyhow::bail!("expected 16-bit hex value, got {raw:?}");
    }
    let value =
        u16::from_str_radix(s, 16).with_context(|| format!("expected hex value, got {raw:?}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_contract(json: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().expect("create tempfile");
        file.write_all(json.as_bytes()).expect("write contract");
        file.flush().expect("flush contract");
        file
    }

    #[test]
    fn contract_loader_rejects_unsupported_schema_version() {
        let file = write_contract(
            r#"{
  "schema_version": 999,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1042"],
      "driver_service_name": "aero_virtio_blk",
      "inf_name": "aero_virtio_blk.inf",
      "virtio_device_type": 2
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("schema_version"), "{err_str}");
    }

    #[test]
    fn contract_loader_rejects_hardware_id_mismatches() {
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1041&REV_01"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 2
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("VEN_1AF4&DEV_1042"), "{err_str}");
    }

    #[test]
    fn contract_loader_rejects_transitional_virtio_pci_device_ids() {
        // virtio-net is device type 1, so contract v1 expects PCI Device ID 0x1041 (modern-only).
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-net",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1000",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1000&REV_01"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 1
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("0x1040 + virtio_device_type"), "{err_str}");
    }

    #[test]
    fn contract_loader_rejects_virtio_hwid_lists_missing_rev_01() {
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1042"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 2
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("REV_01"), "{err_str}");
    }

    #[test]
    fn contract_loader_rejects_non_rev01_virtio_revision_ids() {
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1042&REV_00", "PCI\\VEN_1AF4&DEV_1042"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 2
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("revision ID"), "{err_str}");
        assert!(err_str.contains("0x00"), "{err_str}");
    }

    #[test]
    fn contract_loader_enforces_virtio_invariants_when_virtio_device_type_is_present() {
        // virtio validation is keyed off virtio_device_type (not the `device` name prefix).
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "not-a-virtio-name",
      "pci_vendor_id": "0x1234",
      "pci_device_id": "0x1041",
      "hardware_id_patterns": ["PCI\\VEN_1234&DEV_1041&REV_01"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 1
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("0x1AF4"), "{err_str}");
    }

    #[test]
    fn contract_loader_rejects_virtio_names_missing_virtio_device_type() {
        let file = write_contract(
            r#"{
  "schema_version": 1,
  "contract_name": "test",
  "contract_version": "0.0.0",
  "devices": [
    {
      "device": "virtio-blk",
      "pci_vendor_id": "0x1AF4",
      "pci_device_id": "0x1042",
      "hardware_id_patterns": ["PCI\\VEN_1AF4&DEV_1042&REV_01"],
      "driver_service_name": "svc",
      "inf_name": "x.inf"
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("missing virtio_device_type"), "{err_str}");
    }
}
