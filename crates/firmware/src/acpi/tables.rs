use super::{aml, checksum, dsdt};

#[derive(Debug)]
pub struct BuiltAcpiTables {
    pub base_address: u64,

    pub dsdt_address: u64,
    pub dsdt: Vec<u8>,

    pub fadt_address: u64,
    pub fadt: Vec<u8>,

    pub rsdt_address: u64,
    pub rsdt: Vec<u8>,

    pub xsdt_address: u64,
    pub xsdt: Vec<u8>,

    pub rsdp: Vec<u8>,
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

fn build_header(signature: &[u8; 4], revision: u8, body_len: usize) -> [u8; 36] {
    let mut hdr = [0u8; 36];
    hdr[0..4].copy_from_slice(signature);
    hdr[4..8].copy_from_slice(&((36 + body_len) as u32).to_le_bytes());
    hdr[8] = revision;
    hdr[9] = 0; // checksum patched later
    hdr[10..16].copy_from_slice(b"AERO  "); // OEMID (6)
    hdr[16..24].copy_from_slice(b"AEROTBL "); // OEM Table ID (8)
    hdr[24..28].copy_from_slice(&1u32.to_le_bytes()); // OEM Revision
    hdr[28..32].copy_from_slice(b"AERO"); // Creator ID
    hdr[32..36].copy_from_slice(&1u32.to_le_bytes()); // Creator Revision
    hdr
}

fn finalize_checksum(table: &mut [u8]) {
    debug_assert!(table.len() >= 36);
    table[9] = 0;
    table[9] = checksum::generate_checksum_byte(table);
    debug_assert_eq!(checksum::acpi_checksum(table), 0);
}

fn build_fadt(dsdt_address: u64) -> Vec<u8> {
    // ACPI 2.0+ FADT (rev 3, 244 bytes total).
    const BODY_LEN: usize = 244 - 36;
    let header = build_header(b"FACP", 3, BODY_LEN);

    let mut table = Vec::new();
    table.extend_from_slice(&header);
    table.resize(244, 0);

    // firmware_ctrl (u32) @ 36
    // dsdt (u32) @ 40
    table[40..44].copy_from_slice(&(dsdt_address as u32).to_le_bytes());

    // Preferred power management profile @ 45. Leave 0 (unspecified).
    // SCI interrupt @ 46 (u16). Use the traditional ISA IRQ 9.
    table[46..48].copy_from_slice(&9u16.to_le_bytes());

    // 64-bit pointers added in ACPI 2.0+.
    // x_firmware_ctrl (u64) @ 132
    // x_dsdt (u64) @ 140
    table[140..148].copy_from_slice(&dsdt_address.to_le_bytes());

    finalize_checksum(&mut table);
    table
}

fn build_rsdt(fadt_address: u64) -> Vec<u8> {
    let body_len = 4; // one u32 entry
    let header = build_header(b"RSDT", 1, body_len);

    let mut table = Vec::new();
    table.extend_from_slice(&header);
    table.extend_from_slice(&(fadt_address as u32).to_le_bytes());
    finalize_checksum(&mut table);
    table
}

fn build_xsdt(fadt_address: u64) -> Vec<u8> {
    let body_len = 8; // one u64 entry
    let header = build_header(b"XSDT", 1, body_len);

    let mut table = Vec::new();
    table.extend_from_slice(&header);
    table.extend_from_slice(&fadt_address.to_le_bytes());
    finalize_checksum(&mut table);
    table
}

fn build_rsdp(rsdt_address: u64, xsdt_address: u64) -> Vec<u8> {
    // ACPI 2.0+ RSDP is 36 bytes.
    let mut rsdp = [0u8; 36];
    rsdp[0..8].copy_from_slice(b"RSD PTR ");
    rsdp[8] = 0; // checksum (patched)
    rsdp[9..15].copy_from_slice(b"AERO  "); // OEMID
    rsdp[15] = 2; // revision
    rsdp[16..20].copy_from_slice(&(rsdt_address as u32).to_le_bytes());
    rsdp[20..24].copy_from_slice(&(36u32).to_le_bytes()); // length
    rsdp[24..32].copy_from_slice(&xsdt_address.to_le_bytes());
    rsdp[32] = 0; // extended checksum (patched)

    // Checksum for the first 20 bytes.
    rsdp[8] = checksum::generate_checksum_byte(&rsdp[0..20]);
    debug_assert_eq!(checksum::acpi_checksum(&rsdp[0..20]), 0);

    // Extended checksum for the entire structure.
    rsdp[32] = checksum::generate_checksum_byte(&rsdp);
    debug_assert_eq!(checksum::acpi_checksum(&rsdp), 0);

    rsdp.to_vec()
}

pub fn build_acpi_table_set(base_address: u64) -> BuiltAcpiTables {
    let dsdt_bytes = dsdt::DSDT_AML.to_vec();

    let dsdt_address = base_address;
    let mut cursor = base_address + dsdt_bytes.len() as u64;
    cursor = align_up(cursor, 16);

    let fadt_address = cursor;
    let fadt_bytes = build_fadt(dsdt_address);
    cursor += fadt_bytes.len() as u64;
    cursor = align_up(cursor, 16);

    let rsdt_address = cursor;
    let rsdt_bytes = build_rsdt(fadt_address);
    cursor += rsdt_bytes.len() as u64;
    cursor = align_up(cursor, 16);

    let xsdt_address = cursor;
    let xsdt_bytes = build_xsdt(fadt_address);

    let rsdp_bytes = build_rsdp(rsdt_address, xsdt_address);

    BuiltAcpiTables {
        base_address,
        dsdt_address,
        dsdt: dsdt_bytes,
        fadt_address,
        fadt: fadt_bytes,
        rsdt_address,
        rsdt: rsdt_bytes,
        xsdt_address,
        xsdt: xsdt_bytes,
        rsdp: rsdp_bytes,
    }
}

fn parse_integer(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    match *bytes.get(offset)? {
        aml::AML_OP_ZERO => Some((0, 1)),
        aml::AML_OP_ONE => Some((1, 1)),
        aml::AML_OP_BYTE_PREFIX => Some((*bytes.get(offset + 1)? as u64, 2)),
        aml::AML_OP_WORD_PREFIX => Some((
            u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?) as u64,
            3,
        )),
        aml::AML_OP_DWORD_PREFIX => Some((
            u32::from_le_bytes(bytes.get(offset + 1..offset + 5)?.try_into().ok()?) as u64,
            5,
        )),
        aml::AML_OP_QWORD_PREFIX => Some((
            u64::from_le_bytes(bytes.get(offset + 1..offset + 9)?.try_into().ok()?),
            9,
        )),
        _ => None,
    }
}

