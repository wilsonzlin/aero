use std::any::Any;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
mod cmd_bios_rom;
mod cmd_conformance;
mod cmd_input;
#[cfg(feature = "shader-opcode-report")]
mod cmd_shader_opcode_report;
#[cfg(not(feature = "shader-opcode-report"))]
mod cmd_shader_opcode_report {
    use crate::error::{Result, XtaskError};

    pub fn cmd(_args: Vec<String>) -> Result<()> {
        Err(XtaskError::Message(
            "`shader-opcode-report` requires building `xtask` with the `shader-opcode-report` feature.\n\nTry:\n  cargo run -p xtask --locked --features shader-opcode-report -- shader-opcode-report [args...]"
                .to_string(),
        ))
    }
}
mod cmd_snapshot;
mod cmd_test_all;
mod cmd_wasm;
mod cmd_wasm_check;
mod cmd_web;
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
    // When `cargo xtask ... --help` is piped to a command like `head`, Rust's `println!` will
    // panic on EPIPE ("Broken pipe") once the downstream process closes stdout. Treat that as a
    // successful early-termination instead of crashing with a noisy panic.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if is_broken_pipe_panic(info.payload()) {
            return;
        }
        default_hook(info);
    }));

    let result = std::panic::catch_unwind(|| {
        if let Err(err) = try_main() {
            eprintln!("error: {err}");
            std::process::exit(err.exit_code());
        }
    });

    if let Err(payload) = result {
        if is_broken_pipe_panic(payload.as_ref()) {
            return;
        }
        std::panic::resume_unwind(payload);
    }
}

fn try_main() -> Result<()> {
    let repo_root = paths::repo_root()?;
    maybe_isolate_cargo_home(&repo_root)?;

    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        return help();
    };

    let args = strip_global_noop_flags(args.collect());

    match cmd.as_str() {
        "bios-rom" => cmd_bios_rom::cmd(args),
        "fixtures" => cmd_fixtures(args),
        "conformance" => cmd_conformance::cmd(args),
        "input" => cmd_input::cmd(args),
        "snapshot" => cmd_snapshot::cmd(args),
        "shader-opcode-report" => cmd_shader_opcode_report::cmd(args),
        "test-all" => cmd_test_all::cmd(args),
        "wasm" => cmd_wasm::cmd(args),
        "wasm-check" => cmd_wasm_check::cmd(args),
        "web" => cmd_web::cmd(args),
        "-h" | "--help" | "help" => help(),
        other => Err(format!("unknown xtask subcommand `{other}` (run `cargo xtask help`)").into()),
    }
}

fn strip_global_noop_flags(args: Vec<String>) -> Vec<String> {
    // `cargo xtask` is an alias for `cargo run --locked -p xtask -- ...`. This means cargo-level
    // flags like `--locked` cannot be passed after the xtask subcommand without reaching the xtask
    // binary itself, which historically caused confusing "unexpected argument" failures.
    //
    // Treat `--locked` as a global no-op unless the user explicitly passes it after `--` (i.e.
    // forwarding to a child command/test binary).
    let mut out = Vec::with_capacity(args.len());
    let mut passthrough = false;
    for arg in args {
        if passthrough {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg);
            continue;
        }
        if arg == "--locked" {
            continue;
        }
        out.push(arg);
    }
    out
}

