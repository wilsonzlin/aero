use std::{
    ffi::OsStr,
    fs,
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_pc_constants::{PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_START_BUS};

fn iasl_available() -> bool {
    match Command::new("iasl").arg("-v").output() {
        Ok(_) => true,
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => panic!("failed to invoke `iasl`: {err}"),
    }
}

fn run_iasl(current_dir: &Path, args: &[&str]) -> io::Result<Output> {
    Command::new("iasl")
        .args(args)
        .current_dir(current_dir)
        .output()
}

fn fmt_output(output: &Output) -> String {
    let mut msg = String::new();
    msg.push_str(&format!("status: {}\n", output.status));
    if !output.stdout.is_empty() {
        msg.push_str("stdout:\n");
        msg.push_str(&String::from_utf8_lossy(&output.stdout));
        if !msg.ends_with('\n') {
            msg.push('\n');
        }
    }
    if !output.stderr.is_empty() {
        msg.push_str("stderr:\n");
        msg.push_str(&String::from_utf8_lossy(&output.stderr));
        if !msg.ends_with('\n') {
            msg.push('\n');
        }
    }
    msg
}

fn find_generated_dsl(temp_dir: &Path) -> io::Result<PathBuf> {
    // In practice, `iasl -d dsdt.aml` produces `dsdt.dsl`, but prefer discovery
    // over hard-coding to tolerate tool/version differences.
    let preferred = temp_dir.join("dsdt.dsl");
    if preferred.exists() {
        return Ok(preferred);
    }

    let mut found = Vec::new();
    for entry in fs::read_dir(temp_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dsl"))
        {
            found.push(path);
        }
    }

    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "iasl did not produce a .dsl file",
        )),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("iasl produced multiple .dsl files: {found:?}"),
        )),
    }
}

fn iasl_disassemble_files(temp_path: &Path, files: &[&str]) {
    for file in files {
        let disasm = run_iasl(temp_path, &["-d", file])
            .unwrap_or_else(|err| panic!("failed to spawn `iasl -d {file}`: {err}"));
        if !disasm.status.success() {
            panic!(
                "`iasl -d {file}` failed\n{}\n(temp dir: {})",
                fmt_output(&disasm),
                temp_path.display()
            );
        }
    }
}

fn dsdt_iasl_roundtrip(cfg: &AcpiConfig, label: &str) {
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(cfg, placement);

    let tempdir = tempfile::Builder::new()
        .prefix(&format!("aero-acpi-iasl-{label}-"))
        .tempdir()
        .expect("create tempdir");
    let temp_path = tempdir.path();

    // `iasl -d` expects a full ACPI table blob (header + AML).
    let dsdt_aml_path = temp_path.join("dsdt.aml");
    fs::write(&dsdt_aml_path, &tables.dsdt).expect("write dsdt.aml");

    // Disassemble DSDT AML -> DSL.
    let disasm = run_iasl(temp_path, &["-d", "dsdt.aml"])
        .unwrap_or_else(|err| panic!("failed to spawn `iasl -d dsdt.aml`: {err}"));
    if !disasm.status.success() {
        panic!(
            "`iasl -d dsdt.aml` failed\n{}\n(temp dir: {})",
            fmt_output(&disasm),
            temp_path.display()
        );
    }

    let dsdt_dsl = find_generated_dsl(temp_path).unwrap_or_else(|err| {
        let files: Vec<_> = fs::read_dir(temp_path)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        panic!(
            "failed to locate disassembled DSDT DSL: {err}\n(files: {files:?})\n(temp dir: {})",
            temp_path.display()
        );
    });

    // Recompile DSL -> AML.
    let output_prefix = format!("dsdt_recompiled_{label}");
    let recompile = run_iasl(
        temp_path,
        &[
            "-tc",
            "-p",
            &output_prefix,
            dsdt_dsl
                .file_name()
                .expect("dsdt.dsl has a file name")
                .to_str()
                .expect("dsdt.dsl file name is UTF-8"),
        ],
    )
    .expect("spawn `iasl -tc`");

    if !recompile.status.success() {
        panic!(
            "`iasl -tc -p {output_prefix} ...` failed\n{}\n(temp dir: {})",
            fmt_output(&recompile),
            temp_path.display()
        );
    }
}

#[test]
fn dsdt_iasl_roundtrip_handles_ecam_disabled_and_enabled_variants() {
    if !iasl_available() {
        eprintln!("skipping: `iasl` not found in PATH");
        return;
    }

    dsdt_iasl_roundtrip(&AcpiConfig::default(), "ecam-disabled");
    dsdt_iasl_roundtrip(
        &AcpiConfig {
            pcie_ecam_base: PCIE_ECAM_BASE,
            ..Default::default()
        },
        "ecam-enabled",
    );
}

#[test]
fn full_table_set_iasl_disassembly_smoke() {
    if !iasl_available() {
        eprintln!("skipping: `iasl` not found in PATH");
        return;
    }

    let placement = AcpiPlacement::default();

    // Disassemble every emitted table both with and without ECAM enabled. This
    // catches regressions where non-AML tables (FADT/MADT/HPET/MCFG, etc) become
    // malformed even if checksums still pass.
    for (label, cfg) in [
        ("ecam-disabled", AcpiConfig::default()),
        (
            "ecam-enabled",
            AcpiConfig {
                // Typical Q35 ECAM/MMCONFIG base; must be 1MiB aligned.
                pcie_ecam_base: PCIE_ECAM_BASE,
                pcie_segment: PCIE_ECAM_SEGMENT,
                pcie_start_bus: PCIE_ECAM_START_BUS,
                pcie_end_bus: PCIE_ECAM_END_BUS,
                ..Default::default()
            },
        ),
    ] {
        let tables = AcpiTables::build(&cfg, placement);
        if cfg.pcie_ecam_base != 0 {
            assert!(
                tables.mcfg.is_some(),
                "expected AcpiTables::build to emit MCFG when ECAM is enabled ({label})"
            );
        } else {
            assert!(
                tables.mcfg.is_none(),
                "did not expect AcpiTables::build to emit MCFG when ECAM is disabled ({label})"
            );
        }

        let tempdir = tempfile::Builder::new()
            .prefix(&format!("aero-acpi-iasl-tables-{label}-"))
            .tempdir()
            .expect("create tempdir");
        let temp_path = tempdir.path();

        let mut files: Vec<&'static str> = Vec::new();
        let mut write_table = |name: &'static str, bytes: &[u8]| {
            fs::write(temp_path.join(name), bytes)
                .unwrap_or_else(|err| panic!("write {label} {name}: {err}"));
            files.push(name);
        };

        // NOTE: `iasl -d` expects full table blobs including the SDT header.
        write_table("rsdt.dat", &tables.rsdt);
        write_table("xsdt.dat", &tables.xsdt);
        write_table("fadt.dat", &tables.fadt);
        write_table("madt.dat", &tables.madt);
        write_table("hpet.dat", &tables.hpet);
        if let Some(mcfg) = tables.mcfg.as_ref() {
            write_table("mcfg.dat", mcfg);
        }
        write_table("dsdt.dat", &tables.dsdt);

        iasl_disassemble_files(temp_path, &files);
    }
}
