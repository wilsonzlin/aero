use std::env;
use std::fs;
use std::io;
use std::path::Path;

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
mod cmd_bios_rom;
mod cmd_snapshot;
mod cmd_test_all;
mod cmd_input;
mod cmd_wasm;
mod cmd_wasm_check;
mod cmd_web;
mod cmd_conformance;
mod error;

// Fixture "sources" are compiled into the `xtask` binary to keep generation
// deterministic and license-safe (no external OS images or assembler toolchains
// required in CI).
//
// Some sources live under `tests/fixtures/` (so they can also be consumed by
// integration tests), and others live directly under `xtask/src/fixture_sources/`.
// Modules under `tests/fixtures/` are compiled in via `#[path]` (see
// `xtask/src/fixture_sources/mod.rs`).
mod fixture_sources;
mod paths;
mod runner;
mod tools;

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
        "bios-rom" => cmd_bios_rom::cmd(args.collect()),
        "fixtures" => cmd_fixtures(args.collect()),
        "conformance" => cmd_conformance::cmd(args.collect()),
        "input" => cmd_input::cmd(args.collect()),
        "snapshot" => cmd_snapshot::cmd(args.collect()),
        "test-all" => cmd_test_all::cmd(args.collect()),
        "wasm" => cmd_wasm::cmd(args.collect()),
        "wasm-check" => cmd_wasm_check::cmd(args.collect()),
        "web" => cmd_web::cmd(args.collect()),
        "-h" | "--help" | "help" => help(),
        other => Err(format!("unknown xtask subcommand `{other}` (run `cargo xtask help`)").into()),
    }
}

