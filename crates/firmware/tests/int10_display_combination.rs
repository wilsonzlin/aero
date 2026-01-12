use firmware::{
    bios::Bios,
    cpu::CpuState,
    memory::VecMemory,
    rtc::{CmosRtc, DateTime},
};

#[test]
fn int10_get_display_combination_code_reports_vga_color() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState {
        rax: 0x1234_5678_0000_0000,
        rbx: 0xFEDC_BA98_0000_0000,
        ..Default::default()
    };
    cpu.set_ax(0x1A00); // AH=1Ah, AL=00h

    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(cpu.al(), 0x1A);
    assert_eq!(cpu.bl(), 0x08);
    assert_eq!(cpu.bh(), 0x00);

    // Ensure we only modified the documented low bytes.
    assert_eq!(cpu.rax & !0xFFFF, 0x1234_5678_0000_0000);
    assert_eq!(cpu.rbx & !0xFFFF, 0xFEDC_BA98_0000_0000);
}
