use std::{env, fs, path::PathBuf};

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

const UPDATE_ENV: &str = "AERO_UPDATE_ACPI_FIXTURES";

fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn first_diff_index(a: &[u8], b: &[u8]) -> Option<usize> {
    let min_len = a.len().min(b.len());
    for i in 0..min_len {
        if a[i] != b[i] {
            return Some(i);
        }
    }
    if a.len() != b.len() {
        return Some(min_len);
    }
    None
}

#[test]
fn dsdt_matches_firmware_fixture() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let generated = tables.dsdt;

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../firmware/acpi/dsdt.aml");
    let fixture = fs::read(&fixture_path).expect("read DSDT fixture (crates/firmware/acpi/dsdt.aml)");

    if generated == fixture {
        return;
    }

    if env::var_os(UPDATE_ENV).is_some() {
        fs::write(&fixture_path, &generated)
            .expect("write updated DSDT fixture (crates/firmware/acpi/dsdt.aml)");
        eprintln!("updated DSDT fixture at {}", fixture_path.display());
        return;
    }

    let idx = first_diff_index(&generated, &fixture);
    let diff = idx.map(|i| {
        let g = generated.get(i).copied();
        let f = fixture.get(i).copied();
        let fmt = |v: Option<u8>| match v {
            Some(b) => format!("0x{b:02x}"),
            None => "<eof>".to_string(),
        };
        format!(
            "first difference at byte {i}: generated={} fixture={}",
            fmt(g),
            fmt(f)
        )
    });

    panic!(
        "generated DSDT does not match fixture: {}\n  generated: len={} fnv1a64=0x{:016x}\n  fixture:   len={} fnv1a64=0x{:016x}\n  {}\n\nTo update the fixture, re-run with {UPDATE_ENV}=1",
        fixture_path.display(),
        generated.len(),
        fnv1a64(&generated),
        fixture.len(),
        fnv1a64(&fixture),
        diff.unwrap_or_else(|| "no byte-level difference found (unexpected)".to_string()),
    );
}