fn help() -> Result<()> {
    println!(
        "\
Usage:
  cargo xtask bios-rom [--check]
  cargo xtask fixtures [--check]
  cargo xtask conformance [options] [-- <test args>]
  cargo xtask input [--e2e] [-- <extra playwright args>]
  cargo xtask snapshot inspect <path>
  cargo xtask snapshot validate [--deep] <path>
  cargo xtask snapshot diff <path_a> <path_b> [--deep]
  cargo xtask test-all [options] [-- <extra playwright args>]
  cargo xtask wasm [single|threaded|both] [dev|release]
  cargo xtask wasm-check
  cargo xtask web dev|build|preview

Commands:
  bios-rom   Generate/check the in-repo 64KiB BIOS ROM fixture at `assets/bios.bin`.
  fixtures   Generate tiny, deterministic in-repo fixtures (boot sectors + firmware blobs).
  conformance Run instruction conformance / differential tests (x86_64 unix only).
  input      Run the USB/input-focused test suite (Rust + web; optional Playwright subset).
  snapshot   Inspect/validate/diff an `aero-snapshot` file without loading multi-GB RAM payloads.
  test-all   Run the full test stack (Rust, WASM, TypeScript, Playwright). Also validates
             deterministic in-repo fixtures when Rust tests are enabled.
  wasm       Build the Rust→WASM packages used by the web app.
  wasm-check Compile-check wasm32 compatibility for selected crates (e.g. `aero-devices-gpu`).
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
    let fixtures_dir = root.join("tests/fixtures");
    fs::create_dir_all(&fixtures_dir)
        .map_err(|e| XtaskError::Message(format!("create {fixtures_dir:?}: {e}")))?;

    let boot_fixtures_dir = fixtures_dir.join("boot");
    fs::create_dir_all(&boot_fixtures_dir)
        .map_err(|e| XtaskError::Message(format!("create {boot_fixtures_dir:?}: {e}")))?;

    let boot_sector = boot_sector_from_code(fixture_sources::boot_vga_serial::CODE)?;

    // Raw boot sector (exactly 512 bytes).
    ensure_file(
        &boot_fixtures_dir.join("boot_vga_serial.bin"),
        &boot_sector,
        check,
    )?;

    // Tiny "disk image" (8 sectors / 4KiB) whose first sector is the boot sector.
    let disk_img = disk_image_with_fill(&boot_sector, 8)?;
    ensure_file(
        &boot_fixtures_dir.join("boot_vga_serial_8s.img"),
        &disk_img,
        check,
    )?;

    // Legacy BIOS interrupt sanity boot sector (`int_sanity.asm`), kept in-repo to avoid requiring
    // an assembler in CI.
    ensure_file(
        &boot_fixtures_dir.join("int_sanity.bin"),
        &fixture_sources::int_sanity::BIN,
        check,
    )?;

    // Legacy tiny boot fixtures at `tests/fixtures/*.bin` (generated here to
    // avoid requiring external assemblers like `nasm` / GNU `as` in CI).
    ensure_file(
        &fixtures_dir.join("bootsector.bin"),
        &fixture_sources::bootsector::BIN,
        check,
    )?;
    let realmode_vbe_test = boot_sector_from_code(fixture_sources::realmode_vbe_test::CODE)?;
    ensure_file(
        &fixtures_dir.join("realmode_vbe_test.bin"),
        &realmode_vbe_test,
        check,
    )?;

    // QEMU differential-test boot sector (`tools/qemu_diff/boot/boot.S`), committed and
    // regenerated here so `qemu_diff` does not need an assembler toolchain in CI.
    ensure_file(
        &root.join("tools/qemu_diff/boot/boot.bin"),
        &fixture_sources::qemu_diff_boot::BIN,
        check,
    )?;

    // Firmware fixtures (kept in-repo; must remain small and deterministic).
    let placement = AcpiPlacement::default();

    // ACPI DSDT (legacy PCI root bridge; ECAM/MMCONFIG disabled).
    let cfg = AcpiConfig::default();
    let acpi_tables = AcpiTables::build(&cfg, placement);
    ensure_file(&root.join("crates/firmware/acpi/dsdt.aml"), &acpi_tables.dsdt, check)?;

    // ACPI DSDT with PCIe ECAM/MMCONFIG enabled (PCI0 becomes a PCIe root bridge).
    let cfg = AcpiConfig {
        pcie_ecam_base: aero_pc_constants::PCIE_ECAM_BASE,
        pcie_segment: aero_pc_constants::PCIE_ECAM_SEGMENT,
        pcie_start_bus: aero_pc_constants::PCIE_ECAM_START_BUS,
        pcie_end_bus: aero_pc_constants::PCIE_ECAM_END_BUS,
        ..Default::default()
    };
    let acpi_tables = AcpiTables::build(&cfg, placement);
    ensure_file(
        &root.join("crates/firmware/acpi/dsdt_pcie.aml"),
        &acpi_tables.dsdt,
        check,
    )?;

    // BIOS ROM image.
    let bios_rom = firmware::bios::build_bios_rom();
    cmd_bios_rom::validate_bios_rom(&bios_rom)?;
    ensure_file(&root.join("assets/bios.bin"), &bios_rom, check)?;

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
    let path_display = display_rel_path(path);
    let existing = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(format!("read {path_display}: {err}").into());
        }
    };

    if check {
        let Some(existing) = existing else {
            return Err(format!("{path_display} is missing (run `cargo xtask fixtures`)").into());
        };
        if existing != expected {
            return Err(
                format!("{path_display} is out of date (run `cargo xtask fixtures`)").into(),
            );
        }
        return Ok(());
    }

    if existing.as_deref() != Some(expected) {
        fs::write(path, expected)
            .map_err(|e| XtaskError::Message(format!("write {path_display}: {e}")))?;
    }

    Ok(())
}

fn display_rel_path(path: &Path) -> String {
    // Prefer a repo-relative path in errors for readability. This stays stable
    // across machines and makes CI output more actionable.
    match paths::repo_root() {
        Ok(repo_root) => path
            .strip_prefix(&repo_root)
            .unwrap_or(path)
            .display()
            .to_string(),
        Err(_) => path.display().to_string(),
    }
}
