use aero_devices::acpi_pm::SLP_TYP_S5;
use aero_pc_platform::{PcPlatform, ResetEvent};

#[test]
fn pc_platform_records_power_off_event_on_acpi_s5_request() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    let pm1a_cnt_blk = pc.acpi_pm.borrow().cfg().pm1a_cnt_blk;

    // ACPI PM1a_CNT encoding:
    // - SLP_TYP in bits 10..=12
    // - SLP_EN in bit 13
    const PM1_CNT_SLP_TYP_SHIFT: u16 = 10;
    const PM1_CNT_SLP_EN: u16 = 1 << 13;

    let s5_request = (u16::from(SLP_TYP_S5) << PM1_CNT_SLP_TYP_SHIFT) | PM1_CNT_SLP_EN;

    pc.io.write(pm1a_cnt_blk, 2, u32::from(s5_request));
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::PowerOff]);

    // Power-off requests must be cleared by a platform reset so they don't persist across boots.
    pc.io.write(pm1a_cnt_blk, 2, u32::from(s5_request));
    pc.reset();
    assert!(pc.take_reset_events().is_empty());
}
