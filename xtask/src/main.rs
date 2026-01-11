use std::env;
use std::fs;
use std::io;
use std::path::Path;

mod cmd_test_all;
mod cmd_wasm;
mod cmd_web;
mod error;
// Fixture "sources" live under `tests/fixtures/` so they can be consumed by
// system/integration tests and by this generator.
//
// We compile them into the `xtask` binary via `#[path]` in the module definition
// (see `xtask/src/fixture_sources/`) to avoid any external build tooling.
mod fixture_sources;
mod paths;
mod runner;

use crate::error::{Result, XtaskError};

fn main() {
    if let Err(err) = try_main() {
        eprintln!("error: {err}");
        std::process::exit(err.exit_code());
    }
}

fn try_main() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        return help();
    };

    match cmd.as_str() {
        "fixtures" => cmd_fixtures(args.collect()),
        "test-all" => cmd_test_all::cmd(args.collect()),
        "wasm" => cmd_wasm::cmd(args.collect()),
        "web" => cmd_web::cmd(args.collect()),
        "-h" | "--help" | "help" => help(),
        other => Err(format!(
            "unknown xtask subcommand `{other}` (run `cargo xtask help`)"
        )
        .into()),
    }
}

fn help() -> Result<()> {
    println!(
        "\
Usage:
  cargo xtask fixtures [--check]
  cargo xtask test-all [options] [-- <extra playwright args>]
  cargo xtask wasm [single|threaded|both] [dev|release]
  cargo xtask web dev|build|preview

Commands:
  fixtures   Generate tiny, deterministic test fixtures under `tests/fixtures/`.
  test-all   Run the full test stack (Rust, WASM, TypeScript, Playwright).
  wasm       Build the Rust→WASM packages used by the web app.
  web        Run web (Node/Vite) tasks via npm.

Run `cargo xtask <command> --help` for command-specific help.
"
    );
    Ok(())
}

fn cmd_fixtures(args: Vec<String>) -> Result<()> {
    let mut check = false;
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            other => {
                return Err(XtaskError::Message(format!(
                    "unknown flag for `fixtures`: `{other}`"
                )))
            }
        }
    }

    let root = paths::repo_root()?;
    let out_dir = root.join("tests/fixtures/boot");
    fs::create_dir_all(&out_dir)
        .map_err(|e| XtaskError::Message(format!("create {out_dir:?}: {e}")))?;

    let boot_sector = boot_sector_from_code(fixture_sources::boot_vga_serial::CODE)?;

    // Raw boot sector (exactly 512 bytes).
    ensure_file(
        &out_dir.join("boot_vga_serial.bin"),
        &boot_sector,
        check,
    )?;

    // Tiny "disk image" (8 sectors / 4KiB) whose first sector is the boot sector.
    let disk_img = disk_image_with_fill(&boot_sector, 8)?;
    ensure_file(&out_dir.join("boot_vga_serial_8s.img"), &disk_img, check)?;

    Ok(())
}

fn boot_sector_from_code(code: &[u8]) -> Result<Vec<u8>> {
    if code.len() > 510 {
        return Err(format!(
            "boot sector code is too large: {} bytes (max 510)",
            code.len()
        )
        .into());
    }

    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(code);
    out.resize(510, 0);
    out.push(0x55);
    out.push(0xAA);

    debug_assert_eq!(out.len(), 512);
    Ok(out)
}

fn disk_image_with_fill(boot_sector: &[u8], sectors: usize) -> Result<Vec<u8>> {
    if boot_sector.len() != 512 {
        return Err(format!(
            "boot sector must be exactly 512 bytes, got {}",
            boot_sector.len()
        )
        .into());
    }
    if sectors == 0 {
        return Err("disk image must have at least 1 sector".into());
    }

    let mut img = Vec::with_capacity(sectors * 512);
    img.extend_from_slice(boot_sector);
    for sector_idx in 1..sectors {
        // Deterministic fill pattern: each additional sector is filled with its (u8) sector index.
        img.extend(std::iter::repeat_n(sector_idx as u8, 512));
    }

    Ok(img)
}

fn ensure_file(path: &Path, expected: &[u8], check: bool) -> Result<()> {
    let existing = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(format!("read {path:?}: {err}").into());
        }
    };

    if check {
        let Some(existing) = existing else {
            return Err(format!("{path:?} is missing (run `cargo xtask fixtures`)").into());
        };
        if existing != expected {
            return Err(
                format!("{path:?} is out of date (run `cargo xtask fixtures`)").into(),
            );
        }
        return Ok(());
    }

    if existing.as_deref() != Some(expected) {
        fs::write(path, expected)
            .map_err(|e| XtaskError::Message(format!("write {path:?}: {e}")))?;
    }

    Ok(())
}
