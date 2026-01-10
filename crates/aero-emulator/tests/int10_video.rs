use aero_emulator::{
    bios::{int10, int10_vbe::NoVbe},
    cpu::CpuState,
    devices::vga::VgaDevice,
    firmware::bda::BiosDataArea,
    memory::LinearMemory,
};

#[test]
fn mode_03_teletype_scrolling() {
    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = NoVbe::default();

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    for i in 0..26u8 {
        cpu.set_ah(0x0E);
        cpu.set_al(b'A' + i);
        cpu.set_bh(0);
        cpu.set_bl(0);
        int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

        if i != 25 {
            cpu.set_al(b'\r');
            int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
            cpu.set_al(b'\n');
            int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);
        }
    }

    assert_eq!(vga.read_text_cell(0, 0).0, b'B');
    assert_eq!(vga.read_text_cell(23, 0).0, b'Y');
    assert_eq!(vga.read_text_cell(24, 0).0, b'Z');

    let (row, col) = BiosDataArea::cursor_pos(&mem, 0);
    assert_eq!((row, col), (24, 1));
}

#[test]
fn cursor_position_and_shape() {
    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = NoVbe::default();

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(5);
    cpu.set_dl(10);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x01);
    cpu.set_ch(1);
    cpu.set_cl(2);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x03);
    cpu.set_bh(0);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    assert_eq!((cpu.dh(), cpu.dl()), (5, 10));
    assert_eq!((cpu.ch(), cpu.cl()), (1, 2));
}

#[test]
fn write_char_attr_repeat_does_not_move_cursor() {
    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = NoVbe::default();

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(0);
    cpu.set_dl(0);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x09);
    cpu.set_al(b'X');
    cpu.set_bh(0);
    cpu.set_bl(0x1E);
    cpu.set_cx(3);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    assert_eq!(vga.read_text_cell(0, 0), (b'X', 0x1E));
    assert_eq!(vga.read_text_cell(0, 1), (b'X', 0x1E));
    assert_eq!(vga.read_text_cell(0, 2), (b'X', 0x1E));

    let (row, col) = BiosDataArea::cursor_pos(&mem, 0);
    assert_eq!((row, col), (0, 0));
}

#[test]
fn write_char_only_repeat_preserves_attribute() {
    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = NoVbe::default();

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    vga.write_text_cell(0, 0, b'A', 0x2F);
    vga.write_text_cell(0, 1, b'A', 0x2F);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(0);
    cpu.set_dl(0);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    cpu.set_ah(0x0A);
    cpu.set_al(b'B');
    cpu.set_bh(0);
    cpu.set_cx(2);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    assert_eq!(vga.read_text_cell(0, 0), (b'B', 0x2F));
    assert_eq!(vga.read_text_cell(0, 1), (b'B', 0x2F));
}

#[test]
fn mode_13_sets_text_geometry_and_allows_pixel_writes() {
    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = NoVbe::default();

    cpu.set_ah(0x00);
    cpu.set_al(0x13);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    assert_eq!(BiosDataArea::video_mode(&mem), 0x13);
    assert_eq!(BiosDataArea::text_columns(&mem), 40);

    vga.write_pixel(1, 2, 0x7F);
    assert_eq!(vga.read_pixel(1, 2), 0x7F);
}

#[test]
fn ax_4f_calls_are_dispatched_to_vbe() {
    use aero_emulator::bios::int10_vbe::VbeServices;

    #[derive(Default)]
    struct Probe {
        calls: u32,
    }

    impl VbeServices for Probe {
        fn handle_int10(
            &mut self,
            cpu: &mut CpuState,
            _mem: &mut impl aero_emulator::memory::MemoryBus,
            _vga: &mut VgaDevice,
        ) {
            self.calls += 1;
            cpu.set_ax(0x004F);
            cpu.set_cf(false);
        }
    }

    let mut cpu = CpuState::default();
    let mut mem = LinearMemory::new(1024 * 1024);
    let mut vga = VgaDevice::default();
    let mut vbe = Probe::default();

    cpu.set_ax(0x4F00);
    int10::handle_int10(&mut cpu, &mut mem, &mut vga, &mut vbe);

    assert_eq!(vbe.calls, 1);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());
}
