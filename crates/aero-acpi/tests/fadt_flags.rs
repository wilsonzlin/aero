use aero_acpi::{
    AcpiConfig, AcpiPlacement, AcpiTables, FADT_FLAG_PWR_BUTTON, FADT_FLAG_RESET_REG_SUP,
};

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn fadt_flags_advertise_reset_register_and_fixed_feature_power_button() {
    let tables = AcpiTables::build(&AcpiConfig::default(), AcpiPlacement::default());
    let fadt = tables.fadt.as_slice();

    // ACPI 2.0+ FADT `Flags` field lives at offset 112.
    const FLAGS_OFFSET: usize = 112;
    let flags = read_u32_le(fadt, FLAGS_OFFSET);

    assert_ne!(
        flags & FADT_FLAG_RESET_REG_SUP,
        0,
        "FADT must advertise RESET_REG_SUP so OSes use RESET_REG/RESET_VALUE"
    );
    assert_ne!(
        flags & FADT_FLAG_PWR_BUTTON,
        0,
        "FADT must advertise PWR_BUTTON so OSes (e.g. Win7) use PM1_STS.PWRBTN_STS"
    );
}
