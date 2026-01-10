use super::{aml, checksum};

/// HPET base address (required by Windows for the `PNP0103` device).
pub const HPET_BASE: u32 = 0xFED0_0000;
pub const HPET_LENGTH: u32 = 0x400;

/// ISA IRQ routing used by the emulator's PCI INTx swizzle:
/// PIRQ A-D → GSIs 10, 11, 12, 13.
pub const PCI_PIRQ_GSI: [u32; 4] = [10, 11, 12, 13];

pub const DSDT_AML: &[u8] = include_bytes!("../../acpi/dsdt.aml");

pub fn pci_intx_gsi(device: u8, pin: u8) -> u32 {
    let idx = ((device as usize) + (pin as usize)) & 3;
    PCI_PIRQ_GSI[idx]
}

fn hpet_crs_bytes() -> [u8; 14] {
    // Memory32Fixed(ReadWrite, 0xFED00000, 0x400)
    // 0x86 = Memory32Fixed (large), length=9
    // Writeable=1, base, length
    // EndTag
    [
        0x86,
        0x09,
        0x00,
        0x01,
        0x00,
        0x00,
        0xD0,
        0xFE,
        0x00,
        0x04,
        0x00,
        0x00,
        0x79,
        0x00,
    ]
}

fn rtc_crs_bytes() -> [u8; 13] {
    // IO(Decode16, 0x70, 0x70, 1, 2) + IRQNoFlags {8} + EndTag
    [
        0x47, 0x01, 0x70, 0x00, 0x70, 0x00, 0x01, 0x02, // IO
        0x22, 0x00, 0x01, // IRQ (bit 8)
        0x79, 0x00, // EndTag
    ]
}

fn timr_crs_bytes() -> [u8; 13] {
    // IO(Decode16, 0x40, 0x40, 1, 4) + IRQNoFlags {0} + EndTag
    [
        0x47, 0x01, 0x40, 0x00, 0x40, 0x00, 0x01, 0x04, // IO
        0x22, 0x01, 0x00, // IRQ (bit 0)
        0x79, 0x00, // EndTag
    ]
}

fn build_prt() -> Vec<u8> {
    let mut entries = Vec::new();
    for dev in 1u8..=31 {
        let addr = ((dev as u32) << 16) | 0xFFFF;
        for pin in 0u8..=3 {
            let gsi = pci_intx_gsi(dev, pin);
            entries.push(aml::op_package(vec![
                aml::op_integer(addr as u64),
                aml::op_integer(pin as u64),
                aml::op_integer(0),
                aml::op_integer(gsi as u64),
            ]));
        }
    }

    aml::op_package(entries)
}

