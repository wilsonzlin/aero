use firmware::{
    bios::Bios,
    cpu::CpuState,
    memory::{MemoryBus, VecMemory},
    rtc::{CmosRtc, DateTime},
};

const VGA_FB_BASE: u64 = 0xA0000;

#[test]
fn int10_mode13h_write_and_read_pixel_services() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 13h (clear requested; bit7=0).
    cpu.set_ax(0x0013);
    bios.handle_int10(&mut cpu, &mut mem);

    let x: u16 = 12;
    let y: u16 = 34;
    let color: u8 = 0x5A;
    let addr = VGA_FB_BASE + (u64::from(y) * 320) + u64::from(x);

    // AH=0Ch: write pixel.
    cpu.set_ax((0x0C_u16 << 8) | u16::from(color));
    cpu.set_cx(x);
    cpu.set_dx(y);
    cpu.set_bx(0); // BH=page 0 (ignored)
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(mem.read_u8(addr), color);

    // AH=0Dh: read pixel.
    cpu.set_ax(0x0D00);
    cpu.set_al(0x00); // overwritten by BIOS if supported
    cpu.set_cx(x);
    cpu.set_dx(y);
    cpu.set_bx(0);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(cpu.al(), color);
}

#[test]
fn int10_mode13h_write_pixel_out_of_bounds_is_ignored() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    cpu.set_ax(0x0013);
    bios.handle_int10(&mut cpu, &mut mem);

    // Seed pixels that a buggy "clamp to bounds" implementation might accidentally hit.
    //
    // - x=320 would clamp to x=319, y=0
    // - y=200 would clamp to y=199, x=0
    let addr_clamp_x = VGA_FB_BASE + 319;
    let addr_clamp_y = VGA_FB_BASE + (199u64 * 320);
    mem.write_u8(addr_clamp_x, 0x11);
    mem.write_u8(addr_clamp_y, 0x33);

    // Attempt to write out-of-bounds; should not modify the framebuffer.
    cpu.set_ax((0x0C_u16 << 8) | 0x22);
    cpu.set_cx(320); // out of bounds: x must be < 320
    cpu.set_dx(0);
    cpu.set_bx(0);
    bios.handle_int10(&mut cpu, &mut mem);

    cpu.set_ax((0x0C_u16 << 8) | 0x44);
    cpu.set_cx(0);
    cpu.set_dx(200); // out of bounds: y must be < 200
    cpu.set_bx(0);
    bios.handle_int10(&mut cpu, &mut mem);

    // Reads out of bounds should return 0.
    cpu.set_ax(0x0D00);
    cpu.set_cx(320);
    cpu.set_dx(0);
    cpu.set_bx(0);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.al(), 0);

    cpu.set_ax(0x0D00);
    cpu.set_cx(0);
    cpu.set_dx(200);
    cpu.set_bx(0);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.al(), 0);

    assert_eq!(mem.read_u8(addr_clamp_x), 0x11);
    assert_eq!(mem.read_u8(addr_clamp_y), 0x33);
}
