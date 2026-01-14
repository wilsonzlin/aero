use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
struct ForbiddenApiUse {
    path: PathBuf,
    line: usize,
    pattern: &'static str,
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = fs::read_dir(dir).map_err(|e| {
        XtaskError::Message(format!(
            "wasm-check: failed to read dir {}: {e}",
            paths::display_rel_path(dir)
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| {
            XtaskError::Message(format!(
                "wasm-check: failed to read dir entry in {}: {e}",
                paths::display_rel_path(dir)
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
            continue;
        }
        if path.extension() == Some(OsStr::new("rs")) {
            out.push(path);
        }
    }
    Ok(())
}

fn check_aero_devices_gpu_no_host_only_apis(repo_root: &Path) -> Result<()> {
    // `aero-devices-gpu` is used by both native hosts and the browser runtime. Some "host-like"
    // std APIs technically compile on wasm32 but rely on JS shims or have semantics that do not
    // match Aero's externally supplied deterministic time base.
    //
    // Guardrail: ensure the core device model sources do not use obviously host-only APIs.
    // (Tests may still use them behind cfg gates.)
    let src_dir = repo_root.join("crates/aero-devices-gpu/src");
    if !src_dir.is_dir() {
        return Err(XtaskError::Message(format!(
            "wasm-check: expected directory missing: {}",
            paths::display_rel_path(&src_dir)
        )));
    }

    // Keep the list small to avoid false positives.
    const FORBIDDEN: &[&str] = &[
        "std::time::Instant",
        "Instant::now",
        "std::thread",
        "std::fs",
    ];

    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files)?;
    files.sort();

    let mut hits: Vec<ForbiddenApiUse> = Vec::new();
    for file in files {
        let contents = fs::read_to_string(&file).map_err(|e| {
            XtaskError::Message(format!(
                "wasm-check: failed to read {}: {e}",
                paths::display_rel_path(&file)
            ))
        })?;
        for (idx, line) in contents.lines().enumerate() {
            for &needle in FORBIDDEN {
                if line.contains(needle) {
                    hits.push(ForbiddenApiUse {
                        path: file.clone(),
                        line: idx + 1,
                        pattern: needle,
                    });
                }
            }
        }
    }

    if hits.is_empty() {
        return Ok(());
    }

    let mut msg = String::new();
    msg.push_str(
        "wasm-check: forbidden host-only std APIs detected in aero-devices-gpu sources:\n",
    );
    for hit in hits {
        msg.push_str(&format!(
            "- {}:{}: `{}`\n",
            paths::display_rel_path(&hit.path),
            hit.line,
            hit.pattern
        ));
    }
    msg.push_str("\nThis crate is used by the browser runtime; keep the default feature set free of host-only APIs (Instant/threads/file I/O).\n");
    Err(XtaskError::Message(msg))
}

pub fn print_help() {
    println!(
        "\
Compile-check wasm32 compatibility for selected crates.

Usage:
  cargo xtask wasm-check [--locked]

Currently checked:
  - aero-devices-gpu
  - aero-wasm (standalone, to avoid feature unification masking)
  - aero-machine + aero-wasm (together)
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    let mut force_locked = false;
    for arg in args {
        match arg.as_str() {
            "--locked" => force_locked = true,
            other => {
                return Err(XtaskError::Message(format!(
                    "unexpected argument for `wasm-check`: `{other}` (run `cargo xtask wasm-check --help`)"
                )));
            }
        }
    }

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    check_aero_devices_gpu_no_host_only_apis(&repo_root)?;

    let cargo_locked = force_locked || repo_root.join("Cargo.lock").is_file();
    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-devices-gpu");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step("Rust: cargo check (wasm32, aero-devices-gpu)", &mut cmd)?;
    }

    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-wasm");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step("Rust: cargo check (wasm32, aero-wasm)", &mut cmd)?;
    }

    {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&repo_root)
            .arg("check")
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-p")
            .arg("aero-machine")
            .arg("-p")
            .arg("aero-wasm");
        if cargo_locked {
            cmd.arg("--locked");
        }
        runner.run_step(
            "Rust: cargo check (wasm32, aero-machine, aero-wasm)",
            &mut cmd,
        )?;
    }

    Ok(())
}
