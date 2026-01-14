use firmware::{
    bios::Bios,
    bios::BiosConfig,
    cpu::CpuState,
    memory::{far_ptr_to_phys, real_addr, MemoryBus, VecMemory},
    rtc::{CmosRtc, DateTime},
    video::vbe::VbeDevice,
};

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

#[test]
fn int10_vbe_mode_info_uses_configured_lfb_base() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let rtc = CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0));
    let lfb_base = 0x00C0_0000;
    let mut bios = Bios::new_with_rtc(
        BiosConfig {
            vbe_lfb_base: Some(lfb_base),
            ..BiosConfig::default()
        },
        rtc,
    );
    let mut cpu = CpuState::default();

    cpu.set_ax(0x4F01);
    cpu.set_cx(0x118);
    cpu.set_es(0x2000);
    cpu.set_di(0x0300);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mode_addr = real_addr(cpu.es(), cpu.di());
    let mut info = vec![0u8; 256];
    mem.read_bytes(mode_addr, &mut info);
    assert_eq!(read_u32(&info, 40), lfb_base); // PhysBasePtr
}

#[test]
fn int10_vbe_controller_and_mode_info() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // ModeInfoBlock::ModeAttributes flags we rely on for bootloader/Windows compatibility.
    const MODE_ATTR_SUPPORTED: u16 = 1 << 0;
    const MODE_ATTR_COLOR: u16 = 1 << 2;
    const MODE_ATTR_GRAPHICS: u16 = 1 << 3;
    const MODE_ATTR_WINDOWED: u16 = 1 << 5;
    const MODE_ATTR_LFB: u16 = 1 << 7;
    const REQUIRED_MODE_ATTRS: u16 = MODE_ATTR_SUPPORTED
        | MODE_ATTR_COLOR
        | MODE_ATTR_GRAPHICS
        | MODE_ATTR_WINDOWED
        | MODE_ATTR_LFB;

    // 4F00: controller info.
    cpu.set_ax(0x4F00);
    cpu.set_es(0x2000);
    cpu.set_di(0x0100);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let info_addr = real_addr(cpu.es(), cpu.di());
    let mut info = vec![0u8; 512];
    mem.read_bytes(info_addr, &mut info);
    assert_eq!(&info[0..4], b"VESA");
    assert!(read_u16(&info, 4) >= 0x0200);

    let mode_ptr = read_u32(&info, 14);
    let mode_list_phys = far_ptr_to_phys(mode_ptr);
    let mut modes = Vec::new();
    for i in 0..64u32 {
        let m = mem.read_u16(mode_list_phys + (i * 2) as u64);
        if m == 0xFFFF {
            break;
        }
        modes.push(m);
    }
    assert!(modes.contains(&0x115));
    assert!(modes.contains(&0x118));
    assert!(modes.contains(&0x160));

    let mut assert_mode_info = |mode: u16, width: u16, height: u16| {
        cpu.set_ax(0x4F01);
        cpu.set_cx(mode);
        cpu.set_es(0x2000);
        cpu.set_di(0x0300);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(cpu.ax(), 0x004F);
        assert!(!cpu.cf());

        let mode_addr = real_addr(cpu.es(), cpu.di());
        let mut info = vec![0u8; 256];
        mem.read_bytes(mode_addr, &mut info);
        let attrs = read_u16(&info, 0);
        assert_eq!(attrs & REQUIRED_MODE_ATTRS, REQUIRED_MODE_ATTRS);
        assert_eq!(read_u16(&info, 18), width); // XResolution
        assert_eq!(read_u16(&info, 20), height); // YResolution
        assert_eq!(read_u16(&info, 16), width * 4); // BytesPerScanLine
        assert_eq!(info[25], 32); // BitsPerPixel
                                  // Banked window parameters: 64KiB window and correct bank count for the mode.
        assert_eq!(read_u16(&info, 4), 64); // WinGranularity (KB)
        assert_eq!(read_u16(&info, 6), 64); // WinSize (KB)
        let fb_bytes = u32::from(width) * u32::from(height) * 4;
        let expected_banks = fb_bytes.div_ceil(64 * 1024) as u8;
        assert_eq!(info[26], expected_banks); // NumberOfBanks
        assert_eq!(info[28], 64); // BankSize (KB)
        assert_eq!(info[27], 0x06); // MemoryModel (direct color)
        assert_eq!(info[31], 8); // RedMaskSize
        assert_eq!(info[32], 16); // RedFieldPosition
        assert_eq!(info[33], 8); // GreenMaskSize
        assert_eq!(info[34], 8); // GreenFieldPosition
        assert_eq!(info[35], 8); // BlueMaskSize
        assert_eq!(info[36], 0); // BlueFieldPosition
        assert_eq!(info[37], 8); // ReservedMaskSize
        assert_eq!(info[38], 24); // ReservedFieldPosition
        assert_eq!(read_u32(&info, 40), VbeDevice::LFB_BASE_DEFAULT); // PhysBasePtr
    };

    // 4F01: mode info for required 32bpp LFB modes.
    assert_mode_info(0x115, 800, 600);
    assert_mode_info(0x118, 1024, 768);
    assert_mode_info(0x160, 1280, 720);
    // Some bootloaders preserve the "LFB requested" flag bit when calling 4F01; accept it.
    assert_mode_info(0x118 | 0x4000, 1024, 768);

    // Also verify an 8bpp packed-pixel mode advertises the same expected attributes.
    cpu.set_ax(0x4F01);
    cpu.set_cx(0x101);
    cpu.set_es(0x2000);
    cpu.set_di(0x0400);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mode_addr = real_addr(cpu.es(), cpu.di());
    let mut info = vec![0u8; 256];
    mem.read_bytes(mode_addr, &mut info);
    let attrs = read_u16(&info, 0);
    assert_eq!(attrs & REQUIRED_MODE_ATTRS, REQUIRED_MODE_ATTRS);
    assert_eq!(read_u16(&info, 18), 640); // XResolution
    assert_eq!(read_u16(&info, 20), 480); // YResolution
    assert_eq!(info[25], 8); // BitsPerPixel
    assert_eq!(read_u16(&info, 4), 64); // WinGranularity (KB)
    assert_eq!(read_u16(&info, 6), 64); // WinSize (KB)
    let fb_bytes = 640u32 * 480u32;
    let expected_banks = fb_bytes.div_ceil(64 * 1024) as u8;
    assert_eq!(info[26], expected_banks); // NumberOfBanks
    assert_eq!(info[28], 64); // BankSize (KB)
}

