use std::{
    ffi::OsStr,
    fs,
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

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
        if path.extension().and_then(OsStr::to_str).is_some_and(|ext| ext.eq_ignore_ascii_case("dsl"))
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

#[test]
fn dsdt_iasl_roundtrip() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let temp_path = tempdir.path();

    // `iasl -d` expects a full ACPI table blob (header + AML).
    let dsdt_aml_path = temp_path.join("dsdt.aml");
    fs::write(&dsdt_aml_path, &tables.dsdt).expect("write dsdt.aml");

    // Disassemble DSDT AML -> DSL. If `iasl` is not available, skip this test
    // (we don't want to make CI dependent on ACPICA being installed).
    let disasm = match run_iasl(temp_path, &["-d", "dsdt.aml"]) {
        Ok(out) => out,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipping: `iasl` not found in PATH");
            return;
        }
        Err(err) => panic!("failed to spawn `iasl -d dsdt.aml`: {err}"),
    };
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
    let recompile = run_iasl(
        temp_path,
        &[
            "-tc",
            "-p",
            "dsdt_recompiled",
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
            "`iasl -tc -p dsdt_recompiled ...` failed\n{}\n(temp dir: {})",
            fmt_output(&recompile),
            temp_path.display()
        );
    }
}

