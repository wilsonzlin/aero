use emulator::devices::vga::vbe;
use firmware::{
    bios::Bios,
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
fn int10_vbe_controller_and_mode_info() {
    let mut mem = VecMemory::new(32 * 1024 * 1024);
    let mut bios = Bios::new(CmosRtc::new(DateTime::new(2026, 1, 1, 0, 0, 0)));
    let mut cpu = CpuState::default();

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
    assert!(modes.contains(&0x118));

    // 4F01: mode info for 0x118.
    cpu.set_ax(0x4F01);
    cpu.set_cx(0x118);
    cpu.set_es(0x2000);
    cpu.set_di(0x0300);
    bios.handle_int10(&mut cpu, &mut mem);
    assert_eq!(cpu.ax(), 0x004F);
    assert!(!cpu.cf());

    let mode_addr = real_addr(cpu.es(), cpu.di());
    let mut mode = vec![0u8; 256];
    mem.read_bytes(mode_addr, &mut mode);
    assert_eq!(read_u16(&mode, 18), 1024); // XResolution
    assert_eq!(read_u16(&mode, 20), 768); // YResolution
    assert_eq!(mode[25], 32); // BitsPerPixel
    assert_eq!(read_u32(&mode, 40), VbeDevice::LFB_BASE_DEFAULT); // PhysBasePtr
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
    let edid = vbe::read_edid(0).expect("missing base EDID");
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
