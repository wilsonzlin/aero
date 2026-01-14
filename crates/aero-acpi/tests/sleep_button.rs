use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn dsdt_contains_fixed_feature_sleep_button_device() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);

    // Skip the 36-byte SDT header; the rest is AML.
    let aml = &tables.dsdt[36..];

    assert!(
        contains_subslice(aml, b"SLPB"),
        "expected DSDT AML to contain SLPB device object"
    );

    // `Name (_HID, EisaId(\"PNP0C0E\"))` is encoded as:
    //   NameOp (0x08), \"_HID\", DWordConst (0x0C), <EISA ID as u32 little-endian>
    let pnp0c0e = 0x0E0C_D041u32.to_le_bytes();
    let hid_pnp0c0e = [&[0x08][..], &b"_HID"[..], &[0x0C][..], &pnp0c0e[..]].concat();
    assert!(
        contains_subslice(aml, &hid_pnp0c0e),
        "expected DSDT AML to contain EISA ID encoding for PNP0C0E (sleep button)"
    );
}