#[test]
fn int10_vbe_set_mode_clears_framebuffer_and_reports_current_mode() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Pre-fill framebuffer with a non-zero pattern.
    let fb_base = VbeDevice::LFB_BASE_DEFAULT as u64;
    for i in 0..4096u64 {
        mem.write_u8(fb_base + i, 0xAA);
    }

    // 4F02: set mode 0x118 with clear.
    cpu.set_ax(0x4F02);
    cpu.set_bx(0x118 | 0x4000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());
    assert_eq!(bios.video.vbe.current_mode, Some(0x118));

    // Mode set clears framebuffer when bit15 is not set.
    let mut buf = vec![0u8; 4096];
    mem.read_bytes(fb_base, &mut buf);
    assert!(buf.iter().all(|&b| b == 0));

    // Make sure 4F03 reports the mode.
    cpu.set_ax(0x4F03);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.bx(), 0x118);

    // INT 10h AH=0F should report "VESA mode active" via AL=0x6F.
    cpu.set_ax(0x0F00);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.al(), 0x6F);

    // 4F02: set mode again with no-clear, after writing a pattern.
    for i in 0..4096u64 {
        mem.write_u8(fb_base + i, 0x55);
    }
    cpu.set_ax(0x4F02);
    cpu.set_bx(0x118 | 0x4000 | 0x8000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    let mut buf = vec![0u8; 4096];
    mem.read_bytes(fb_base, &mut buf);
    assert!(buf.iter().all(|&b| b == 0x55));

    // New required modes should also be settable.
    for mode in [0x115u16, 0x160u16] {
        // Touch the last byte of the expected framebuffer and ensure a clear-mode-set wipes it.
        let fb_size = match mode {
            0x115 => 800u64 * 600 * 4,
            0x160 => 1280u64 * 720 * 4,
            _ => unreachable!(),
        };
        mem.write_u8(fb_base + fb_size - 1, 0xCC);

        cpu.set_ax(0x4F02);
        cpu.set_bx(mode | 0x4000);
        bios.handle_int10(&mut cpu, &mut mem);
        assert_eq!(cpu.ax(), 0x004F);
        assert!(!cpu.cf());
        assert_eq!(bios.video.vbe.current_mode, Some(mode));
        assert_eq!(mem.read_u8(fb_base + fb_size - 1), 0);
    }
}

