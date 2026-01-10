mod common;

use std::collections::BTreeMap;

use bcd_patch::{patch_bcd_store, PatchOpts};
use common::*;
use regf::hive::RegistryKey;
use regf::RegistryHive;
use tempfile::tempdir;

#[test]
fn patch_adds_missing_elements_and_is_idempotent() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("BCD");

    std::fs::write(&store, build_minimal_bcd_hive(false)).unwrap();

    patch_bcd_store(&store, PatchOpts::default()).unwrap();

    let hive = RegistryHive::from_file(&store).unwrap();
    assert_boolean_element(
        &hive,
        OBJ_GLOBALSETTINGS,
        ELEM_DISABLE_INTEGRITY_CHECKS,
        true,
    );
    assert_boolean_element(
        &hive,
        OBJ_GLOBALSETTINGS,
        ELEM_ALLOW_PRERELEASE_SIGNATURES,
        true,
    );
    assert_boolean_element(
        &hive,
        OBJ_BOOTLOADERSETTINGS,
        ELEM_DISABLE_INTEGRITY_CHECKS,
        true,
    );
    assert_boolean_element(
        &hive,
        OBJ_BOOTLOADERSETTINGS,
        ELEM_ALLOW_PRERELEASE_SIGNATURES,
        true,
    );

    let loader_obj = "{11111111-2222-3333-4444-555555555555}";
    assert_boolean_element(&hive, loader_obj, ELEM_DISABLE_INTEGRITY_CHECKS, true);
    assert_boolean_element(&hive, loader_obj, ELEM_ALLOW_PRERELEASE_SIGNATURES, true);

    // Idempotency: second run shouldn't produce different bytes.
    let after_first = std::fs::read(&store).unwrap();
    patch_bcd_store(&store, PatchOpts::default()).unwrap();
    let after_second = std::fs::read(&store).unwrap();
    assert_eq!(after_first, after_second);
}

fn snapshot_hive(hive: &RegistryHive) -> BTreeMap<String, BTreeMap<String, (u32, Vec<u8>)>> {
    fn rec(
        key: &RegistryKey<'_>,
        path: String,
        out: &mut BTreeMap<String, BTreeMap<String, (u32, Vec<u8>)>>,
    ) {
        let mut values = BTreeMap::new();
        for v in key.values().unwrap() {
            values.insert(
                v.name(),
                (v.raw_data_type(), v.raw_data().unwrap_or_default()),
            );
        }
        out.insert(path.clone(), values);

        for sk in key.subkeys().unwrap() {
            let child_path = if path.is_empty() {
                sk.name()
            } else if sk.name().is_empty() {
                path.clone()
            } else {
                format!("{path}\\{}", sk.name())
            };
            rec(&sk, child_path, out);
        }
    }

    let mut out = BTreeMap::new();
    let root = hive.root_key().unwrap();
    rec(&root, root.name(), &mut out);
    out
}

#[test]
fn roundtrip_preserves_tree_when_already_patched() {
    let dir = tempdir().unwrap();
    let store = dir.path().join("BCD");

    std::fs::write(&store, build_minimal_bcd_hive(true)).unwrap();

    let before = RegistryHive::from_file(&store).unwrap();
    let snap_before = snapshot_hive(&before);

    patch_bcd_store(&store, PatchOpts::default()).unwrap();

    let after = RegistryHive::from_file(&store).unwrap();
    let snap_after = snapshot_hive(&after);

    assert_eq!(snap_before, snap_after);
}
