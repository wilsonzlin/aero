use crate::error::{Result, XtaskError};
use crate::paths;
use crate::runner::Runner;
use std::process::Command;

pub fn print_help() {
    println!(
        "\
Build the Rust→WASM packages used by the web app.

Usage:
  cargo xtask wasm [single|threaded|both] [dev|release]

Defaults:
  cargo xtask wasm both release
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    let Some((variant, mode)) = parse_args(args)? else {
        return Ok(());
    };

    let repo_root = paths::repo_root()?;
    let runner = Runner::new();

    let web_build_script = repo_root.join("web/scripts/build_wasm.mjs");

    let variants: Vec<&str> = match variant.as_str() {
        "single" => vec!["single"],
        "threaded" => vec!["threaded"],
        "both" => vec!["single", "threaded"],
        other => {
            return Err(XtaskError::Message(format!(
                "unknown wasm variant `{other}` (expected: single|threaded|both)"
            )));
        }
    };

    let (mode_flag, mode_str) = match mode.as_str() {
        "dev" => ("dev", "dev"),
        "release" => ("release", "release"),
        other => {
            return Err(XtaskError::Message(format!(
                "unknown wasm mode `{other}` (expected: dev|release)"
            )));
        }
    };

    if web_build_script.is_file() {
        for v in variants {
            let mut cmd = Command::new("node");
            cmd.current_dir(&repo_root)
                .arg(&web_build_script)
                .arg(v)
                .arg(mode_flag);
            runner.run_step(&format!("WASM: build ({v} {mode_str})"), &mut cmd)?;
        }
        return Ok(());
    }

    // Fallback: build directly with wasm-pack (single-threaded only).
    if variants.iter().any(|v| *v == "threaded") {
        return Err(XtaskError::Message(
            "threaded wasm builds require `web/scripts/build_wasm.mjs`".to_string(),
        ));
    }

    let wasm_crate_dir = paths::resolve_wasm_crate_dir(&repo_root, None)?;
    let mut cmd = Command::new("wasm-pack");
    cmd.current_dir(&repo_root)
        .args(["build", "--target", "web", if mode_flag == "dev" { "--dev" } else { "--release" }])
        .arg(&wasm_crate_dir);
    runner.run_step(
        &format!(
            "WASM: wasm-pack build ({}, {mode_str})",
            wasm_crate_dir.display()
        ),
        &mut cmd,
    )?;

    Ok(())
}

fn parse_args(args: Vec<String>) -> Result<Option<(String, String)>> {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(None);
    }

    match args.as_slice() {
        [] => Ok(Some(("both".to_string(), "release".to_string()))),
        [a] => {
            if matches!(a.as_str(), "single" | "threaded" | "both") {
                Ok(Some((a.to_string(), "release".to_string())))
            } else if matches!(a.as_str(), "dev" | "release") {
                Ok(Some(("both".to_string(), a.to_string())))
            } else {
                Err(XtaskError::Message(format!(
                    "unknown argument `{a}` (run `cargo xtask wasm --help`)"
                )))
            }
        }
        [variant, mode] => Ok(Some((variant.to_string(), mode.to_string()))),
        _ => Err(XtaskError::Message(
            "too many arguments (run `cargo xtask wasm --help`)".to_string(),
        )),
    }
}