#[test]
fn int10_vbe_default_palette_matches_vga_defaults() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Enter an 8bpp VBE mode.
    cpu.set_ax(0x4F02);
    cpu.set_bx(0x105 | 0x4000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    // Read back one palette entry via INT 10h AX=4F09 "Get Palette Data".
    let pal_seg = 0x3000;
    let pal_off = 0x0100;
    let pal_addr = real_addr(pal_seg, pal_off);

    cpu.set_ax(0x4F09);
    cpu.set_bx(0x0001); // BL=1 get
    cpu.set_cx(1); // one entry
    cpu.set_dx(4); // palette index 4 (EGA red)
    cpu.set_es(pal_seg);
    cpu.set_di(pal_off);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    // Firmware stores entries as B, G, R, 0 with 6-bit components. EGA red = (0xAA,0,0) in 8-bit
    // which scales to 0x2A in 6-bit.
    let mut buf = [0u8; 4];
    mem.read_bytes(pal_addr, &mut buf);
    assert_eq!(buf, [0x00, 0x00, 0x2A, 0x00]);
}

#[test]
fn int10_vbe_set_mode_oem_1280x720_updates_scanline_and_clears_framebuffer() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    let mode = bios
        .video
        .vbe
        .find_mode(0x160)
        .expect("missing VBE mode 0x160");
    let fb_base = VbeDevice::LFB_BASE_DEFAULT as u64;
    let fb_size = mode.framebuffer_size_bytes() as u64;

    // Touch the last byte so we can verify the clear covers the whole mode.
    mem.write_u8(fb_base + fb_size - 1, 0xAA);

    // 4F02: set mode 0x160 with clear.
    cpu.set_ax(0x4F02);
    cpu.set_bx(0x160 | 0x4000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());
    assert_eq!(bios.video.vbe.current_mode, Some(0x160));

    assert_eq!(mem.read_u8(fb_base + fb_size - 1), 0);

    // 4F06 get logical scan line length should reflect the mode's pitch.
    cpu.set_ax(0x4F06);
    cpu.set_bx(0x0001); // BL=1 get
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.bx(), 1280 * 4);
    assert_eq!(cpu.cx(), 1280);
}

#[test]
fn int10_vbe_misc_services() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

    // Enter a VBE mode first.
    cpu.set_ax(0x4F02);
    cpu.set_bx(0x118 | 0x4000);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    // 4F06 get logical scan line length.
    cpu.set_ax(0x4F06);
    cpu.set_bx(0x0001); // BL=1 get
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.bx(), 1024 * 4);
    assert_eq!(cpu.cx(), 1024);

    // 4F07 set and get display start.
    cpu.set_ax(0x4F07);
    cpu.set_bx(0x0000);
    cpu.set_cx(10);
    cpu.set_dx(20);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    cpu.set_ax(0x4F07);
    cpu.set_bx(0x0001);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.cx(), 10);
    assert_eq!(cpu.dx(), 20);

    // 4F08 set and get DAC width.
    cpu.set_ax(0x4F08);
    cpu.set_bx(0x0800); // BL=0 set, BH=8 bits
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    cpu.set_ax(0x4F08);
    cpu.set_bx(0x0001); // BL=1 get
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert_eq!(cpu.bh(), 8);

    // 4F09 palette set/get.
    let pal_seg = 0x3000;
    let pal_off = 0x0200;
    let pal_addr = real_addr(pal_seg, pal_off);
    mem.write_bytes(pal_addr, &[1, 2, 3, 0, 4, 5, 6, 0]);

    cpu.set_ax(0x4F09);
    cpu.set_bx(0x0000);
    cpu.set_cx(2);
    cpu.set_dx(0);
    cpu.set_es(pal_seg);
    cpu.set_di(pal_off);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    for i in 0..8u64 {
        mem.write_u8(pal_addr + i, 0);
    }
    cpu.set_ax(0x4F09);
    cpu.set_bx(0x0001);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);

    let mut pal_buf = [0u8; 8];
    mem.read_bytes(pal_addr, &mut pal_buf);
    assert_eq!(&pal_buf, &[1, 2, 3, 0, 4, 5, 6, 0]);

    // 4F15 DDC: capability query.
    cpu.set_ax(0x4F15);
    cpu.set_bx(0x0000); // BL=0
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());
    assert_eq!(cpu.bx(), 0x0200);

    // 4F15 DDC: read EDID block.
    let edid = aero_edid::read_edid(0).expect("missing base EDID");
    let edid_seg = 0x3100;
    let edid_off = 0x0400;
    let edid_addr = real_addr(edid_seg, edid_off);

    cpu.set_ax(0x4F15);
    cpu.set_bx(0x0001); // BL=1
    cpu.set_dx(0);
    cpu.set_es(edid_seg);
    cpu.set_di(edid_off);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mut edid_buf = vec![0u8; edid.len()];
    mem.read_bytes(edid_addr, &mut edid_buf);
    assert_eq!(edid_buf.as_slice(), edid.as_slice());

    // Unsupported DDC subfunction should fail cleanly.
    cpu.set_ax(0x4F15);
    cpu.set_bl(0xFF);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x014F);
    assert!(cpu.cf());
}
