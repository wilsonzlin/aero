use std::fs;
use std::path::{Path, PathBuf};

fn repo_specs_dir() -> PathBuf {
    // tools/packaging/aero_packager -> tools/packaging/specs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("specs")
}

#[test]
fn in_repo_packaging_specs_have_schema_and_parse() -> anyhow::Result<()> {
    let specs_dir = repo_specs_dir();
    assert!(
        specs_dir.is_dir(),
        "expected specs directory to exist: {}",
        specs_dir.display()
    );

    let mut json_paths: Vec<PathBuf> = fs::read_dir(&specs_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
        .collect();
    json_paths.sort();
    assert!(
        !json_paths.is_empty(),
        "expected at least one spec JSON file under {}",
        specs_dir.display()
    );

    for path in json_paths {
        let bytes = fs::read(&path)?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;

        let schema_value = json
            .get("$schema")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("{:?} is missing a string $schema field", path))?;
        assert_eq!(
            schema_value,
            "../packaging-spec.schema.json",
            "unexpected $schema value in {}",
            path.display()
        );

        // Ensure the referenced schema file exists relative to this spec file.
        let schema_path = Path::new(&path)
            .parent()
            .expect("spec has parent dir")
            .join(schema_value);
        assert!(
            schema_path.is_file(),
            "$schema path in {} does not exist: {}",
            path.display(),
            schema_path.display()
        );

        // And ensure aero_packager can load the spec under strict parsing.
        aero_packager::PackagingSpec::load(&path)
            .map_err(|e| anyhow::anyhow!("load {}: {e:#}", path.display()))?;
    }

    Ok(())
}

