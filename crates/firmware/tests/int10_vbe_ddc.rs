use emulator::devices::vga::vbe;
use firmware::{
    bios::Bios,
    cpu::CpuState,
    memory::{MemoryBus, VecMemory},
    rtc::DateTime,
};

#[test]
fn int10_vbe_ddc_copies_edid_block() {
    let expected = vbe::read_edid(0).expect("missing base EDID");

    let mut bios = Bios::new(firmware::rtc::CmosRtc::new(DateTime::new(
        2026, 1, 1, 0, 0, 0,
    )));
    let mut memory = VecMemory::new(0x100000);
    bios.init(&mut memory);

    let mut cpu = CpuState::default();
    cpu.set_ax(0x4F15);
    cpu.set_bl(0x01);
    cpu.set_dx(0);
    cpu.set_es(0x2000);
    cpu.set_di(0x0100);

    let addr = ((cpu.es() as u64) << 4) + (cpu.di() as u64);
    bios.handle_int10_vbe(&mut cpu, &mut memory);

    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let actual: Vec<u8> = (0..expected.len())
        .map(|i| memory.read_u8(addr + i as u64))
        .collect();
    assert_eq!(actual.as_slice(), expected.as_slice());
}
