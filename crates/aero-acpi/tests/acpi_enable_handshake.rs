use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_CNT_SCI_EN};
use aero_devices::clock::ManualClock;
use aero_platform::io::PortIoDevice;

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn smi_cmd_enable_write_sets_sci_en() {
    let cfg = AcpiConfig::default();
    let placement = AcpiPlacement::default();
    let tables = AcpiTables::build(&cfg, placement);
    let fadt = tables.fadt.as_slice();

    let smi_cmd_port = read_u32_le(fadt, 48) as u16;
    let acpi_enable_cmd = fadt[52];
    let acpi_disable_cmd = fadt[53];
    let pm1a_evt_blk = read_u32_le(fadt, 56) as u16;
    let pm1a_cnt_blk = read_u32_le(fadt, 64) as u16;

    let clock = ManualClock::new();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(
        AcpiPmConfig {
            pm1a_evt_blk,
            pm1a_cnt_blk,
            smi_cmd_port,
            acpi_enable_cmd,
            acpi_disable_cmd,
            start_enabled: false,
            ..AcpiPmConfig::default()
        },
        AcpiPmCallbacks::default(),
        clock,
    );

    assert_eq!(pm.read(pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);

    // Guest performs the standard ACPI handshake: write ACPI_ENABLE to SMI_CMD.
    pm.write(smi_cmd_port, 1, acpi_enable_cmd as u32);
    assert_ne!(pm.read(pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);

    // ACPI_DISABLE should clear SCI_EN again.
    pm.write(smi_cmd_port, 1, acpi_disable_cmd as u32);
    assert_eq!(pm.read(pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);
}
