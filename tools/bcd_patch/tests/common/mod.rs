use regf::{DataType, HiveBuilder, RegistryHive};
use uuid::Uuid;

pub const OBJ_GLOBALSETTINGS: &str = "{7ea2e1ac-2e61-4728-aaa3-896d9d0a9f0e}";
pub const OBJ_BOOTLOADERSETTINGS: &str = "{6efb52bf-1766-41db-a6b3-0ee5eff72bd7}";
pub const OBJ_BOOTMGR: &str = "{9dea862c-5cdd-4e70-acc1-f32b344d4795}";

pub const ELEM_APPLICATION_PATH: u32 = 0x1200_0002;
pub const ELEM_DISABLE_INTEGRITY_CHECKS: u32 = 0x1600_0048;
pub const ELEM_ALLOW_PRERELEASE_SIGNATURES: u32 = 0x1600_0049;
pub const ELEM_BOOTMGR_DISPLAY_ORDER: u32 = 0x2400_0001;
pub const ELEM_BOOTMGR_DEFAULT_OBJECT: u32 = 0x2300_0003;

pub fn element_key_name(element_type: u32) -> String {
    format!("{element_type:08X}")
}

pub fn encode_bcd_boolean(element_type: u32, value: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&element_type.to_le_bytes());
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(if value { 1u32 } else { 0u32 }).to_le_bytes());
    out
}

pub fn encode_bcd_string(element_type: u32, value: &str) -> Vec<u8> {
    let mut string_bytes = Vec::new();
    for ch in value.encode_utf16() {
        string_bytes.extend_from_slice(&ch.to_le_bytes());
    }
    string_bytes.extend_from_slice(&[0, 0]);

    let mut out = Vec::with_capacity(8 + string_bytes.len());
    out.extend_from_slice(&element_type.to_le_bytes());
    out.extend_from_slice(&(string_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&string_bytes);
    out
}

pub fn encode_bcd_guid(element_type: u32, guid: &Uuid) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 + 16);
    out.extend_from_slice(&element_type.to_le_bytes());
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&guid.to_bytes_le());
    out
}

pub fn encode_bcd_guid_list(element_type: u32, guids: &[Uuid]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 4 + 16 * guids.len());
    out.extend_from_slice(&element_type.to_le_bytes());
    out.extend_from_slice(&((16 * guids.len()) as u32).to_le_bytes());
    for guid in guids {
        out.extend_from_slice(&guid.to_bytes_le());
    }
    out
}

/// Build a synthetic, BCD-shaped REGF hive suitable for tests.
pub fn build_minimal_bcd_hive(already_patched: bool) -> Vec<u8> {
    let loader_obj = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();

    let mut builder = HiveBuilder::new_with_name("BCD00000000");
    let root = builder.root_offset();

    let objects = builder.add_key(root, "Objects").unwrap();

    // Boot manager object referencing loader object via DisplayOrder + Default.
    let bootmgr = builder.add_key(objects, OBJ_BOOTMGR).unwrap();
    let bootmgr_elements = builder.add_key(bootmgr, "Elements").unwrap();

    let display_order = builder
        .add_key(
            bootmgr_elements,
            &element_key_name(ELEM_BOOTMGR_DISPLAY_ORDER),
        )
        .unwrap();
    builder
        .add_value(
            display_order,
            "Element",
            DataType::Binary,
            &encode_bcd_guid_list(ELEM_BOOTMGR_DISPLAY_ORDER, &[loader_obj]),
        )
        .unwrap();

    let default_obj = builder
        .add_key(
            bootmgr_elements,
            &element_key_name(ELEM_BOOTMGR_DEFAULT_OBJECT),
        )
        .unwrap();
    builder
        .add_value(
            default_obj,
            "Element",
            DataType::Binary,
            &encode_bcd_guid(ELEM_BOOTMGR_DEFAULT_OBJECT, &loader_obj),
        )
        .unwrap();

    // OS loader object (ApplicationPath points to winload.exe)
    let loader = builder
        .add_key(objects, &format!("{{{}}}", loader_obj))
        .unwrap();
    let loader_elements = builder.add_key(loader, "Elements").unwrap();

    let app_path_key = builder
        .add_key(loader_elements, &element_key_name(ELEM_APPLICATION_PATH))
        .unwrap();
    builder
        .add_value(
            app_path_key,
            "Element",
            DataType::Binary,
            &encode_bcd_string(ELEM_APPLICATION_PATH, "\\Windows\\System32\\winload.exe"),
        )
        .unwrap();

    // Global settings / bootloader settings objects.
    for obj in [OBJ_GLOBALSETTINGS, OBJ_BOOTLOADERSETTINGS] {
        let k = builder.add_key(objects, obj).unwrap();
        let elements = builder.add_key(k, "Elements").unwrap();

        if already_patched {
            for (elem, val) in [
                (ELEM_DISABLE_INTEGRITY_CHECKS, true),
                (ELEM_ALLOW_PRERELEASE_SIGNATURES, true),
            ] {
                let ek = builder.add_key(elements, &element_key_name(elem)).unwrap();
                builder
                    .add_value(
                        ek,
                        "Element",
                        DataType::Binary,
                        &encode_bcd_boolean(elem, val),
                    )
                    .unwrap();
            }
        }
    }

    if already_patched {
        for (elem, val) in [
            (ELEM_DISABLE_INTEGRITY_CHECKS, true),
            (ELEM_ALLOW_PRERELEASE_SIGNATURES, true),
        ] {
            let ek = builder
                .add_key(loader_elements, &element_key_name(elem))
                .unwrap();
            builder
                .add_value(
                    ek,
                    "Element",
                    DataType::Binary,
                    &encode_bcd_boolean(elem, val),
                )
                .unwrap();
        }
    }

    builder.to_bytes().unwrap()
}

pub fn assert_boolean_element(hive: &RegistryHive, object: &str, elem: u32, expected: bool) {
    let path = format!("Objects\\{object}\\Elements\\{}", element_key_name(elem));
    let key = hive.open_key(&path).unwrap();
    let val = key.value("Element").unwrap();
    assert_eq!(val.data_type(), DataType::Binary);
    let data = val.raw_data().unwrap();
    assert!(
        data.len() >= 12,
        "expected boolean element data to include length prefix (>= 12 bytes), got {}",
        data.len()
    );
    assert_eq!(&data[0..4], &elem.to_le_bytes());
    assert_eq!(&data[4..8], &4u32.to_le_bytes());
    let got = u32::from_le_bytes(data[8..12].try_into().unwrap());
    assert_eq!(got != 0, expected);
}