fn maybe_isolate_cargo_home(repo_root: &Path) -> Result<()> {
    let Ok(raw) = env::var("AERO_ISOLATE_CARGO_HOME") else {
        return Ok(());
    };
    let value = raw.trim();
    if value.is_empty() {
        return Ok(());
    }

    let is_false = matches!(value, "0" | "false" | "FALSE" | "no" | "NO" | "off" | "OFF");
    if is_false {
        return Ok(());
    }

    let mut cargo_home: PathBuf = if matches!(
        value,
        "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
    ) {
        repo_root.join(".cargo-home")
    } else {
        let mut custom = value.to_string();

        if custom == "~" || custom.starts_with("~/") {
            match env::var("HOME").or_else(|_| env::var("USERPROFILE")) {
                Ok(home) if !home.is_empty() => {
                    custom = format!("{}{}", home.trim_end_matches('/'), &custom[1..]);
                }
                _ => {
                    eprintln!(
                        "warning: cannot expand '~' in AERO_ISOLATE_CARGO_HOME because HOME/USERPROFILE is unset; using literal path: {custom}"
                    );
                }
            }
        } else if custom.starts_with('~') {
            eprintln!(
                "warning: AERO_ISOLATE_CARGO_HOME only supports '~' or '~/' expansion; using literal path: {custom}"
            );
        }

        let path = PathBuf::from(custom);
        if path.is_absolute() {
            path
        } else {
            repo_root.join(path)
        }
    };

    fs::create_dir_all(&cargo_home).map_err(|e| {
        XtaskError::Message(format!(
            "failed to create isolated Cargo home directory {}: {e}",
            cargo_home.display()
        ))
    })?;
    // Ensure the path is normalized for consistent downstream env usage.
    cargo_home = cargo_home.canonicalize().unwrap_or(cargo_home);
    env::set_var("CARGO_HOME", &cargo_home);

    Ok(())
}

fn is_broken_pipe_panic(payload: &(dyn Any + Send)) -> bool {
    let msg = if let Some(s) = payload.downcast_ref::<String>() {
        s.as_str()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        *s
    } else {
        return false;
    };

    msg.contains("failed printing to stdout") && msg.contains("Broken pipe")
}

