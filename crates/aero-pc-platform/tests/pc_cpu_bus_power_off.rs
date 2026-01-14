use aero_cpu_core::mem::CpuBus as _;
use aero_devices::acpi_pm::SLP_TYP_S5;
use aero_pc_platform::{PcCpuBus, PcPlatform, ResetEvent};

#[test]
fn pc_cpu_bus_fetch_does_not_treat_acpi_s5_as_reset() {
    let mut bus = PcCpuBus::new(PcPlatform::new(2 * 1024 * 1024));

    // Place a known instruction byte in guest RAM so we can prove fetch still works.
    bus.platform.memory.write_u8(0, 0x90); // NOP

    let pm1a_cnt_blk = bus.platform.acpi_pm.borrow().cfg().pm1a_cnt_blk;

    // ACPI PM1a_CNT encoding:
    // - SLP_TYP in bits 10..=12
    // - SLP_EN in bit 13
    const PM1_CNT_SLP_TYP_SHIFT: u16 = 10;
    const PM1_CNT_SLP_EN: u16 = 1 << 13;
    let s5_request = (u16::from(SLP_TYP_S5) << PM1_CNT_SLP_TYP_SHIFT) | PM1_CNT_SLP_EN;

    bus.platform
        .io
        .write(pm1a_cnt_blk, 2, u32::from(s5_request));

    // The power-off request should not trip the CPU-bus reset fast-path (which is intended for
    // CPU/system resets).
    let bytes = bus.fetch(0, 1).expect("fetch should succeed");
    assert_eq!(bytes[0], 0x90);

    // The power-off request is still observable via the platform event queue.
    assert_eq!(bus.platform.take_reset_events(), vec![ResetEvent::PowerOff]);
}
