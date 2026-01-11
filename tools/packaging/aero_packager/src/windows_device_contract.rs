use anyhow::{Context as _, Result};
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
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let contract: WindowsDeviceContract =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    validate_windows_device_contract(&contract)
        .with_context(|| format!("validate {}", path.display()))?;
    Ok(contract)
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

    for device in &contract.devices {
        let name = device.device.trim();
        if name.is_empty() {
            anyhow::bail!("windows-device-contract has a device entry with an empty device name");
        }
        let key = name.to_ascii_lowercase();
        if !seen_device_names.insert(key) {
            anyhow::bail!("windows-device-contract contains duplicate device entry: {name}");
        }

        let pci_vendor_id =
            parse_hex_u16(&device.pci_vendor_id).with_context(|| format!("{name}.pci_vendor_id"))?;
        let pci_device_id =
            parse_hex_u16(&device.pci_device_id).with_context(|| format!("{name}.pci_device_id"))?;
        let expected_substr = format!("VEN_{pci_vendor_id:04X}&DEV_{pci_device_id:04X}");

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
                anyhow::bail!("windows-device-contract device {name} has empty hardware_id_patterns entry");
            }
            if !hwid.to_ascii_uppercase().contains(&expected_substr) {
                anyhow::bail!(
                    "windows-device-contract device {name} hardware_id_patterns entry does not match pci_vendor_id/pci_device_id: {hwid} (expected to contain {expected_substr})"
                );
            }
        }

        if name.to_ascii_lowercase().starts_with("virtio-") && device.virtio_device_type.is_none() {
            anyhow::bail!(
                "windows-device-contract device {name} is missing virtio_device_type (required for virtio-* devices)"
            );
        }
    }

    Ok(())
}

fn parse_hex_u16(raw: &str) -> Result<u16> {
    let s = raw.trim();
    let s = s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if s.is_empty() {
        anyhow::bail!("expected hex value, got empty string");
    }
    if s.len() > 4 {
        anyhow::bail!("expected 16-bit hex value, got {raw:?}");
    }
    let value = u16::from_str_radix(s, 16)
        .with_context(|| format!("expected hex value, got {raw:?}"))?;
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
      "driver_service_name": "aerovblk",
      "inf_name": "aerovblk.inf",
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
      "pci_vendor_id": "0x1234",
      "pci_device_id": "0x5678",
      "hardware_id_patterns": ["PCI\\VEN_ABCD&DEV_EF01"],
      "driver_service_name": "svc",
      "inf_name": "x.inf",
      "virtio_device_type": 2
    }
  ]
}"#,
        );

        let err = load_windows_device_contract(file.path()).unwrap_err();
        let err_str = format!("{err:#}");
        assert!(err_str.contains("VEN_1234&DEV_5678"), "{err_str}");
    }
}
