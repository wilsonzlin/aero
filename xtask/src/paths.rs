use crate::error::{Result, XtaskError};
use std::env;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn repo_root() -> Result<PathBuf> {
    // `CARGO_MANIFEST_DIR` points at `<repo>/xtask`, so the parent is the repo root.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| XtaskError::Message("failed to locate repo root".to_string()))
}

pub fn resolve_node_dir(repo_root: &Path, cli_override: Option<&str>) -> Result<PathBuf> {
    // Prefer sharing detection logic with other tooling/CI by using the Node-based resolver when
    // available.
    let detect_script = repo_root.join("scripts/ci/detect-node-dir.mjs");
    if detect_script.is_file() {
        let mut cmd = Command::new("node");
        cmd.current_dir(repo_root).arg(&detect_script);
        if let Some(dir) = cli_override {
            cmd.args(["--node-dir", dir]);
        }

        let output = cmd.output().map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                XtaskError::Message("missing required command: node".to_string())
            }
            _ => XtaskError::Message(format!("failed to run detect-node-dir script: {err}")),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(XtaskError::Message(format!(
                "detect-node-dir failed (exit code {:?}): {stderr}",
                output.status.code()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut dir: Option<&str> = None;
        for line in stdout.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "dir" {
                dir = Some(value.trim());
                break;
            }
        }

        let Some(dir) = dir.filter(|v| !v.is_empty()) else {
            return Err(XtaskError::Message(
                "detect-node-dir did not return a workspace directory".to_string(),
            ));
        };

        let resolved = normalize_dir(repo_root, dir);
        if resolved.join("package.json").is_file() {
            return Ok(resolved);
        }

        return Err(XtaskError::Message(format!(
            "detected node dir does not contain package.json: {resolved:?}"
        )));
    }

    if let Some(dir) = cli_override {
        return normalize_and_validate_node_dir(repo_root, dir);
    }

    if let Some(dir) = env_var_nonempty("AERO_NODE_DIR").or_else(|| env_var_nonempty("AERO_WEB_DIR"))
    {
        return normalize_and_validate_node_dir(repo_root, &dir);
    }

    for candidate in [repo_root.to_path_buf(), repo_root.join("frontend"), repo_root.join("web")] {
        if candidate.join("package.json").is_file() {
            return Ok(candidate);
        }
    }

    Err(XtaskError::Message(
        "unable to locate package.json; pass --node-dir <path> or set AERO_NODE_DIR".to_string(),
    ))
}

pub fn resolve_wasm_crate_dir(repo_root: &Path, cli_override: Option<&str>) -> Result<PathBuf> {
    // Prefer sharing detection logic with other tooling/CI by using the Node-based resolver when
    // available.
    //
    // This keeps crate selection consistent (including ambiguity checks) between:
    // - `cargo xtask test-all`
    // - CI workflows/scripts
    // - `./scripts/test-all.sh` (wrapper)
    let detect_script = repo_root.join("scripts/ci/detect-wasm-crate.mjs");
    if detect_script.is_file() {
        let mut cmd = Command::new("node");
        cmd.current_dir(repo_root).arg(&detect_script);

        // Legacy compatibility: `AERO_WASM_DIR` predates the `AERO_WASM_CRATE_DIR` env var.
        let legacy_env_override = env_var_nonempty("AERO_WASM_DIR");

        if let Some(dir) = cli_override {
            cmd.args(["--wasm-crate-dir", dir]);
        } else if env_var_nonempty("AERO_WASM_CRATE_DIR").is_none() {
            if let Some(dir) = legacy_env_override.as_deref() {
                cmd.args(["--wasm-crate-dir", dir]);
            }
        }

        let output = cmd.output().map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => {
                XtaskError::Message("missing required command: node".to_string())
            }
            _ => XtaskError::Message(format!("failed to run detect-wasm-crate script: {err}")),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(XtaskError::Message(format!(
                "detect-wasm-crate failed (exit code {:?}): {stderr}",
                output.status.code()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut dir: Option<&str> = None;
        for line in stdout.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "dir" {
                dir = Some(value.trim());
                break;
            }
        }

        let Some(dir) = dir.filter(|v| !v.is_empty()) else {
            return Err(XtaskError::Message(
                "detect-wasm-crate did not return a crate directory".to_string(),
            ));
        };

        let resolved = normalize_dir(repo_root, dir);
        if resolved.join("Cargo.toml").is_file() {
            return Ok(resolved);
        }

        return Err(XtaskError::Message(format!(
            "detected wasm crate dir does not contain Cargo.toml: {resolved:?}"
        )));
    }

    // Fallback implementation when the shared resolver script is unavailable.
    if let Some(dir) = cli_override {
        return normalize_and_validate_wasm_crate_dir(repo_root, dir);
    }

    if let Some(dir) = env_var_nonempty("AERO_WASM_CRATE_DIR").or_else(|| env_var_nonempty("AERO_WASM_DIR")) {
        return normalize_and_validate_wasm_crate_dir(repo_root, &dir);
    }

    let canonical = repo_root.join("crates/aero-wasm");
    if canonical.join("Cargo.toml").is_file() {
        return Ok(canonical);
    }

    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(repo_root)
        .output()
        .map_err(|err| {
            XtaskError::Message(format!(
                "failed to run `cargo metadata` for wasm crate auto-detection: {err}"
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(XtaskError::Message(format!(
            "`cargo metadata` failed (exit code {:?}): {stderr}",
            output.status.code()
        )));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        XtaskError::Message(format!("failed to parse `cargo metadata` output as JSON: {err}"))
    })?;

    let Some(packages) = json.get("packages").and_then(|v| v.as_array()) else {
        return Err(XtaskError::Message(
            "`cargo metadata` output did not include a `packages` array".to_string(),
        ));
    };

    let mut cdylib_dirs: Vec<PathBuf> = Vec::new();
    for pkg in packages {
        let Some(targets) = pkg.get("targets").and_then(|v| v.as_array()) else {
            continue;
        };

        let has_cdylib = targets.iter().any(|t| {
            t.get("kind")
                .and_then(|v| v.as_array())
                .is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("cdylib")))
        });
        if !has_cdylib {
            continue;
        }

        let Some(manifest_path) = pkg.get("manifest_path").and_then(|v| v.as_str()) else {
            continue;
        };

        let manifest_path = PathBuf::from(manifest_path);
        let Some(dir) = manifest_path.parent() else {
            continue;
        };
        cdylib_dirs.push(dir.to_path_buf());
    }

    match cdylib_dirs.as_slice() {
        [] => Err(XtaskError::Message(
            "unable to auto-detect a wasm-pack crate (no workspace packages expose a cdylib target); set AERO_WASM_CRATE_DIR"
                .to_string(),
        )),
        [single] => Ok(single.clone()),
        _ => Err(XtaskError::Message(
            "multiple workspace crates expose a cdylib target; set AERO_WASM_CRATE_DIR to disambiguate"
                .to_string(),
        )),
    }
}

fn normalize_and_validate_node_dir(repo_root: &Path, dir: &str) -> Result<PathBuf> {
    let dir = normalize_dir(repo_root, dir);
    if dir.join("package.json").is_file() {
        Ok(dir)
    } else {
        Err(XtaskError::Message(format!(
            "package.json not found in node dir: {dir:?}"
        )))
    }
}

fn normalize_and_validate_wasm_crate_dir(repo_root: &Path, dir: &str) -> Result<PathBuf> {
    let dir = normalize_dir(repo_root, dir);
    if dir.join("Cargo.toml").is_file() {
        Ok(dir)
    } else {
        Err(XtaskError::Message(format!(
            "Cargo.toml not found in wasm crate dir: {dir:?}"
        )))
    }
}

fn normalize_dir(repo_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        repo_root.join(path)
    }
}

fn env_var_nonempty(key: &str) -> Option<String> {
    let value = env::var(key).ok()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}
