use crate::wim::SigningMode;
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn patch_with_bcdedit(bcdedit: &Path, store: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    match mode {
        SigningMode::None => Ok(()),
        SigningMode::TestSigning => {
            run(
                Command::new(bcdedit)
                    .arg("/store")
                    .arg(store)
                    .arg("/set")
                    .arg("{default}")
                    .arg("testsigning")
                    .arg("on"),
                verbose,
            )?;
            Ok(())
        }
        SigningMode::NoIntegrityChecks => {
            run(
                Command::new(bcdedit)
                    .arg("/store")
                    .arg(store)
                    .arg("/set")
                    .arg("{default}")
                    .arg("nointegritychecks")
                    .arg("on"),
                verbose,
            )?;
            Ok(())
        }
    }
}

pub fn patch_with_hivex(hivexregedit: &Path, store: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    match mode {
        SigningMode::None => return Ok(()),
        _ => {}
    }

    let exported = run_capture(
        Command::new(hivexregedit).arg("--export").arg(store),
        verbose,
    )
    .context("Failed to export BCD hive via hivexregedit")?;

    let object_guids = parse_object_guids_from_export(&exported);
    if object_guids.is_empty() {
        return Err(anyhow!(
            "Failed to locate any BCD Objects GUIDs in hive export (unexpected BCD layout?)"
        ));
    }

    let reg_patch = render_bcd_reg_patch(&object_guids, mode);
    let patch_file = tempfile::Builder::new()
        .prefix("aero-win7-slipstream-bcd-")
        .suffix(".reg")
        .tempfile()
        .context("Failed to create temporary BCD patch file")?;
    std::fs::write(patch_file.path(), reg_patch).context("Failed to write temporary BCD patch file")?;

    run(
        Command::new(hivexregedit)
            .arg("--merge")
            .arg(store)
            .arg(patch_file.path()),
        verbose,
    )
    .context("Failed to merge BCD patch into hive")?;

    Ok(())
}

pub fn hive_contains_policy(exported_reg: &str, mode: SigningMode) -> bool {
    match mode {
        SigningMode::None => true,
        SigningMode::TestSigning => exported_reg.contains("\\Elements\\16000049]"),
        SigningMode::NoIntegrityChecks => exported_reg.contains("\\Elements\\16000048]"),
    }
}

fn parse_object_guids_from_export(exported_reg: &str) -> BTreeSet<String> {
    let mut guids = BTreeSet::new();
    for line in exported_reg.lines() {
        let line = line.trim();
        if !line.starts_with("[HKEY_LOCAL_MACHINE\\Objects\\{") {
            continue;
        }
        let rest = &line["[HKEY_LOCAL_MACHINE\\Objects\\{".len()..];
        if let Some(end) = rest.find('}') {
            let guid = &rest[..end];
            if !guid.is_empty() {
                guids.insert(guid.to_string());
            }
        }
    }
    guids
}

fn render_bcd_reg_patch(object_guids: &BTreeSet<String>, mode: SigningMode) -> String {
    let mut out = String::new();
    out.push_str("Windows Registry Editor Version 5.00\n\n");
    for guid in object_guids {
        match mode {
            SigningMode::TestSigning => {
                out.push_str(&render_bool_element(guid, 0x16000049, true));
            }
            SigningMode::NoIntegrityChecks => {
                out.push_str(&render_bool_element(guid, 0x16000048, true));
            }
            SigningMode::None => {}
        }
    }
    out
}

fn render_bool_element(object_guid: &str, element_type: u32, value: bool) -> String {
    // BCD hive layout stores element data as a REG_BINARY value named "Element", with the data
    // typically encoded as:
    //   [u32 element_type LE][u32 data_len LE][data...]
    // For booleans, data is a u32 (0 or 1).
    let data = if value { 1u32 } else { 0u32 };
    let mut blob = Vec::with_capacity(12);
    blob.extend_from_slice(&element_type.to_le_bytes());
    blob.extend_from_slice(&4u32.to_le_bytes());
    blob.extend_from_slice(&data.to_le_bytes());

    let bytes = crate::wim::format_reg_binary(&blob);
    format!(
        "[HKEY_LOCAL_MACHINE\\Objects\\{{{object_guid}}}\\Elements\\{element_type:08x}]\n\"Element\"=hex:{bytes}\n\n",
        object_guid = object_guid,
        element_type = element_type,
        bytes = bytes
    )
}

fn run(cmd: &mut Command, verbose: bool) -> Result<()> {
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let status = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to spawn external command")?;
    if !status.success() {
        return Err(anyhow!("External command failed with status: {status}"));
    }
    Ok(())
}

fn run_capture(cmd: &mut Command, verbose: bool) -> Result<String> {
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let out = cmd
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .context("Failed to spawn external command")?;
    if !out.status.success() {
        return Err(anyhow!("External command failed with status: {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
