use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::process::Command;

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
