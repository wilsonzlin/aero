use aero_devices::acpi_pm::{
    AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, DEFAULT_ACPI_DISABLE, DEFAULT_ACPI_ENABLE,
    DEFAULT_GPE0_BLK, DEFAULT_GPE0_BLK_LEN, DEFAULT_PM1A_CNT_BLK, DEFAULT_PM1A_EVT_BLK,
    DEFAULT_PM_TMR_BLK, DEFAULT_SMI_CMD_PORT, PM1_CNT_SCI_EN,
};
use aero_devices::clock::ManualClock;
use aero_platform::io::PortIoDevice;
use firmware::acpi::{AcpiConfig, AcpiTables};

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[test]
fn guest_enable_and_disable_writes_toggle_sci_en() {
    // Firmware provides the handshake parameters via the FADT.
    let tables =
        AcpiTables::build(&AcpiConfig::new(1, 64 * 1024 * 1024)).expect("ACPI tables should build");
    let fadt = tables.fadt.as_slice();

    // ACPI fixed-feature blocks (offsets per ACPI 2.0 FADT layout).
    let smi_cmd_port = read_u32_le(fadt, 48) as u16;
    let acpi_enable_cmd = fadt[52];
    let acpi_disable_cmd = fadt[53];
    let pm1a_evt_blk = read_u32_le(fadt, 56) as u16;
    let pm1a_cnt_blk = read_u32_le(fadt, 64) as u16;
    let pm_tmr_blk = read_u32_le(fadt, 76) as u16;
    let gpe0_blk = read_u32_le(fadt, 80) as u16;
    let gpe0_len = fadt[92];

    // Ensure the table values match the device model defaults.
    assert_eq!(smi_cmd_port, DEFAULT_SMI_CMD_PORT);
    assert_eq!(acpi_enable_cmd, DEFAULT_ACPI_ENABLE);
    assert_eq!(acpi_disable_cmd, DEFAULT_ACPI_DISABLE);
    assert_eq!(pm1a_evt_blk, DEFAULT_PM1A_EVT_BLK);
    assert_eq!(pm1a_cnt_blk, DEFAULT_PM1A_CNT_BLK);
    assert_eq!(pm_tmr_blk, DEFAULT_PM_TMR_BLK);
    assert_eq!(gpe0_blk, DEFAULT_GPE0_BLK);
    assert_eq!(gpe0_len, DEFAULT_GPE0_BLK_LEN);

    let cfg = AcpiPmConfig {
        pm1a_evt_blk,
        pm1a_cnt_blk,
        pm_tmr_blk,
        gpe0_blk,
        gpe0_blk_len: gpe0_len,
        smi_cmd_port,
        acpi_enable_cmd,
        acpi_disable_cmd,
        start_enabled: false,
    };
    let clock = ManualClock::new();
    let mut pm = AcpiPmIo::new_with_callbacks_and_clock(cfg, AcpiPmCallbacks::default(), clock);

    assert_eq!(pm.read(cfg.pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);

    // Enable ACPI by writing `ACPI_ENABLE` to `SMI_CMD`.
    pm.write(cfg.smi_cmd_port, 1, cfg.acpi_enable_cmd as u32);
    assert_ne!(pm.read(cfg.pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);

    // Disable ACPI by writing `ACPI_DISABLE` to `SMI_CMD`.
    pm.write(cfg.smi_cmd_port, 1, cfg.acpi_disable_cmd as u32);
    assert_eq!(pm.read(cfg.pm1a_cnt_blk, 2) as u16 & PM1_CNT_SCI_EN, 0);
}
