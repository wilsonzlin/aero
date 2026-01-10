use crate::wim::SigningMode;
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const BCD_BOOTMGR_GUID: &str = "9dea862c-5cdd-4e70-acc1-f32b344d4795";
const BCD_GLOBALSETTINGS_GUID: &str = "7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e";
const BCD_BOOTLOADERSETTINGS_GUID: &str = "6efb52bf-1766-41db-a6b3-0ee5eff72bd7";

const ELEMENT_TESTSIGNING: &str = "16000049";
const ELEMENT_NOINTEGRITYCHECKS: &str = "16000048";
const ELEMENT_BOOTMGR_DEFAULT_OBJECT: &str = "23000003";

/// Windows-native BCD patching without `bcdedit`.
///
/// Rationale: some offline BCD stores (notably `BCD-Template`) do not always expose the `{default}`
/// alias in ways that `bcdedit /set {default} ...` can mutate reliably. Patching the hive
/// directly (via `reg load` + `.reg` import) matches the approach used by the cross-platform
/// backend (`hivexregedit`).
pub fn patch_with_reg(reg: &Path, store: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    let element_hex = match mode {
        SigningMode::None => return Ok(()),
        SigningMode::TestSigning => ELEMENT_TESTSIGNING,
        SigningMode::NoIntegrityChecks => ELEMENT_NOINTEGRITYCHECKS,
    };

    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mount = format!("HKLM\\AERO_BCD_{pid}_{nanos}");

    run(Command::new(reg).arg("load").arg(&mount).arg(store), verbose)
        .context("reg load (BCD hive) failed")?;

    struct UnloadGuard<'a> {
        reg: &'a Path,
        mount: String,
        verbose: bool,
    }
    impl Drop for UnloadGuard<'_> {
        fn drop(&mut self) {
            let _ = run(
                Command::new(self.reg).arg("unload").arg(&self.mount),
                self.verbose,
            );
        }
    }
    let _guard = UnloadGuard {
        reg,
        mount: mount.clone(),
        verbose,
    };

    let default_obj = query_default_loader_object(reg, &mount, verbose)
        .unwrap_or(None);

    let mut object_guids = vec![
        BCD_GLOBALSETTINGS_GUID.to_string(),
        BCD_BOOTLOADERSETTINGS_GUID.to_string(),
    ];
    if let Some(guid) = default_obj {
        object_guids.push(guid);
    }
    object_guids.sort();
    object_guids.dedup();

    let root_prefix = format!("HKEY_LOCAL_MACHINE\\{}", mount.trim_start_matches("HKLM\\"));
    let reg_patch = render_bcd_boolean_patch(&root_prefix, &object_guids, element_hex);
    let patch_file = tempfile::Builder::new()
        .prefix("aero-win7-slipstream-bcd-")
        .suffix(".reg")
        .tempfile()
        .context("Failed to create temporary BCD patch file")?;
    std::fs::write(patch_file.path(), reg_patch)
        .context("Failed to write temporary BCD patch file")?;

    run(Command::new(reg).arg("import").arg(patch_file.path()), verbose)
        .context("reg import (BCD patch) failed")?;
    Ok(())
}

pub fn patch_with_hivex(hivexregedit: &Path, store: &Path, mode: SigningMode, verbose: bool) -> Result<()> {
    match mode {
        SigningMode::None => return Ok(()),
        _ => {}
    }

    let exported =
        run_capture(Command::new(hivexregedit).arg("--export").arg(store), verbose)
            .context("Failed to export BCD hive via hivexregedit")?;

    // Prefer patching well-known library objects ({globalsettings} + {bootloadersettings}) since
    // Win7 loader entries commonly inherit settings from them. Additionally patch the store's
    // default loader entry when it can be resolved from the boot manager.
    let mut object_guids = vec![
        BCD_GLOBALSETTINGS_GUID.to_string(),
        BCD_BOOTLOADERSETTINGS_GUID.to_string(),
    ];

    if let Some(default_obj) =
        parse_guid_element_from_export(&exported, BCD_BOOTMGR_GUID, ELEMENT_BOOTMGR_DEFAULT_OBJECT)
    {
        object_guids.push(default_obj);
    }

    object_guids.sort();
    object_guids.dedup();

    let element = match mode {
        SigningMode::TestSigning => ELEMENT_TESTSIGNING,
        SigningMode::NoIntegrityChecks => ELEMENT_NOINTEGRITYCHECKS,
        SigningMode::None => unreachable!(),
    };

    let reg_patch = render_bcd_boolean_patch("HKEY_LOCAL_MACHINE", &object_guids, element);
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
        SigningMode::TestSigning => export_has_enabled_boolean(exported_reg, ELEMENT_TESTSIGNING),
        SigningMode::NoIntegrityChecks => {
            export_has_enabled_boolean(exported_reg, ELEMENT_NOINTEGRITYCHECKS)
        }
    }
}

