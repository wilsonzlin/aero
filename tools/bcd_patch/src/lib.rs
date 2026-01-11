//! Offline patching of Windows Boot Configuration Data (BCD) stores.
//!
//! Windows stores the BCD database as a REGF registry hive. This crate edits the hive directly,
//! making it possible to patch Windows 7 installation media/templates on non-Windows hosts
//! (Linux/macOS CI) without `bcdedit.exe`.

pub mod constants;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use regf::hive::RegistryKey;
use regf::{DataType, HiveBuilder, KeyTreeNode, KeyTreeValue, RegistryHive};
use uuid::Uuid;

use crate::constants::{
    ELEM_ALLOW_PRERELEASE_SIGNATURES, ELEM_APPLICATION_PATH, ELEM_BOOTMGR_DEFAULT_OBJECT,
    ELEM_BOOTMGR_DISPLAY_ORDER, ELEM_DISABLE_INTEGRITY_CHECKS, OBJ_BOOTLOADERSETTINGS, OBJ_BOOTMGR,
    OBJ_GLOBALSETTINGS, OBJ_RESUMELOADERSETTINGS,
};

pub(crate) const BCD_KEY_OBJECTS: &str = "Objects";
pub(crate) const BCD_KEY_ELEMENTS: &str = "Elements";
pub(crate) const BCD_VALUE_ELEMENT: &str = "Element";

/// Options controlling which BCD flags are enabled/disabled.
#[derive(Debug, Clone, Copy)]
pub struct PatchOpts {
    /// Enable/disable the `testsigning` BCD flag.
    pub testsigning: bool,
    /// Enable/disable the `nointegritychecks` BCD flag.
    pub nointegritychecks: bool,
}

impl Default for PatchOpts {
    fn default() -> Self {
        Self {
            testsigning: true,
            nointegritychecks: true,
        }
    }
}

/// Result of patching one store file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchFileResult {
    pub path: PathBuf,
    pub changed: bool,
}

/// Report for patching all relevant Win7 stores in an extracted tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Win7TreePatchReport {
    pub patched: Vec<PatchFileResult>,
    pub missing: Vec<String>,
}

/// Patch an offline BCD store (REGF hive) at `path`.
///
/// This function is intentionally cross-platform: it edits the offline hive directly instead of
/// calling Windows APIs.
pub fn patch_bcd_store(path: &Path, opts: PatchOpts) -> Result<()> {
    let _ = patch_bcd_store_inner(path, opts)?;
    Ok(())
}

fn patch_bcd_store_inner(path: &Path, opts: PatchOpts) -> Result<bool> {
    let original_bytes =
        fs::read(path).with_context(|| format!("read BCD store {}", path.display()))?;
    let hive = RegistryHive::from_bytes(original_bytes.clone())
        .map_err(|e| anyhow!("parse REGF hive {}: {e}", path.display()))?;

    let targets = select_target_objects(&hive)?;
    if targets.is_empty() {
        return Err(anyhow!(
            "no patch targets found in {}; is this a BCD store?",
            path.display()
        ));
    }

    let (major, minor) = hive.version();

    let mut tree = hive_to_tree(
        &hive
            .root_key()
            .map_err(|e| anyhow!("read hive root key: {e}"))?,
    )
    .context("convert hive to editable tree")?;

    for obj in &targets {
        patch_object(&mut tree, obj, opts)?;
    }

    sort_tree(&mut tree);

    let mut builder = HiveBuilder::from_tree_with_version(tree, major, minor);
    let mut new_bytes = builder
        .to_bytes()
        .with_context(|| format!("serialize patched hive for {}", path.display()))?;

    preserve_base_block(&original_bytes, &mut new_bytes)
        .context("preserve base block metadata for deterministic output")?;

    if new_bytes == original_bytes {
        return Ok(false);
    }

    write_atomic(path, &new_bytes)
        .with_context(|| format!("write patched hive to {}", path.display()))?;
    Ok(true)
}

