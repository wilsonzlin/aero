use std::fs;

#[test]
fn spec_rejects_unknown_top_level_fields() -> anyhow::Result<()> {
    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
            }
        ],
        "unknown_top_level_key": true,
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let err = aero_packager::PackagingSpec::load(&spec_path).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("unknown_top_level_key"),
        "error did not mention unknown key name: {msg}"
    );
    Ok(())
}

#[test]
fn spec_rejects_unknown_driver_fields() -> anyhow::Result<()> {
    let spec_dir = tempfile::tempdir()?;
    let spec_path = spec_dir.path().join("spec.json");
    let spec = serde_json::json!({
        "drivers": [
            {
                "name": "testdrv",
                "required": true,
                "expected_hardware_idz": [],
            }
        ],
    });
    fs::write(&spec_path, serde_json::to_vec_pretty(&spec)?)?;

    let err = aero_packager::PackagingSpec::load(&spec_path).unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("expected_hardware_idz"),
        "error did not mention unknown key name: {msg}"
    );
    Ok(())
}