fn render_bcd_boolean_patch(root_prefix: &str, object_guids: &[String], element_hex: &str) -> String {
    let mut out = String::new();
    out.push_str("Windows Registry Editor Version 5.00\n\n");
    for guid in object_guids {
        out.push_str(&render_bool_element(root_prefix, guid, element_hex, true));
    }
    out
}

fn render_bool_element(root_prefix: &str, object_guid: &str, element_hex: &str, value: bool) -> String {
    // For Win7 BCD hives, OS loader booleans are stored as REG_BINARY "Element" values containing
    // a 32-bit little-endian integer (0 or 1).
    let data = if value { [1u8, 0, 0, 0] } else { [0u8, 0, 0, 0] };
    let bytes = crate::wim::format_reg_binary(&data);
    format!(
        "[{root_prefix}\\Objects\\{{{object_guid}}}\\Elements\\{element_hex}]\n\"Element\"=hex:{bytes}\n\n",
        root_prefix = root_prefix,
        object_guid = object_guid,
        element_hex = element_hex,
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

fn export_has_enabled_boolean(exported_reg: &str, element_hex: &str) -> bool {
    let target_suffix = format!("\\Elements\\{element_hex}]");
    let mut in_target = false;
    let mut lines = exported_reg.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_target = trimmed.ends_with(&target_suffix);
            continue;
        }
        if !in_target {
            continue;
        }
        if let Some(bytes) = parse_reg_binary_value(trimmed, &mut lines) {
            return is_enabled_bool_bytes(&bytes);
        }
    }
    false
}

fn is_enabled_bool_bytes(bytes: &[u8]) -> bool {
    match bytes.len() {
        0 => false,
        1 => bytes[0] != 0,
        _ => {
            if bytes.len() >= 4 {
                u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != 0
            } else {
                bytes.iter().any(|b| *b != 0)
            }
        }
    }
}

fn parse_guid_element_from_export(exported_reg: &str, object_guid: &str, element_hex: &str) -> Option<String> {
    let header = format!(
        "[HKEY_LOCAL_MACHINE\\Objects\\{{{object_guid}}}\\Elements\\{element_hex}]",
        object_guid = object_guid,
        element_hex = element_hex
    );
    let mut in_target = false;
    let mut lines = exported_reg.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_target = trimmed == header;
            continue;
        }
        if !in_target {
            continue;
        }
        if let Some(bytes) = parse_reg_binary_value(trimmed, &mut lines) {
            if let Some(guid) = guid_from_le_bytes(&bytes) {
                return Some(guid);
            }
        }
    }
    None
}

fn guid_from_le_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 16 {
        return None;
    }
    let d1 = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let d2 = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    let d3 = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
    let d4 = &bytes[8..16];
    Some(format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d1, d2, d3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    ))
}

fn parse_reg_binary_value<'a, I>(first_line: &str, rest: &mut std::iter::Peekable<I>) -> Option<Vec<u8>>
where
    I: Iterator<Item = &'a str>,
{
    // Look for `"Element"=hex:...` or `"Element"=hex(<type>):...`.
    let line = first_line.trim();
    if !line.starts_with("\"Element\"=") {
        return None;
    }

    let mut value = line.splitn(2, '=').nth(1)?.trim().to_string();
    while value.ends_with('\\') {
        value.pop();
        let next = rest.next()?.trim();
        value.push_str(next);
    }

    if let Some(rest) = value.strip_prefix("dword:") {
        let v = u32::from_str_radix(rest.trim(), 16).ok()?;
        return Some(v.to_le_bytes().to_vec());
    }

    let hex_payload = if let Some(rest) = value.strip_prefix("hex:") {
        rest
    } else if let Some(idx) = value.find("):") {
        // hex(<type>):...
        &value[(idx + 2)..]
    } else {
        return None;
    };

    let mut bytes = Vec::new();
    for part in hex_payload.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let b = u8::from_str_radix(part, 16).ok()?;
        bytes.push(b);
    }
    Some(bytes)
}

