use std::fs;
use std::io::Write as _;

fn assert_driver_eq(a: &aero_packager::DriverSpec, b: &aero_packager::DriverSpec) {
    assert_eq!(a.name, b.name);
    assert_eq!(a.required, b.required);
    assert_eq!(a.expected_hardware_ids, b.expected_hardware_ids);
    assert_eq!(
        a.expected_hardware_ids_from_devices_cmd_var,
        b.expected_hardware_ids_from_devices_cmd_var
    );
    assert_eq!(a.allow_extensions, b.allow_extensions);
    assert_eq!(a.allow_path_regexes, b.allow_path_regexes);
}

#[test]
fn spec_loader_accepts_schema_field_without_changing_behavior() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;

    let base = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": false,
                "expected_hardware_ids": [],
                "allow_extensions": ["pdb"],
                "allow_path_regexes": [],
            }
        ],
        "required_drivers": [
            {
                "name": "TESTDRV",
                "expected_hardware_ids": [r"PCI\\VEN_1234&DEV_5678"],
            }
        ],
    });

    let spec_path_no_schema = dir.path().join("spec-no-schema.json");
    fs::write(
        &spec_path_no_schema,
        serde_json::to_vec_pretty(&base)?,
    )?;

    let with_schema = {
        let mut obj = base
            .as_object()
            .expect("spec json object")
            .clone();
        obj.insert(
            "$schema".to_string(),
            serde_json::Value::String("../packaging-spec.schema.json".to_string()),
        );
        serde_json::Value::Object(obj)
    };
    let spec_path_with_schema = dir.path().join("spec-with-schema.json");
    fs::write(
        &spec_path_with_schema,
        serde_json::to_vec_pretty(&with_schema)?,
    )?;

    let loaded_no_schema = aero_packager::PackagingSpec::load(&spec_path_no_schema)?;
    let loaded_with_schema = aero_packager::PackagingSpec::load(&spec_path_with_schema)?;

    assert_eq!(loaded_no_schema.drivers.len(), loaded_with_schema.drivers.len());
    for (a, b) in loaded_no_schema
        .drivers
        .iter()
        .zip(loaded_with_schema.drivers.iter())
    {
        assert_driver_eq(a, b);
    }

    Ok(())
}

#[test]
fn spec_loader_rejects_unknown_fields_under_strict_parsing() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;

    // Unknown top-level field should be rejected.
    let spec1 = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [],
            }
        ],
        "unknown_top_level_field": true,
    });
    let path1 = dir.path().join("unknown-top.json");
    fs::write(&path1, serde_json::to_vec_pretty(&spec1)?)?;
    let err = aero_packager::PackagingSpec::load(&path1).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown field") && msg.contains("unknown_top_level_field"),
        "unexpected error: {msg}"
    );

    // Unknown driver field should also be rejected.
    let spec2 = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_ids": [],
                "unknown_driver_field": 123,
            }
        ],
    });
    let path2 = dir.path().join("unknown-driver.json");
    let mut file = fs::File::create(&path2)?;
    // Write via std::io::Write so we can ensure the file handle is flushed before parsing on
    // platforms with aggressive buffering.
    file.write_all(&serde_json::to_vec_pretty(&spec2)?)?;
    file.flush()?;

    let err = aero_packager::PackagingSpec::load(&path2).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown field") && msg.contains("unknown_driver_field"),
        "unexpected error: {msg}"
    );

    Ok(())
}

