use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

#[test]
fn dsdt_contains_pts_and_wak_methods() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let aml = &tables.dsdt[36..];

    assert!(
        aml.windows(4).any(|w| w == b"_PTS"),
        "expected DSDT AML to contain Method (_PTS, 1)"
    );
    assert!(
        aml.windows(4).any(|w| w == b"_WAK"),
        "expected DSDT AML to contain Method (_WAK, 1)"
    );
}

#[test]
fn wak_returns_two_zero_package() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let aml = &tables.dsdt[36..];

    // Look for the minimal encoding:
    //   Method (_WAK, 1) { Return (Package() { Zero, Zero }) }
    //
    // After the NameSeg `_WAK`, the method flags byte (0x01) should be followed by:
    //   ReturnOp (0xA4)
    //   PackageOp (0x12) PkgLength(0x04) NumElements(0x02) ZeroOp(0x00) ZeroOp(0x00)
    let needle = b"_WAK\x01\xA4\x12\x04\x02\x00\x00";
    assert!(
        aml.windows(needle.len()).any(|w| w == needle),
        "expected _WAK to return Package(){{0,0}} (needle: {needle:x?})"
    );
}