fn query_default_loader_object(reg: &Path, mount: &str, verbose: bool) -> Result<Option<String>> {
    let key = format!(
        "{mount}\\Objects\\{{{bootmgr}}}\\Elements\\{elem}",
        mount = mount,
        bootmgr = BCD_BOOTMGR_GUID,
        elem = ELEMENT_BOOTMGR_DEFAULT_OBJECT
    );

    let mut cmd = Command::new(reg);
    cmd.arg("query").arg(&key).arg("/v").arg("Element");
    if verbose {
        eprintln!("> {:?}", cmd);
    }
    let output = cmd
        .stdin(Stdio::null())
        .output()
        .context("Failed to spawn reg query for BCD default loader object")?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bytes = parse_reg_query_binary_value(&stdout)
        .ok_or_else(|| anyhow!("reg query succeeded but did not contain a parseable Element REG_BINARY value"))?;
    Ok(guid_from_le_bytes(&bytes))
}

fn parse_reg_query_binary_value(output: &str) -> Option<Vec<u8>> {
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        if !name.eq_ignore_ascii_case("Element") {
            continue;
        }
        let ty = parts.next()?;
        if !ty.eq_ignore_ascii_case("REG_BINARY") {
            continue;
        }

        let mut bytes = Vec::new();
        for p in parts {
            if p.len() != 2 {
                continue;
            }
            bytes.push(u8::from_str_radix(p, 16).ok()?);
        }
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_enabled_boolean_in_export() {
        let export = r#"Windows Registry Editor Version 5.00

[HKEY_LOCAL_MACHINE\Objects\{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}\Elements\16000049]
"Element"=hex:01,00,00,00
"#;
        assert!(hive_contains_policy(export, SigningMode::TestSigning));
        assert!(!hive_contains_policy(export, SigningMode::NoIntegrityChecks));
    }

    #[test]
    fn parses_guid_element() {
        // {01234567-89ab-cdef-0123-456789abcdef}
        let guid_bytes = [
            0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67, 0x89,
            0xab, 0xcd, 0xef,
        ];
        let guid_hex = crate::wim::format_reg_binary(&guid_bytes);
        let export = format!(
            "Windows Registry Editor Version 5.00\n\n[HKEY_LOCAL_MACHINE\\Objects\\{{{}}}\\Elements\\{}]\n\"Element\"=hex:{}\n",
            BCD_BOOTMGR_GUID, ELEMENT_BOOTMGR_DEFAULT_OBJECT, guid_hex
        );
        let parsed =
            parse_guid_element_from_export(&export, BCD_BOOTMGR_GUID, ELEMENT_BOOTMGR_DEFAULT_OBJECT)
                .unwrap();
        assert_eq!(parsed, "01234567-89ab-cdef-0123-456789abcdef");
    }

    #[test]
    fn parses_reg_query_binary_value() {
        let out = r#"HKEY_LOCAL_MACHINE\AERO_BCD\Objects\{9dea862c-5cdd-4e70-acc1-f32b344d4795}\Elements\23000003
    Element    REG_BINARY    67 45 23 01 AB 89 EF CD 01 23 45 67 89 AB CD EF
"#;
        let bytes = parse_reg_query_binary_value(out).unwrap();
        assert_eq!(
            bytes,
            vec![
                0x67, 0x45, 0x23, 0x01, 0xab, 0x89, 0xef, 0xcd, 0x01, 0x23, 0x45, 0x67,
                0x89, 0xab, 0xcd, 0xef
            ]
        );
        assert_eq!(
            guid_from_le_bytes(&bytes).unwrap(),
            "01234567-89ab-cdef-0123-456789abcdef"
        );
    }
}
