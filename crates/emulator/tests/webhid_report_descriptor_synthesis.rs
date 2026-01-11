use emulator::io::usb::hid::webhid::{synthesize_report_descriptor, HidCollectionInfo};

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