fn build_aml_body() -> Vec<u8> {
    let mut root_objects = Vec::new();

    root_objects.extend_from_slice(&aml::op_name(
        "_S5",
        aml::op_package(vec![aml::op_integer(0x05), aml::op_integer(0x05)]),
    ));

    root_objects.extend_from_slice(&aml::op_name("PICM", aml::op_integer(0)));

    root_objects.extend_from_slice(&aml::op_method(
        "_PIC",
        1,
        false,
        aml::op_store(vec![aml::AML_OP_ARG0], "PICM"),
    ));

    let mut sb = Vec::new();

    // \_SB_.PCI0 (root PCI bus)
    let mut pci0 = Vec::new();
    pci0.extend_from_slice(&aml::op_name("_HID", aml::op_string("PNP0A03")));
    pci0.extend_from_slice(&aml::op_name("_UID", aml::op_integer(0)));
    pci0.extend_from_slice(&aml::op_name("_ADR", aml::op_integer(0)));
    pci0.extend_from_slice(&aml::op_name("_PRT", build_prt()));
    sb.extend_from_slice(&aml::op_device("PCI0", pci0));

    // \_SB_.HPET
    let mut hpet = Vec::new();
    hpet.extend_from_slice(&aml::op_name("_HID", aml::op_string("PNP0103")));
    hpet.extend_from_slice(&aml::op_name("_UID", aml::op_integer(0)));
    hpet.extend_from_slice(&aml::op_name("_CRS", aml::op_buffer(&hpet_crs_bytes())));
    sb.extend_from_slice(&aml::op_device("HPET", hpet));

    // \_SB_.RTC
    let mut rtc = Vec::new();
    rtc.extend_from_slice(&aml::op_name("_HID", aml::op_string("PNP0B00")));
    rtc.extend_from_slice(&aml::op_name("_UID", aml::op_integer(0)));
    rtc.extend_from_slice(&aml::op_name("_CRS", aml::op_buffer(&rtc_crs_bytes())));
    sb.extend_from_slice(&aml::op_device("RTC", rtc));

    // \_SB_.TIMR
    let mut timr = Vec::new();
    timr.extend_from_slice(&aml::op_name("_HID", aml::op_string("PNP0100")));
    timr.extend_from_slice(&aml::op_name("_UID", aml::op_integer(0)));
    timr.extend_from_slice(&aml::op_name("_CRS", aml::op_buffer(&timr_crs_bytes())));
    sb.extend_from_slice(&aml::op_device("TIMR", timr));

    root_objects.extend_from_slice(&aml::op_scope("_SB_", sb));

    root_objects
}

pub fn generate_dsdt_aml() -> Vec<u8> {
    let aml = build_aml_body();

    let mut table = Vec::new();
    table.extend_from_slice(b"DSDT");
    table.extend_from_slice(&0u32.to_le_bytes()); // length (patched later)
    table.push(2); // revision
    table.push(0); // checksum (patched later)
    table.extend_from_slice(b"AERO  "); // OEMID (6)
    table.extend_from_slice(b"AERODSDT"); // OEM Table ID (8)
    table.extend_from_slice(&1u32.to_le_bytes()); // OEM revision
    table.extend_from_slice(&u32::from_le_bytes(*b"AERO").to_le_bytes()); // Creator ID
    table.extend_from_slice(&1u32.to_le_bytes()); // Creator revision

    debug_assert_eq!(table.len(), 36);
    table.extend_from_slice(&aml);

    let len = table.len() as u32;
    table[4..8].copy_from_slice(&len.to_le_bytes());

    let checksum_byte = checksum::generate_checksum_byte(&table);
    table[9] = checksum_byte;

    debug_assert_eq!(checksum::acpi_checksum(&table), 0);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    #[test]
    fn embedded_dsdt_has_valid_header_and_checksum() {
        assert!(DSDT_AML.len() >= 36);
        assert_eq!(&DSDT_AML[0..4], b"DSDT");

        let len = read_u32_le(DSDT_AML, 4) as usize;
        assert_eq!(len, DSDT_AML.len());
        assert_eq!(checksum::acpi_checksum(DSDT_AML), 0);
    }

    #[test]
    fn embedded_dsdt_matches_generator() {
        let generated = generate_dsdt_aml();
        assert_eq!(DSDT_AML, generated.as_slice());
    }

    #[test]
    fn embedded_dsdt_prt_matches_routing_constants() {
        let aml_body = &DSDT_AML[36..];

        let mut expected = Vec::new();
        for dev in 1u8..=31 {
            let addr = ((dev as u32) << 16) | 0xFFFF;
            for pin in 0u8..=3 {
                expected.push((addr, pin, pci_intx_gsi(dev, pin)));
            }
        }

        let entries = super::super::tables::parse_prt_entries(aml_body)
            .expect("failed to parse _PRT entries from AML");

        assert_eq!(entries.len(), expected.len());
        for (got, exp) in entries.iter().zip(expected.iter()) {
            assert_eq!(
                got,
                exp,
                "_PRT mismatch: got={got:?} expected={exp:?}"
            );
        }
    }
}