/// Resolve a path under `root` case-insensitively (for case-sensitive host filesystems).
pub fn resolve_case_insensitive_path(root: &Path, segments: &[&str]) -> Result<Option<PathBuf>> {
    let mut current = root.to_path_buf();
    for (idx, seg) in segments.iter().enumerate() {
        if !current.is_dir() {
            return Ok(None);
        }

        let mut matches = Vec::new();
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if file_name.eq_ignore_ascii_case(seg) {
                matches.push(entry.path());
            }
        }

        match matches.len() {
            0 => return Ok(None),
            1 => current = matches.remove(0),
            _ => {
                let display_root = if idx == 0 { root } else { &current };
                return Err(anyhow!(
                    "ambiguous case-insensitive match for path segment {seg:?} under {}",
                    display_root.display()
                ));
            }
        }
    }

    Ok(Some(current))
}

/// Patch all relevant Win7 BCD stores in an extracted tree.
///
/// This is a convenience wrapper around [`patch_bcd_store`] that looks for the standard Win7
/// store locations:
/// - `boot/BCD`
/// - `efi/microsoft/boot/BCD`
/// - `Windows/System32/Config/BCD-Template`
pub fn patch_win7_tree(root: &Path, opts: PatchOpts, strict: bool) -> Result<Win7TreePatchReport> {
    if !root.is_dir() {
        return Err(anyhow!("root is not a directory: {}", root.display()));
    }

    let targets: [(&str, &[&str]); 3] = [
        ("boot/BCD", &["boot", "BCD"]),
        (
            "efi/microsoft/boot/BCD",
            &["efi", "microsoft", "boot", "BCD"],
        ),
        (
            "Windows/System32/Config/BCD-Template",
            &["Windows", "System32", "Config", "BCD-Template"],
        ),
    ];

    let mut missing = Vec::new();
    let mut resolved = Vec::new();
    for (label, segments) in targets {
        match resolve_case_insensitive_path(root, segments)? {
            Some(path) => resolved.push((label.to_string(), path)),
            None => missing.push(label.to_string()),
        }
    }

    if strict && !missing.is_empty() {
        return Err(anyhow!(
            "missing {} required BCD store(s): {}",
            missing.len(),
            missing.join(", ")
        ));
    }

    let mut patched = Vec::new();
    for (_label, path) in resolved {
        let changed = patch_bcd_store_inner(&path, opts)?;
        patched.push(PatchFileResult { path, changed });
    }

    Ok(Win7TreePatchReport { patched, missing })
}

fn preserve_base_block(original: &[u8], out: &mut [u8]) -> Result<()> {
    const BASE_BLOCK_SIZE: usize = 4096;

    if original.len() < BASE_BLOCK_SIZE || out.len() < BASE_BLOCK_SIZE {
        return Err(anyhow!(
            "REGF hive too small (original {} bytes, out {} bytes)",
            original.len(),
            out.len()
        ));
    }

    // Keep the layout-dependent fields from the regenerated hive.
    // Offsets based on regf base block structure:
    // - root_cell_offset @ 0x24 (36)
    // - hive_bins_data_size @ 0x28 (40)
    let root_cell_offset = out[36..40].to_vec();
    let hive_bins_data_size = out[40..44].to_vec();

    // Copy the base block from the original hive. This preserves sequence numbers and timestamps,
    // avoiding "rewrite noise" (and making the patch operation deterministic across runs).
    out[..BASE_BLOCK_SIZE].copy_from_slice(&original[..BASE_BLOCK_SIZE]);

    // Restore layout-dependent fields.
    out[36..40].copy_from_slice(&root_cell_offset);
    out[40..44].copy_from_slice(&hive_bins_data_size);

    // Recompute and update checksum at 0x1FC (508) over the first 508 bytes.
    let checksum = calculate_regf_checksum(&out[..512]);
    out[508..512].copy_from_slice(&checksum.to_le_bytes());

    Ok(())
}

