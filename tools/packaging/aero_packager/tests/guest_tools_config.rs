use std::fs;
use std::path::PathBuf;

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

#[test]
fn guest_tools_devices_cmd_is_generated_from_device_contract() -> anyhow::Result<()> {
    let packager_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = packager_root.join("..").join("..").join("..");

    let contract_path = repo_root
        .join("docs")
        .join("windows-device-contract.json");
    let devices_cmd_path = repo_root
        .join("guest-tools")
        .join("config")
        .join("devices.cmd");

    let generated = aero_packager::generate_guest_tools_devices_cmd_bytes(&contract_path)?;
    let generated = String::from_utf8(generated)?;
    let existing = fs::read_to_string(&devices_cmd_path)?;

    assert_eq!(
        normalize_newlines(&existing),
        normalize_newlines(&generated),
        "guest-tools/config/devices.cmd does not match generated output from {}",
        contract_path.display()
    );

    Ok(())
}