/// Parse the static `_PRT` package we emit in the clean-room DSDT.
///
/// Returns a list of entries: (PCI address, pin, GSI).
pub fn parse_prt_entries(aml_body: &[u8]) -> Option<Vec<(u32, u8, u32)>> {
    // Look for: NameOp + NameSeg("_PRT")
    let mut prt_off = None;
    for i in 0..aml_body.len().saturating_sub(5) {
        if aml_body[i] == aml::AML_OP_NAME && &aml_body[i + 1..i + 5] == b"_PRT" {
            prt_off = Some(i);
            break;
        }
    }
    let prt_off = prt_off?;

    let mut offset = prt_off + 1 + 4;
    if *aml_body.get(offset)? != aml::AML_OP_PACKAGE {
        return None;
    }
    offset += 1;

    let (pkg_len, pkg_len_bytes) = aml::parse_pkg_length(aml_body, offset)?;
    offset += pkg_len_bytes;
    let pkg_end = offset + pkg_len;

    let count = *aml_body.get(offset)? as usize;
    offset += 1;

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if *aml_body.get(offset)? != aml::AML_OP_PACKAGE {
            return None;
        }
        offset += 1;
        let (entry_len, entry_len_bytes) = aml::parse_pkg_length(aml_body, offset)?;
        offset += entry_len_bytes;
        let entry_end = offset + entry_len;

        let entry_count = *aml_body.get(offset)? as usize;
        if entry_count != 4 {
            return None;
        }
        offset += 1;

        let (addr, addr_bytes) = parse_integer(aml_body, offset)?;
        offset += addr_bytes;

        let (pin, pin_bytes) = parse_integer(aml_body, offset)?;
        offset += pin_bytes;

        // Source (we always emit Zero)
        let (source, source_bytes) = parse_integer(aml_body, offset)?;
        offset += source_bytes;
        if source != 0 {
            return None;
        }

        let (gsi, gsi_bytes) = parse_integer(aml_body, offset)?;
        offset += gsi_bytes;

        if offset != entry_end {
            return None;
        }

        out.push((addr as u32, pin as u8, gsi as u32));
    }

    if offset != pkg_end {
        return None;
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
    }

    #[test]
    fn fadt_references_dsdt_and_checksums_are_valid() {
        let tables = build_acpi_table_set(0x1000);

        // Layout is derived from DSDT length.
        assert_eq!(tables.dsdt_address, 0x1000);
        assert_eq!(
            tables.fadt_address,
            align_up(0x1000 + (tables.dsdt.len() as u64), 16)
        );

        assert_eq!(&tables.fadt[0..4], b"FACP");
        assert_eq!(checksum::acpi_checksum(&tables.fadt), 0);

        // dsdt (u32) @ 40, x_dsdt (u64) @ 140
        assert_eq!(read_u32_le(&tables.fadt, 40) as u64, tables.dsdt_address);
        assert_eq!(read_u64_le(&tables.fadt, 140), tables.dsdt_address);

        assert_eq!(&tables.rsdt[0..4], b"RSDT");
        assert_eq!(checksum::acpi_checksum(&tables.rsdt), 0);
        assert_eq!(read_u32_le(&tables.rsdt, 36) as u64, tables.fadt_address);

        assert_eq!(&tables.xsdt[0..4], b"XSDT");
        assert_eq!(checksum::acpi_checksum(&tables.xsdt), 0);
        assert_eq!(read_u64_le(&tables.xsdt, 36), tables.fadt_address);

        // RSDP checksums are verified in builder via debug_asserts; still sanity check signature.
        assert_eq!(&tables.rsdp[0..8], b"RSD PTR ");
        assert_eq!(checksum::acpi_checksum(&tables.rsdp[0..20]), 0);
        assert_eq!(checksum::acpi_checksum(&tables.rsdp), 0);
    }
}

