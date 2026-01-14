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

pub fn display_rel_path(path: &Path) -> String {
    // Prefer repo-relative paths in errors for readability and stable CI output.
    // Fall back to the provided path as-is if the repo root can't be resolved.
    match repo_root() {
        Ok(repo_root) => path
            .strip_prefix(&repo_root)
            .unwrap_or(path)
            .display()
            .to_string(),
        Err(_) => path.display().to_string(),
    }
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

    if let Some(dir) = env_var_nonempty("AERO_NODE_DIR")
        .or_else(|| env_var_nonempty("AERO_WEB_DIR"))
        .or_else(|| env_var_nonempty("WEB_DIR"))
    {
        return normalize_and_validate_node_dir(repo_root, &dir);
    }

    for candidate in [
        repo_root.to_path_buf(),
        repo_root.join("frontend"),
        repo_root.join("web"),
    ] {
        if candidate.join("package.json").is_file() {
            return Ok(candidate);
        }
    }

    Err(XtaskError::Message(
        "unable to locate package.json; pass --node-dir <path> or set AERO_NODE_DIR (deprecated: AERO_WEB_DIR/WEB_DIR)"
            .to_string(),
    ))
}

pub fn resolve_wasm_crate_dir(repo_root: &Path, cli_override: Option<&str>) -> Result<PathBuf> {
    // Prefer sharing detection logic with other tooling/CI by using the Node-based resolver when
    // available.
    //
    // This keeps crate selection consistent (including ambiguity checks) between:
    // - `cargo xtask test-all`
    // - CI workflows/scripts
    // - `bash ./scripts/test-all.sh` (wrapper)
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

    Err(XtaskError::Message(format!(
        "wasm crate resolver script not found: {detect_script:?} (expected in this repo)."
    )))
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
