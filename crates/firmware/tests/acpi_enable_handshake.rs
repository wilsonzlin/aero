use aero_devices::acpi_pm::{AcpiPmConfig, AcpiPmIo, PM1_CNT_SCI_EN};
use aero_platform::io::PortIoDevice;
use firmware::acpi::build_acpi_table_set;

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn guest_enable_write_sets_sci_en() {
    // Firmware provides the handshake parameters via FADT.
    let tables = build_acpi_table_set(0x1000);
    let fadt = tables.fadt;

    // Devices are configured to match the same FADT values.
    let smi_cmd_port = read_u32_le(&fadt, 48) as u16;
    let acpi_enable_cmd = fadt[52];
    let acpi_disable_cmd = fadt[53];
    let pm1a_evt_blk = read_u32_le(&fadt, 56) as u16;
    let pm1a_cnt_blk = read_u32_le(&fadt, 64) as u16;

    let cfg = AcpiPmConfig {
        pm1a_evt_blk,
        pm1a_cnt_blk,
        smi_cmd_port,
        acpi_enable_cmd,
        acpi_disable_cmd,
        start_enabled: false,
        ..AcpiPmConfig::default()
    };
    let mut pm = AcpiPmIo::new(cfg);

    assert_eq!(pm.read(cfg.pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);

    // "Guest payload": enable ACPI by writing `ACPI_ENABLE` to `SMI_CMD`.
    pm.write(cfg.smi_cmd_port, 1, cfg.acpi_enable_cmd as u32);

    assert_ne!(pm.read(cfg.pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);
}
