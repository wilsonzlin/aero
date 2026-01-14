use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn dsdt_contains_fixed_feature_power_button_device() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);

    // Skip the 36-byte SDT header; the rest is AML.
    let aml = &tables.dsdt[36..];

    assert!(
        contains_subslice(aml, b"PWRB"),
        "expected DSDT AML to contain PWRB device object"
    );

    // `Name (_HID, EisaId(\"PNP0C0C\"))` is encoded as:
    //   NameOp (0x08), \"_HID\", DWordConst (0x0C), <EISA ID as u32 little-endian>
    let pnp0c0c = 0x0C0C_D041u32.to_le_bytes();
    let hid_pnp0c0c = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0c0c[..]].concat();
    assert!(
        contains_subslice(aml, &hid_pnp0c0c),
        "expected DSDT AML to contain EISA ID encoding for PNP0C0C (power button)"
    );
}

