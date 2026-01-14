use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::process::Command;

pub fn print_help() {
    println!(
        "\
Compile-check wasm32 compatibility for selected crates.

Usage:
  cargo xtask wasm-check

Currently checked:
  - aero-devices-gpu
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    if !args.is_empty() {
        return Err(XtaskError::Message(
            "unexpected arguments (run `cargo xtask wasm-check --help`)".to_string(),
        ));
    }

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let cargo_locked = repo_root.join("Cargo.lock").is_file();
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

    Ok(())
}