fn calculate_regf_checksum(header: &[u8]) -> u32 {
    assert!(header.len() >= 512);

    let mut checksum: u32 = 0;
    for chunk in header[..508].chunks_exact(4) {
        let value = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        checksum ^= value;
    }

    if checksum == 0xFFFF_FFFF {
        0xFFFF_FFFE
    } else if checksum == 0 {
        1
    } else {
        checksum
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("BCD");
    let tmp_name = format!(".{file_name}.bcd_patch.tmp");
    let tmp_path = parent.join(tmp_name);

    fs::write(&tmp_path, data)
        .with_context(|| format!("write temp file {}", tmp_path.display()))?;

    // `rename` doesn't replace on Windows.
    #[cfg(windows)]
    {
        let _ = fs::remove_file(path);
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "rename temp file {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

fn select_target_objects(hive: &RegistryHive) -> Result<HashSet<String>> {
    let objects_key = hive
        .open_key(BCD_KEY_OBJECTS)
        .map_err(|e| anyhow!("open key '{}': {e}", BCD_KEY_OBJECTS))?;

    let object_names = objects_key
        .subkeys()
        .map_err(|e| anyhow!("enumerate BCD objects: {e}"))?;

    let mut by_upper: HashMap<String, String> = HashMap::new();
    for obj in &object_names {
        by_upper.insert(obj.name().to_uppercase(), obj.name());
    }

    let mut targets: HashSet<String> = HashSet::new();

    for known in [
        OBJ_GLOBALSETTINGS,
        OBJ_BOOTLOADERSETTINGS,
        OBJ_RESUMELOADERSETTINGS,
    ] {
        if let Some(actual) = by_upper.get(&known.to_uppercase()) {
            targets.insert(actual.clone());
        }
    }

    // Patch all loader objects (winload/winresume) by ApplicationPath element.
    for obj in &object_names {
        if let Some(app_path) = read_bcd_string_element(hive, &obj.name(), ELEM_APPLICATION_PATH)? {
            if is_win_loader_path(&app_path) {
                targets.insert(obj.name());
            }
        }
    }

    // Fallback/extra coverage: objects referenced by bootmgr display order + default entry.
    if let Some(bootmgr) = by_upper.get(&OBJ_BOOTMGR.to_uppercase()) {
        if let Some(default_obj) =
            read_bcd_guid_element(hive, bootmgr, ELEM_BOOTMGR_DEFAULT_OBJECT)?
        {
            if let Some(actual) = by_upper.get(&format_guid_key(&default_obj).to_uppercase()) {
                targets.insert(actual.clone());
            }
        }

        if let Some(order) = read_bcd_guid_list_element(hive, bootmgr, ELEM_BOOTMGR_DISPLAY_ORDER)?
        {
            for guid in order {
                if let Some(actual) = by_upper.get(&format_guid_key(&guid).to_uppercase()) {
                    targets.insert(actual.clone());
                }
            }
        }
    }

    Ok(targets)
}

fn patch_object(tree: &mut KeyTreeNode, object_name: &str, opts: PatchOpts) -> Result<()> {
    let base = format!("{BCD_KEY_OBJECTS}\\{object_name}\\{BCD_KEY_ELEMENTS}");

    let nointegrity_key = format!(
        "{base}\\{}",
        element_key_name(ELEM_DISABLE_INTEGRITY_CHECKS)
    );
    let testsigning_key = format!(
        "{base}\\{}",
        element_key_name(ELEM_ALLOW_PRERELEASE_SIGNATURES)
    );

    set_binary_value(
        tree,
        &nointegrity_key,
        BCD_VALUE_ELEMENT,
        &bcd_encode_boolean(ELEM_DISABLE_INTEGRITY_CHECKS, opts.nointegritychecks),
    )?;

    set_binary_value(
        tree,
        &testsigning_key,
        BCD_VALUE_ELEMENT,
        &bcd_encode_boolean(ELEM_ALLOW_PRERELEASE_SIGNATURES, opts.testsigning),
    )?;

    Ok(())
}

fn set_binary_value(tree: &mut KeyTreeNode, key_path: &str, name: &str, data: &[u8]) -> Result<()> {
    let node = tree_get_or_create_path(tree, key_path);
    upsert_value(node, name, DataType::Binary, data.to_vec());
    Ok(())
}

fn upsert_value(node: &mut KeyTreeNode, name: &str, data_type: DataType, data: Vec<u8>) {
    if let Some(existing) = node
        .values
        .iter_mut()
        .find(|v| v.name.eq_ignore_ascii_case(name))
    {
        existing.data_type = data_type;
        existing.data = data;
        return;
    }

    node.values.push(KeyTreeValue {
        name: name.to_string(),
        data_type,
        data,
    });
}

fn tree_get_or_create_path<'a>(root: &'a mut KeyTreeNode, path: &str) -> &'a mut KeyTreeNode {
    if path.is_empty() {
        return root;
    }

    let parts = path.split('\\').filter(|p| !p.is_empty());
    let mut cur = root;
    for part in parts {
        let idx = cur
            .children
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(part));
        if let Some(idx) = idx {
            cur = &mut cur.children[idx];
        } else {
            cur.children.push(KeyTreeNode::new(part));
            let len = cur.children.len();
            cur = &mut cur.children[len - 1];
        }
    }
    cur
}

