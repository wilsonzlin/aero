use crate::windows_device_contract::{load_windows_device_contract, WindowsDeviceContractDevice};
use anyhow::{bail, Result};
use std::collections::BTreeSet;
use std::path::Path;

fn hwids_from_patterns(patterns: &[String]) -> Result<Vec<String>> {
    let mut set = BTreeSet::new();
    for p in patterns {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        set.insert(p.to_ascii_uppercase());
    }

    let out: Vec<String> = set.into_iter().collect();
    if out.is_empty() {
        bail!("device has no hardware_id_patterns");
    }
    Ok(out)
}

fn quote_cmd_list(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

fn device_by_name<'a>(
    contract_path: &Path,
    devices: &'a [WindowsDeviceContractDevice],
    name: &str,
) -> Result<&'a WindowsDeviceContractDevice> {
    devices
        .iter()
        .find(|d| d.device.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow::anyhow!("{} is missing required device entry: {name}", contract_path.display()))
}

pub fn generate_guest_tools_devices_cmd_bytes(contract_path: &Path) -> Result<Vec<u8>> {
    let contract = load_windows_device_contract(contract_path)?;

    let virtio_blk = device_by_name(contract_path, &contract.devices, "virtio-blk")?;
    let virtio_net = device_by_name(contract_path, &contract.devices, "virtio-net")?;
    let virtio_snd = device_by_name(contract_path, &contract.devices, "virtio-snd")?;
    let virtio_input = device_by_name(contract_path, &contract.devices, "virtio-input")?;
    let aero_gpu = device_by_name(contract_path, &contract.devices, "aero-gpu")?;

    let virtio_blk_hwids = hwids_from_patterns(&virtio_blk.hardware_id_patterns)?;
    let virtio_net_hwids = hwids_from_patterns(&virtio_net.hardware_id_patterns)?;
    let virtio_snd_hwids = hwids_from_patterns(&virtio_snd.hardware_id_patterns)?;
    let virtio_input_hwids = hwids_from_patterns(&virtio_input.hardware_id_patterns)?;
    let aero_gpu_hwids = hwids_from_patterns(&aero_gpu.hardware_id_patterns)?;

    let stor_service = virtio_blk.driver_service_name.trim();
    if stor_service.is_empty() {
        bail!("virtio-blk entry has empty driver_service_name");
    }

    let net_service = virtio_net.driver_service_name.trim();
    let snd_service = virtio_snd.driver_service_name.trim();
    let input_service = virtio_input.driver_service_name.trim();
    let gpu_service = aero_gpu.driver_service_name.trim();

    let mut out = String::new();
    out.push_str("@echo off\r\n");
    out.push_str("rem This file is GENERATED from docs/windows-device-contract.json.\r\n");
    out.push_str("rem Do not edit by hand.\r\n");
    out.push_str("\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("rem Boot-critical storage (virtio-blk)\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("\r\n");
    out.push_str(&format!("set \"AERO_VIRTIO_BLK_SERVICE={stor_service}\"\r\n"));
    out.push_str("set \"AERO_VIRTIO_BLK_SYS=\"\r\n");
    out.push_str(&format!(
        "set AERO_VIRTIO_BLK_HWIDS={}\r\n",
        quote_cmd_list(&virtio_blk_hwids)
    ));
    out.push_str("\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("rem Network / input / sound\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("\r\n");
    out.push_str(&format!("set \"AERO_VIRTIO_NET_SERVICE={net_service}\"\r\n"));
    out.push_str(&format!(
        "set AERO_VIRTIO_NET_HWIDS={}\r\n",
        quote_cmd_list(&virtio_net_hwids)
    ));
    out.push_str(&format!("set \"AERO_VIRTIO_INPUT_SERVICE={input_service}\"\r\n"));
    out.push_str(&format!(
        "set AERO_VIRTIO_INPUT_HWIDS={}\r\n",
        quote_cmd_list(&virtio_input_hwids)
    ));
    out.push_str(&format!("set \"AERO_VIRTIO_SND_SERVICE={snd_service}\"\r\n"));
    out.push_str(&format!(
        "set AERO_VIRTIO_SND_HWIDS={}\r\n",
        quote_cmd_list(&virtio_snd_hwids)
    ));
    out.push_str("\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("rem Aero GPU\r\n");
    out.push_str("rem ---------------------------\r\n");
    out.push_str("\r\n");
    out.push_str(&format!("set \"AERO_GPU_SERVICE={gpu_service}\"\r\n"));
    out.push_str(&format!(
        "set AERO_GPU_HWIDS={}\r\n",
        quote_cmd_list(&aero_gpu_hwids)
    ));

    Ok(out.into_bytes())
}
