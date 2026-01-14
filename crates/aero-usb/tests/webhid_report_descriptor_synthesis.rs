use aero_usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};

fn deserialize_collection(json: &str) -> HidCollectionInfo {
    serde_json::from_str::<HidCollectionInfo>(json).expect("collection JSON should deserialize")
}

#[test]
fn webhid_collection_type_deserializes_from_string_or_numeric() {
    let cases = [
        ("physical", 0x00u8),
        ("application", 0x01),
        ("logical", 0x02),
        ("report", 0x03),
        ("namedArray", 0x04),
        ("usageSwitch", 0x05),
        ("usageModifier", 0x06),
    ];

    for (ty, code) in cases {
        let string_field = deserialize_collection(&format!(
            r#"{{
              "usagePage": 1,
              "usage": 0,
              "type": "{ty}",
              "inputReports": [],
              "outputReports": [],
              "featureReports": [],
              "children": []
            }}"#,
        ));
        assert_eq!(string_field.collection_type.code(), code);

        let numeric_type = deserialize_collection(&format!(
            r#"{{
              "usagePage": 1,
              "usage": 0,
              "type": {code},
              "inputReports": [],
              "outputReports": [],
              "featureReports": [],
              "children": []
            }}"#,
        ));
        assert_eq!(numeric_type.collection_type.code(), code);

        let alias_collection_type = deserialize_collection(&format!(
            r#"{{
              "usagePage": 1,
              "usage": 0,
              "collectionType": {code},
              "inputReports": [],
              "outputReports": [],
              "featureReports": [],
              "children": []
            }}"#,
        ));
        assert_eq!(alias_collection_type.collection_type.code(), code);
    }
}

#[test]
fn synth_emits_correct_collection_item_payload() {
    let cases = [
        ("physical", 0x00u8),
        ("application", 0x01),
        ("logical", 0x02),
        ("report", 0x03),
        ("namedArray", 0x04),
        ("usageSwitch", 0x05),
        ("usageModifier", 0x06),
    ];

    for (ty, code) in cases {
        let collection = deserialize_collection(&format!(
            r#"{{
              "usagePage": 1,
              "usage": 0,
              "type": "{ty}",
              "inputReports": [],
              "outputReports": [],
              "featureReports": [],
              "children": []
            }}"#,
        ));

        let desc = synthesize_report_descriptor(&[collection])
            .unwrap_or_else(|err| panic!("synthesize_report_descriptor({ty}): {err}"));

        assert!(
            desc.windows(2).any(|w| w == [0xA1, code]),
            "expected Collection item (0xA1 {code:#04x}) in descriptor for {ty}: {desc:02x?}",
        );
    }
}

#[test]
fn webhid_collection_type_serializes_as_numeric_collection_type_field() {
    let collection = deserialize_collection(
        r#"{
          "usagePage": 1,
          "usage": 0,
          "type": "application",
          "inputReports": [],
          "outputReports": [],
          "featureReports": [],
          "children": []
        }"#,
    );

    let value = serde_json::to_value(&collection).expect("serialize collection");

    assert_eq!(value.get("collectionType"), Some(&serde_json::json!(1)));
    assert!(
        value.get("type").is_none(),
        "normalized JSON should not use the WebHID string enum field name: {value}"
    );
}

#[test]
fn webhid_report_item_accepts_wrap_alias() {
    let collection = deserialize_collection(
        r#"{
          "usagePage": 1,
          "usage": 0,
          "collectionType": 1,
          "children": [],
          "inputReports": [
            {
              "reportId": 0,
              "items": [
                {
                  "usagePage": 1,
                  "usages": [48],
                  "usageMinimum": 0,
                  "usageMaximum": 0,
                  "reportSize": 8,
                  "reportCount": 1,
                  "unitExponent": 0,
                  "unit": 0,
                  "logicalMinimum": 0,
                  "logicalMaximum": 1,
                  "physicalMinimum": 0,
                  "physicalMaximum": 0,
                  "strings": [],
                  "stringMinimum": 0,
                  "stringMaximum": 0,
                  "designators": [],
                  "designatorMinimum": 0,
                  "designatorMaximum": 0,
                  "isAbsolute": true,
                  "isArray": true,
                  "isBufferedBytes": false,
                  "isConstant": false,
                  "isLinear": true,
                  "isRange": false,
                  "isRelative": false,
                  "isVolatile": false,
                  "hasNull": false,
                  "hasPreferredState": true,
                  "wrap": true
                }
              ]
            }
          ],
          "outputReports": [],
          "featureReports": []
        }"#,
    );

    assert!(collection.input_reports[0].items[0].is_wrapped);
}

#[test]
fn webhid_report_item_deserializes_without_is_relative_field() {
    // Some WebHID typings omit `isRelative` (it can be derived from `isAbsolute`).
    // Keep the Rust schema permissive so hand-authored / legacy JSON still works.
    let collection = deserialize_collection(
        r#"{
          "usagePage": 1,
          "usage": 0,
          "collectionType": 1,
          "children": [],
          "inputReports": [
            {
              "reportId": 0,
              "items": [
                {
                  "usagePage": 1,
                  "usages": [48],
                  "usageMinimum": 0,
                  "usageMaximum": 0,
                  "reportSize": 8,
                  "reportCount": 1,
                  "unitExponent": 0,
                  "unit": 0,
                  "logicalMinimum": 0,
                  "logicalMaximum": 1,
                  "physicalMinimum": 0,
                  "physicalMaximum": 0,
                  "strings": [],
                  "stringMinimum": 0,
                  "stringMaximum": 0,
                  "designators": [],
                  "designatorMinimum": 0,
                  "designatorMaximum": 0,
                  "isAbsolute": false,
                  "isArray": true,
                  "isBufferedBytes": false,
                  "isConstant": false,
                  "isLinear": true,
                  "isRange": false,
                  "isVolatile": false,
                  "hasNull": false,
                  "hasPreferredState": true,
                  "isWrapped": false
                }
              ]
            }
          ],
          "outputReports": [],
          "featureReports": []
        }"#,
    );

    assert!(!collection.input_reports[0].items[0].is_absolute);
}
