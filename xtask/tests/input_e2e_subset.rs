#![cfg(not(target_arch = "wasm32"))]

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;

/// Verify `cargo xtask input --e2e -- <extra playwright args>` appends extra args after the curated
/// input spec list (so `--project=...` applies to the subset run).
///
/// This test stubs out `cargo`, `npm`, and `node` via PATH so we can validate argv ordering without
/// requiring Node dependencies or running heavyweight test suites.
#[test]
#[cfg(unix)]
fn input_e2e_appends_extra_playwright_args_after_spec_list() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/CARGO_MANIFEST_DIR should have a parent")
        .to_path_buf();

    let _node_modules_lock =
        acquire_node_modules_lock(&repo_root).expect("acquire node_modules lock");

    // `cargo xtask input` refuses to run if node_modules is missing; create a temporary empty one
    // so we can execute the command path without needing actual Node deps.
    let node_modules_dir = repo_root.join("node_modules");
    let created_node_modules = if node_modules_dir.is_dir() {
        false
    } else {
        fs::create_dir(&node_modules_dir).expect("create temporary node_modules dir");
        true
    };
    struct NodeModulesGuard {
        path: PathBuf,
        created: bool,
    }
    impl Drop for NodeModulesGuard {
        fn drop(&mut self) {
            if self.created {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }
    let _node_modules_guard = NodeModulesGuard {
        path: node_modules_dir,
        created: created_node_modules,
    };

    let tmp = tempfile::tempdir().expect("create tempdir");
    let bin_dir = tmp.path().join("bin");
    fs::create_dir(&bin_dir).expect("create bin dir");
    let log_path = tmp.path().join("argv.log");

    write_fake_argv_logger(&bin_dir.join("cargo"), "cargo").expect("write fake cargo");
    write_fake_argv_logger(&bin_dir.join("npm"), "npm").expect("write fake npm");
    write_fake_argv_logger(&bin_dir.join("node"), "node").expect("write fake node");

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", bin_dir.display(), orig_path);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["input", "--e2e", "--", "--project=chromium"])
        .env_remove("AERO_NODE_DIR")
        .env_remove("AERO_WEB_DIR")
        .env_remove("WEB_DIR")
        .env("AERO_XTASK_TEST_LOG", &log_path)
        .env("PATH", path)
        .assert()
        .success();

    let log = fs::read_to_string(&log_path).expect("read argv log");
    let invocations = parse_invocations(&log);

    let npm_e2e = invocations
        .iter()
        .find(|argv| {
            argv.first().map(|s| s.as_str()) == Some("npm")
                && argv.contains(&"test:e2e".to_string())
        })
        .expect("expected an npm test:e2e invocation");

    // Identify the final spec path without hardcoding which spec happens to be last.
    let idx_last_spec = npm_e2e
        .iter()
        .enumerate()
        .filter(|(_, arg)| {
            arg.starts_with("tests/e2e/")
                && (arg.ends_with(".spec.ts") || arg.ends_with(".spec.mjs"))
        })
        .map(|(idx, _)| idx)
        .max()
        .expect("expected at least one curated spec path in npm argv");
    let idx_malformed = npm_e2e
        .iter()
        .position(|arg| arg == "tests/e2e/input_batch_malformed.spec.ts")
        .expect("expected input_batch_malformed spec in npm argv");
    let idx_project = npm_e2e
        .iter()
        .position(|arg| arg == "--project=chromium")
        .expect("expected --project=chromium in npm argv");

    assert!(
        idx_project > idx_last_spec,
        "expected extra Playwright args to come after curated spec list; argv={npm_e2e:?}"
    );
    assert!(
        idx_malformed < idx_project,
        "expected curated spec list (including malformed spec) to come before extra args; argv={npm_e2e:?}"
    );
}

#[cfg(unix)]
fn acquire_node_modules_lock(repo_root: &Path) -> std::io::Result<std::fs::File> {
    fs::create_dir_all(repo_root.join("target"))?;
    let lock_path = repo_root.join("target/xtask-test-node-modules.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;

    // Serialize tests that create/remove `node_modules` so parallel `cargo test` runs remain
    // deterministic.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc == 0 {
        Ok(file)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn write_fake_argv_logger(path: &Path, name: &str) -> std::io::Result<()> {
    let script = format!(
        r#"#!/bin/bash
set -euo pipefail
log="${{AERO_XTASK_TEST_LOG:?}}"
echo "{name}" >> "$log"
for arg in "$@"; do
  echo "$arg" >> "$log"
done
echo "__END__" >> "$log"
exit 0
"#
    );
    fs::write(path, script)?;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn parse_invocations(log: &str) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    let mut current = Vec::new();

    for line in log.lines() {
        if line == "__END__" {
            if !current.is_empty() {
                invocations.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(line.to_string());
    }

    if !current.is_empty() {
        invocations.push(current);
    }

    invocations
}
