use firmware::{
    bda::BiosDataArea,
    bios::Bios,
    cpu::CpuState,
    memory::{MemoryBus, VecMemory},
    rtc::{CmosRtc, DateTime},
    video::vbe::VbeDevice,
};

#[test]
fn int10_mode13h_write_and_read_pixel_roundtrip() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Set mode 13h (320x200x256).
    cpu.set_ax(0x0013);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(BiosDataArea::read_video_mode(&mut mem), 0x13);

    let x = 100u16;
    let y = 50u16;
    let color = 0x5Au8;

    // AH=0Ch write pixel.
    cpu.set_ax(0x0C00 | u16::from(color));
    cpu.set_bx(0x0000); // page 0
    cpu.set_cx(x);
    cpu.set_dx(y);
    bios.handle_int10(&mut cpu, &mut mem);

    let addr = 0xA0000u64 + (u32::from(y) * 320 + u32::from(x)) as u64;
    assert_eq!(mem.read_u8(addr), color);

    // AH=0Dh read pixel.
    cpu.set_ax(0x0D00);
    cpu.set_bx(0x0000);
    cpu.set_cx(x);
    cpu.set_dx(y);
    bios.handle_int10(&mut cpu, &mut mem);

    assert_eq!(cpu.al(), color);
}

#[test]
fn int10_vbe_32bpp_write_and_read_pixel_roundtrip() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    let x = 10u16;
    let y = 20u16;
    // BGRX bytes: [0x11, 0x22, 0x33, 0x44] in memory.
    let color: u32 = 0x4433_2211;

    for (mode, (w, h)) in [
        (0x115u16, (800u64, 600u64)),
        (0x118u16, (1024, 768)),
        (0x160u16, (1280, 720)),
    ] {
        // Enter a 32bpp VBE mode.
        cpu.set_ax(0x4F02);
        cpu.set_bx(mode | 0x4000);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(cpu.ax(), 0x004F);
        assert_eq!(bios.video.vbe.current_mode, Some(mode));

        // AH=0Ch write pixel. For 32bpp VBE modes we use EBX as the pixel value (BGRX).
        cpu.set_ax(0x0C00);
        cpu.rbx = u64::from(color);
        cpu.set_cx(x);
        cpu.set_dx(y);
        bios.handle_int10(&mut cpu, &mut mem);

        let base = VbeDevice::LFB_BASE_DEFAULT as u64;
        let pitch = w * 4;
        let addr = base + u64::from(y) * pitch + u64::from(x) * 4;
        assert_eq!(
            mem.read_u32(addr),
            color,
            "pixel value should be written for mode 0x{mode:04X}"
        );

        // AH=0Dh read pixel.
        cpu.set_ax(0x0D00);
        cpu.set_cx(x);
        cpu.set_dx(y);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(
            (cpu.rbx & 0xFFFF_FFFF) as u32,
            color,
            "pixel value should be read back for mode 0x{mode:04X}"
        );

        // Sanity: bounds of the write/read coordinates should fit the selected mode.
        assert!(u64::from(x) < w && u64::from(y) < h);
    }
}

#[test]
fn int10_vbe_32bpp_out_of_bounds_pixels_are_ignored_and_read_as_zero() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    let color: u32 = 0x1122_3344;
    let sentinel: u32 = 0xA5A5_5A5A;

    for (mode, (w, h)) in [
        (0x115u16, (800u64, 600u64)),
        (0x118u16, (1024, 768)),
        (0x160u16, (1280, 720)),
    ] {
        // Enter a 32bpp VBE mode.
        cpu.set_ax(0x4F02);
        cpu.set_bx(mode | 0x4000);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(cpu.ax(), 0x004F);

        let base = VbeDevice::LFB_BASE_DEFAULT as u64;
        let pitch = w * 4;
        let last_pixel_addr = base + (h - 1) * pitch + (w - 1) * 4;
        mem.write_u32(last_pixel_addr, sentinel);

        // AH=0Ch write pixel out of bounds: x == width should be ignored (not clamped).
        cpu.set_ax(0x0C00);
        cpu.rbx = u64::from(color);
        cpu.set_cx(w as u16);
        cpu.set_dx(0);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(
            mem.read_u32(last_pixel_addr),
            sentinel,
            "out-of-bounds write should not touch framebuffer for mode 0x{mode:04X}"
        );

        // AH=0Dh read pixel out of bounds should return 0 (not last pixel).
        cpu.set_ax(0x0D00);
        cpu.rbx = 0xDEAD_BEEF_0000_0000u64; // sentinel high bits, low bits should be overwritten
        cpu.set_cx(w as u16);
        cpu.set_dx(0);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(
            (cpu.rbx & 0xFFFF_FFFF) as u32,
            0,
            "out-of-bounds read should return 0 for mode 0x{mode:04X}"
        );
        assert_eq!(
            cpu.rbx & 0xFFFF_FFFF_0000_0000,
            0xDEAD_BEEF_0000_0000,
            "out-of-bounds read should preserve upper bits of RBX for mode 0x{mode:04X}"
        );
    }
}
