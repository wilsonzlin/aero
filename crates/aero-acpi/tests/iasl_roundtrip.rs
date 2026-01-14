use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_pc_constants::PCIE_ECAM_BASE;

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

fn extract_braced_block<'a>(src: &'a str, keyword: &str) -> Option<&'a str> {
    // Find the first occurrence of `keyword`, then return the `{ ... }` block that immediately
    // follows it (with nested brace matching).
    let start = src.find(keyword)?;
    let after = &src[start..];
    let brace_start_rel = after.find('{')?;
    let brace_start = start + brace_start_rel;

    let mut depth = 0usize;
    for (idx_rel, ch) in src[brace_start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = brace_start + idx_rel + 1;
                    return Some(&src[brace_start..end]);
                }
            }
            _ => {}
        }
    }

    None
}

fn disassemble_dsdt_to_dsl(cfg: &AcpiConfig, label: &str) -> String {
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(cfg, placement);

    let tempdir = tempfile::Builder::new()
        .prefix(&format!("aero-acpi-iasl-dsl-{label}-"))
        .tempdir()
        .expect("create tempdir");
    let temp_path = tempdir.path();

    let dsdt_aml_path = temp_path.join("dsdt.aml");
    fs::write(&dsdt_aml_path, &tables.dsdt).expect("write dsdt.aml");

    let disasm = run_iasl(temp_path, &["-d", "dsdt.aml"])
        .unwrap_or_else(|err| panic!("failed to spawn `iasl -d dsdt.aml`: {err}"));
    if !disasm.status.success() {
        panic!(
            "`iasl -d dsdt.aml` failed\n{}\n(temp dir: {})",
            fmt_output(&disasm),
            temp_path.display()
        );
    }

    let dsdt_dsl = find_generated_dsl(temp_path).expect("locate dsdt.dsl");
    fs::read_to_string(dsdt_dsl).expect("read dsdt.dsl")
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
fn dsdt_iasl_disassembly_shows_pci0_mmio_as_cacheable_readwrite_resource_producer() {
    if !iasl_available() {
        eprintln!("skipping: `iasl` not found in PATH");
        return;
    }

    for (label, cfg) in [
        ("ecam-disabled", AcpiConfig::default()),
        (
            "ecam-enabled",
            AcpiConfig {
                pcie_ecam_base: PCIE_ECAM_BASE,
                ..Default::default()
            },
        ),
    ] {
        let dsl = disassemble_dsdt_to_dsl(&cfg, label);
        let pci0_block = extract_braced_block(&dsl, "Device (PCI0)")
            .unwrap_or_else(|| panic!("failed to locate PCI0 device block ({label})"));

        // The PCI root bridge windows should disassemble as ResourceProducer for correct
        // OS resource allocation.
        assert!(
            pci0_block.contains("WordBusNumber (ResourceProducer"),
            "expected PCI0 bus window to disassemble as ResourceProducer ({label})"
        );
        assert!(
            pci0_block.contains("WordIO (ResourceProducer"),
            "expected PCI0 I/O windows to disassemble as ResourceProducer ({label})"
        );
        assert!(
            pci0_block.contains("DWordMemory (ResourceProducer"),
            "expected PCI0 MMIO window to disassemble as ResourceProducer ({label})"
        );

        // The MMIO window must be ReadWrite (not ReadOnly) for Windows 7 PCI resource allocation
        // correctness.
        assert!(
            pci0_block.contains("Cacheable, ReadWrite"),
            "expected PCI0 MMIO window to disassemble as Cacheable, ReadWrite ({label})"
        );
    }
}
