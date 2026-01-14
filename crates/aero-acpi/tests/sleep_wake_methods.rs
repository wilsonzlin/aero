use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};

fn parse_pkg_length(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let b0 = *bytes.get(offset)?;
    let follow_bytes = (b0 >> 6) as usize;
    let mut len: usize = (b0 & 0x3F) as usize;
    for i in 0..follow_bytes {
        let b = *bytes.get(offset + 1 + i)?;
        len |= (b as usize) << (4 + i * 8);
    }
    Some((len, 1 + follow_bytes))
}

fn find_sb_scope_body_range(aml: &[u8]) -> Option<(usize, usize)> {
    // ScopeOp = 0x10, followed by PkgLength and NameString (we only handle NameSeg here).
    for i in 0..aml.len().saturating_sub(1) {
        if aml[i] != 0x10 {
            continue;
        }
        let (pkg_len, pkg_len_bytes) = parse_pkg_length(aml, i + 1)?;
        let payload_start = i + 1 + pkg_len_bytes;
        let obj_end = (i + 1).checked_add(pkg_len)?;
        if obj_end > aml.len() || payload_start + 4 > obj_end {
            continue;
        }
        if &aml[payload_start..payload_start + 4] != b"_SB_" {
            continue;
        }
        return Some((payload_start + 4, obj_end));
    }
    None
}

fn find_method_starts(aml: &[u8], name: &[u8; 4]) -> Vec<usize> {
    // MethodOp = 0x14, followed by PkgLength and NameSeg.
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 1 < aml.len() {
        if aml[i] != 0x14 {
            i += 1;
            continue;
        }
        let Some((pkg_len, pkg_len_bytes)) = parse_pkg_length(aml, i + 1) else {
            i += 1;
            continue;
        };
        let payload_start = i + 1 + pkg_len_bytes;
        let obj_end = (i + 1).saturating_add(pkg_len);
        if obj_end > aml.len() || payload_start + 4 > obj_end {
            i += 1;
            continue;
        }
        if &aml[payload_start..payload_start + 4] == name {
            out.push(i);
        }
        // Skip past the object to avoid quadratic scanning in long AML blobs.
        i = obj_end.max(i + 1);
    }
    out
}

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
fn pts_and_wak_are_not_emitted_under_sb_scope() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let aml = &tables.dsdt[36..];

    let (sb_body_start, sb_end) =
        find_sb_scope_body_range(aml).expect("expected DSDT AML to contain Scope (_SB_)");

    for (name, pretty) in [(b"_PTS", "_PTS"), (b"_WAK", "_WAK")] {
        let starts = find_method_starts(aml, name);
        assert!(
            !starts.is_empty(),
            "expected DSDT to emit a {pretty} method"
        );
        assert!(
            starts.iter().any(|&s| s < sb_body_start || s >= sb_end),
            "expected {pretty} to be emitted at root (outside Scope(_SB_)); found starts={starts:?} _SB_=[0x{sb_body_start:x}..0x{sb_end:x})"
        );
    }
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
