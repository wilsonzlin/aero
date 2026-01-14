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

fn parse_integer(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    match *bytes.get(offset)? {
        0x00 => Some((0, 1)),                              // ZeroOp
        0x01 => Some((1, 1)),                              // OneOp
        0x0A => Some((*bytes.get(offset + 1)? as u64, 2)), // BytePrefix
        0x0B => Some((
            u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?) as u64,
            3,
        )), // WordPrefix
        0x0C => Some((
            u32::from_le_bytes(bytes.get(offset + 1..offset + 5)?.try_into().ok()?) as u64,
            5,
        )), // DWordPrefix
        0x0E => Some((
            u64::from_le_bytes(bytes.get(offset + 1..offset + 9)?.try_into().ok()?),
            9,
        )), // QWordPrefix
        _ => None,
    }
}

#[derive(Debug)]
struct SleepPackage {
    values: [u64; 2],
    raw_element_bytes: Vec<u8>,
}

fn find_sleep_package(aml: &[u8], name: &[u8; 4]) -> Option<SleepPackage> {
    // NameOp (0x08), NameSeg, PackageOp (0x12), PkgLength, NumElements, elements...
    for i in 0..aml.len().saturating_sub(5) {
        if aml[i] != 0x08 || &aml[i + 1..i + 5] != name {
            continue;
        }

        let mut offset = i + 5;
        if *aml.get(offset)? != 0x12 {
            continue;
        }
        offset += 1;

        let (pkg_len, pkg_len_bytes) = parse_pkg_length(aml, offset)?;
        offset += pkg_len_bytes;
        let payload_len = pkg_len.checked_sub(pkg_len_bytes)?;
        let payload_end = offset.checked_add(payload_len)?;
        if payload_end > aml.len() {
            continue;
        }

        let element_count = *aml.get(offset)? as usize;
        offset += 1;
        if element_count != 2 {
            continue;
        }
        let elements_start = offset;

        let (v1, v1_bytes) = parse_integer(aml, offset)?;
        offset += v1_bytes;
        let (v2, v2_bytes) = parse_integer(aml, offset)?;
        offset += v2_bytes;

        if offset != payload_end {
            continue;
        }

        return Some(SleepPackage {
            values: [v1, v2],
            raw_element_bytes: aml[elements_start..payload_end].to_vec(),
        });
    }

    None
}

#[test]
fn dsdt_advertises_sleep_states_s1_s3_s4_s5_with_expected_slp_typ_values() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let aml = &tables.dsdt[36..];

    let cases: [(&[u8; 4], u64, &[u8]); 4] = [
        (b"_S1_", 1, &[0x01, 0x01]),
        (b"_S3_", 3, &[0x0A, 0x03, 0x0A, 0x03]),
        (b"_S4_", 4, &[0x0A, 0x04, 0x0A, 0x04]),
        (b"_S5_", 5, &[0x0A, 0x05, 0x0A, 0x05]),
    ];

    for (name, expected, raw_elements) in cases {
        let pkg = find_sleep_package(aml, name)
            .unwrap_or_else(|| panic!("missing or malformed sleep state package {name:?}"));
        assert_eq!(
            pkg.values,
            [expected, expected],
            "{name:?} package should contain {{0x{expected:02x}, 0x{expected:02x}}}"
        );
        assert_eq!(
            pkg.raw_element_bytes.as_slice(),
            raw_elements,
            "{name:?} package element encoding mismatch"
        );
    }
}
