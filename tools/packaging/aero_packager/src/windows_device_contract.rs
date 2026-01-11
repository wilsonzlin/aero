use anyhow::{Context as _, Result};
use serde::Deserialize;
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
    Ok(contract)
}