fn sort_tree(node: &mut KeyTreeNode) {
    node.children
        .sort_by(|a, b| a.name.to_uppercase().cmp(&b.name.to_uppercase()));
    node.values
        .sort_by(|a, b| a.name.to_uppercase().cmp(&b.name.to_uppercase()));
    for child in &mut node.children {
        sort_tree(child);
    }
}

fn hive_to_tree(key: &RegistryKey<'_>) -> Result<KeyTreeNode> {
    let mut node = KeyTreeNode::new(&key.name());

    let mut values = key.values().map_err(|e| anyhow!("read values: {e}"))?;
    // Ensure stable output bytes: sort values by name before storing.
    values.sort_by(|a, b| a.name().to_uppercase().cmp(&b.name().to_uppercase()));

    for value in values {
        node.values.push(KeyTreeValue {
            name: value.name(),
            data_type: value.data_type(),
            data: value
                .raw_data()
                .map_err(|e| anyhow!("read value data: {e}"))?,
        });
    }

    let mut subkeys = key.subkeys().map_err(|e| anyhow!("read subkeys: {e}"))?;
    subkeys.sort_by(|a, b| a.name().to_uppercase().cmp(&b.name().to_uppercase()));

    for subkey in subkeys {
        node.children.push(hive_to_tree(&subkey)?);
    }

    Ok(node)
}

fn element_key_name(element_type: u32) -> String {
    format!("{element_type:08X}")
}

fn is_win_loader_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("winload") || p.contains("winresume")
}

fn read_bcd_element_value(
    hive: &RegistryHive,
    object_name: &str,
    element_type: u32,
) -> Result<Option<Vec<u8>>> {
    let path = format!(
        "{BCD_KEY_OBJECTS}\\{object_name}\\{BCD_KEY_ELEMENTS}\\{}",
        element_key_name(element_type)
    );

    let key = match hive.open_key(&path) {
        Ok(k) => k,
        Err(regf::Error::KeyNotFound(_)) => return Ok(None),
        Err(e) => return Err(anyhow!("open key '{path}': {e}")),
    };

    let value = match key.value(BCD_VALUE_ELEMENT) {
        Ok(v) => v,
        Err(regf::Error::ValueNotFound(_)) => return Ok(None),
        Err(e) => return Err(anyhow!("read value '{path}\\{BCD_VALUE_ELEMENT}': {e}")),
    };

    if value.data_type() != DataType::Binary {
        return Ok(None);
    }

    Ok(Some(value.raw_data().map_err(|e| {
        anyhow!("read binary value '{path}\\{BCD_VALUE_ELEMENT}': {e}")
    })?))
}

fn read_bcd_string_element(
    hive: &RegistryHive,
    object_name: &str,
    element_type: u32,
) -> Result<Option<String>> {
    let Some(bytes) = read_bcd_element_value(hive, object_name, element_type)? else {
        return Ok(None);
    };
    Ok(bcd_decode_string(&bytes, element_type))
}

fn read_bcd_guid_element(
    hive: &RegistryHive,
    object_name: &str,
    element_type: u32,
) -> Result<Option<Uuid>> {
    let Some(bytes) = read_bcd_element_value(hive, object_name, element_type)? else {
        return Ok(None);
    };
    Ok(bcd_decode_guid(&bytes, element_type))
}

fn read_bcd_guid_list_element(
    hive: &RegistryHive,
    object_name: &str,
    element_type: u32,
) -> Result<Option<Vec<Uuid>>> {
    let Some(bytes) = read_bcd_element_value(hive, object_name, element_type)? else {
        return Ok(None);
    };
    Ok(bcd_decode_guid_list(&bytes, element_type))
}

fn format_guid_key(guid: &Uuid) -> String {
    format!("{{{guid}}}")
}