fn help() -> Result<()> {
    println!(
        "\
Usage:
  cargo xtask bios-rom [--check]
  cargo xtask fixtures [--check]
  cargo xtask conformance [options] [-- <test args>]
  cargo xtask input [--e2e] [--machine] [--wasm] [--with-wasm] [--usb-all] [--rust-only] [--node-dir <path>] [-- <extra playwright args>]
  cargo xtask snapshot inspect <path>
  cargo xtask snapshot validate [--deep] <path>
  cargo xtask snapshot diff <path_a> <path_b> [--deep]
  cargo run -p xtask --locked --features shader-opcode-report -- shader-opcode-report [--deny-unsupported] <files...>
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
  shader-opcode-report
               Report SM2/3 opcode usage and unsupported opcodes for the D3D9 SM2/3 translator.
               (Requires `--features shader-opcode-report`; see usage above.)
  test-all   Run the full test stack (Rust, WASM, TypeScript, Playwright). Also validates
             deterministic in-repo fixtures by default (use `cargo xtask test-all --help`).
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
    let boot_fixtures_dir = fixtures_dir.join("boot");
    if !check {
        fs::create_dir_all(&boot_fixtures_dir)
            .map_err(|e| XtaskError::Message(format!("create {boot_fixtures_dir:?}: {e}")))?;
    }

    let mut fixtures = FixtureWriter::new(check);

    let boot_sector = boot_sector_from_code(fixture_sources::boot_vga_serial::CODE)?;

    // Raw boot sector (exactly 512 bytes).
    fixtures.ensure_file(&boot_fixtures_dir.join("boot_vga_serial.bin"), &boot_sector)?;

    // Tiny "disk image" (8 sectors / 4KiB) whose first sector is the boot sector.
    let disk_img = disk_image_with_fill(&boot_sector, 8)?;
    fixtures.ensure_file(&boot_fixtures_dir.join("boot_vga_serial_8s.img"), &disk_img)?;

    // Legacy BIOS interrupt sanity boot sector (`int_sanity.asm`), kept in-repo to avoid requiring
    // an assembler in CI.
    fixtures.ensure_file(
        &boot_fixtures_dir.join("int_sanity.bin"),
        &fixture_sources::int_sanity::BIN,
    )?;

    // Legacy tiny boot fixtures at `tests/fixtures/*.bin` (generated here to
    // avoid requiring external assemblers like `nasm` / GNU `as` in CI).
    fixtures.ensure_file(
        &fixtures_dir.join("bootsector.bin"),
        &fixture_sources::bootsector::BIN,
    )?;
    let realmode_vbe_test = boot_sector_from_code(fixture_sources::realmode_vbe_test::CODE)?;
    fixtures.ensure_file(
        &fixtures_dir.join("realmode_vbe_test.bin"),
        &realmode_vbe_test,
    )?;

    // QEMU differential-test boot sector (`tools/qemu_diff/boot/boot.S`), committed and
    // regenerated here so `qemu_diff` does not need an assembler toolchain in CI.
    fixtures.ensure_file(
        &root.join("tools/qemu_diff/boot/boot.bin"),
        &fixture_sources::qemu_diff_boot::BIN,
    )?;

    // Firmware fixtures (kept in-repo; must remain small and deterministic).
    let placement = AcpiPlacement::default();

    // ACPI DSDT (legacy PCI root bridge; ECAM/MMCONFIG disabled).
    let cfg = AcpiConfig::default();
    let acpi_tables = AcpiTables::build(&cfg, placement);
    fixtures.ensure_file(
        &root.join("crates/firmware/acpi/dsdt.aml"),
        &acpi_tables.dsdt,
    )?;

    // ACPI DSDT with PCIe ECAM/MMCONFIG enabled (PCI0 becomes a PCIe root bridge).
    let cfg = AcpiConfig {
        pcie_ecam_base: aero_pc_constants::PCIE_ECAM_BASE,
        pcie_segment: aero_pc_constants::PCIE_ECAM_SEGMENT,
        pcie_start_bus: aero_pc_constants::PCIE_ECAM_START_BUS,
        pcie_end_bus: aero_pc_constants::PCIE_ECAM_END_BUS,
        ..Default::default()
    };
    let acpi_tables = AcpiTables::build(&cfg, placement);
    fixtures.ensure_file(
        &root.join("crates/firmware/acpi/dsdt_pcie.aml"),
        &acpi_tables.dsdt,
    )?;

    // BIOS ROM image.
    let bios_rom = firmware::bios::build_bios_rom();
    cmd_bios_rom::validate_bios_rom(&bios_rom)?;
    fixtures.ensure_file(&root.join("assets/bios.bin"), &bios_rom)?;

    fixtures.finish()
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

struct FixtureWriter {
    check: bool,
    failures: Vec<String>,
}

impl FixtureWriter {
    fn new(check: bool) -> Self {
        Self {
            check,
            failures: Vec::new(),
        }
    }

    fn ensure_file(&mut self, path: &Path, expected: &[u8]) -> Result<()> {
        let path_display = paths::display_rel_path(path);
        let existing = match fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                return Err(format!("read {path_display}: {err}").into());
            }
        };

        if self.check {
            let Some(existing) = existing else {
                self.failures.push(format!("- {path_display} (missing)"));
                return Ok(());
            };
            if existing != expected {
                self.failures
                    .push(format!("- {path_display} (out of date)"));
            }
            return Ok(());
        }

        if existing.as_deref() != Some(expected) {
            fs::write(path, expected)
                .map_err(|e| XtaskError::Message(format!("write {path_display}: {e}")))?;
        }

        Ok(())
    }

    fn finish(self) -> Result<()> {
        if !self.check || self.failures.is_empty() {
            return Ok(());
        }

        Err(XtaskError::Message(format!(
            "fixtures are out of date:\n{}\n\nrun `cargo xtask fixtures` to regenerate",
            self.failures.join("\n")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::strip_global_noop_flags;

    #[test]
    fn strip_global_noop_flags_removes_locked_before_double_dash() {
        let out = strip_global_noop_flags(vec![
            "wasm".to_string(),
            "--locked".to_string(),
            "single".to_string(),
        ]);
        assert_eq!(out, vec!["wasm".to_string(), "single".to_string()]);
    }

    #[test]
    fn strip_global_noop_flags_preserves_locked_after_double_dash() {
        let out = strip_global_noop_flags(vec![
            "--".to_string(),
            "--locked".to_string(),
            "foo".to_string(),
        ]);
        assert_eq!(
            out,
            vec!["--".to_string(), "--locked".to_string(), "foo".to_string()]
        );
    }
}