fn bcd_encode_boolean(element_type: u32, value: bool) -> Vec<u8> {
    // BCD elements are stored as a REG_BINARY value named "Element", typically encoded as:
    //   [u32 element_type LE][u32 data_len LE][data...]
    //
    // For booleans, the data is a u32 (0 or 1) and the length is 4 bytes.
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&element_type.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(if value { 1u32 } else { 0u32 }).to_le_bytes());
    out
}

fn bcd_decode_string(bytes: &[u8], expected_type: u32) -> Option<String> {
    let (ty, payload) = bcd_payload(bytes)?;
    if ty != expected_type {
        return None;
    }

    // Many BCD element encodings include a u32 data length prefix (bytes). Be forgiving and
    // accept a couple of common layouts.
    let string_bytes = decode_len_prefixed_bytes(payload).unwrap_or(payload);

    decode_utf16le_nul_terminated(string_bytes)
}

fn bcd_decode_guid(bytes: &[u8], expected_type: u32) -> Option<Uuid> {
    let (ty, payload) = bcd_payload(bytes)?;
    if ty != expected_type {
        return None;
    }

    // Common layout: [u32 len=16][guid bytes]
    if let Some(bytes) = decode_len_prefixed_bytes(payload) {
        if bytes.len() >= 16 {
            let raw: [u8; 16] = bytes[0..16].try_into().ok()?;
            return Some(Uuid::from_bytes_le(raw));
        }
    }

    if payload.len() >= 16 {
        let raw: [u8; 16] = payload[0..16].try_into().ok()?;
        return Some(Uuid::from_bytes_le(raw));
    }
    None
}

fn bcd_decode_guid_list(bytes: &[u8], expected_type: u32) -> Option<Vec<Uuid>> {
    let (ty, payload) = bcd_payload(bytes)?;
    if ty != expected_type {
        return None;
    }

    let list_bytes = if let Some(bytes) = decode_len_prefixed_bytes(payload) {
        bytes
    } else if payload.len() % 16 == 0 {
        payload
    } else if payload.len() >= 4 && (payload.len() - 4) % 16 == 0 {
        // Some layouts use a u32 prefix other than length (e.g. count). Treat it like length for
        // decoding purposes.
        &payload[4..]
    } else if payload.len() >= 8 && (payload.len() - 8) % 16 == 0 {
        &payload[8..]
    } else {
        return None;
    };

    let mut out = Vec::new();
    for chunk in list_bytes.chunks_exact(16) {
        let raw: [u8; 16] = chunk.try_into().ok()?;
        out.push(Uuid::from_bytes_le(raw));
    }
    Some(out)
}

fn bcd_payload(bytes: &[u8]) -> Option<(u32, &[u8])> {
    if bytes.len() < 4 {
        return None;
    }
    let ty = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    Some((ty, &bytes[4..]))
}

fn decode_len_prefixed_bytes(payload: &[u8]) -> Option<&[u8]> {
    // Try: [u32 byte_len][data...]
    for start in [0usize, 4] {
        if payload.len() < start + 4 {
            continue;
        }
        let n = u32::from_le_bytes(payload[start..start + 4].try_into().ok()?) as usize;
        if n == 0 {
            continue;
        }

        // Treat n as byte length.
        if start + 4 + n <= payload.len() {
            let end = start + 4 + n;
            // Avoid mis-detecting random UTF-16 string data as a length prefix by requiring the
            // next UTF-16 code unit boundary to either be the end of the payload or begin with a
            // NUL terminator.
            if end == payload.len() || payload[end..].starts_with(&[0, 0]) {
                return Some(&payload[start + 4..end]);
            }
        }

        // Treat n as count of UTF-16 code units.
        if start + 4 + n * 2 <= payload.len() {
            let end = start + 4 + n * 2;
            if end == payload.len() || payload[end..].starts_with(&[0, 0]) {
                return Some(&payload[start + 4..end]);
            }
        }
    }
    None
}

fn decode_utf16le_nul_terminated(bytes: &[u8]) -> Option<String> {
    let mut u16s = Vec::new();
    for chunk in bytes.chunks_exact(2) {
        let val = u16::from_le_bytes(chunk.try_into().ok()?);
        if val == 0 {
            break;
        }
        u16s.push(val);
    }
    String::from_utf16(&u16s).ok()
}
