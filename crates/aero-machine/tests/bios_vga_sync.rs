use aero_gpu_vga::DisplayOutput;
use aero_machine::{Machine, MachineConfig, RunExit};
use firmware::bda::BDA_SCREEN_COLS_ADDR;
use pretty_assertions::assert_eq;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

fn build_vbe_boot_sector(mode: u16) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov ax, 0x4F01
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;
    // mov cx, mode
    let [mode_lo, mode_hi] = mode.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xB9, mode_lo, mode_hi]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, (mode + LFB requested)
    let bx = mode | 0x4000;
    let [bx_lo, bx_hi] = bx.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xBB, bx_lo, bx_hi]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_set_cursor_boot_sector(row: u8, col: u8) -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor bx, bx (BH=page 0)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov dx, imm16 (DH=row, DL=col)
    sector[i..i + 3].copy_from_slice(&[0xBA, col, row]);
    i += 3;
    // mov ax, 0x0200 (AH=0x02 set cursor position)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0x02]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_vbe_palette_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // Query VBE mode info for mode 0x105 into 0x0000:0x0600 so the host can discover PhysBasePtr.
    //
    // mov di, 0x0600
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x06]);
    i += 3;
    // mov ax, 0x4F01
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;
    // mov cx, 0x0105
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x05, 0x01]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4105 (mode 0x105 + LFB requested, 1024x768x8bpp)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x05, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write one palette entry (B,G,R,0) to 0x0000:0x0500.
    // We'll set palette index 1 to full red (6-bit component 0x3F).
    //
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov byte [di], 0x00 (B)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x00 (G)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x3F (R)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x3F]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x00 (reserved)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;

    // mov ax, 0x4F09 (set palette data)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x09, 0x4F]);
    i += 3;
    // mov bx, 0x0000 (BL=0 set palette)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x00]);
    i += 3;
    // mov cx, 0x0001 (count=1)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x01, 0x00]);
    i += 3;
    // mov dx, 0x0001 (start index=1)
    sector[i..i + 3].copy_from_slice(&[0xBA, 0x01, 0x00]);
    i += 3;
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_vbe_palette_boot_sector_6bit_accepts_8bit_input() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;

    // Query VBE mode info for mode 0x105 into 0x0000:0x0600 so the host can discover PhysBasePtr.
    //
    // mov di, 0x0600
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x06]);
    i += 3;
    // mov ax, 0x4F01
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;
    // mov cx, 0x0105
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x05, 0x01]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4105 (mode 0x105 + LFB requested, 1024x768x8bpp)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x05, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write one palette entry (B,G,R,0) to 0x0000:0x0500.
    //
    // The BIOS defaults to 6-bit DAC width. Some guests write 8-bit component values even when in
    // 6-bit mode; the firmware should downscale them so `4F09h Get` never returns out-of-range
    // palette values.
    //
    // We'll set palette index 1 to red with an 8-bit component value 0xAA. In 6-bit mode this
    // should be interpreted as 0x2A (0xAA >> 2), which renders back to an 8-bit value of 0xAA when
    // expanded for display.
    //
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov byte [di], 0x00 (B)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x00 (G)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0xAA (R, 8-bit input)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0xAA]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x00 (reserved)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;

    // mov ax, 0x4F09 (set palette data)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x09, 0x4F]);
    i += 3;
    // mov bx, 0x0000 (BL=0 set palette)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x00]);
    i += 3;
    // mov cx, 0x0001 (count=1)
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x01, 0x00]);
    i += 3;
    // mov dx, 0x0001 (start index=1)
    sector[i..i + 3].copy_from_slice(&[0xBA, 0x01, 0x00]);
    i += 3;
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn build_vbe_failed_mode_set_does_not_clear_boot_sector() -> [u8; 512] {
    let mut sector = [0u8; 512];
    let mut i = 0usize;

    // Ensure DS=0 so absolute memory operands hit physical 0x0000:xxxx.
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0xC118 (mode 0x118 + LFB requested + no-clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0xC1]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Signal the host that mode set has completed and we are about to wait.
    // mov word [0x0500], 0xBEEF
    sector[i..i + 6].copy_from_slice(&[0xC7, 0x06, 0x00, 0x05, 0xEF, 0xBE]);
    i += 6;

    // Wait until the host flips the word at 0x0500 to 0xDEAD.
    // mov ax, [0x0500]
    sector[i..i + 3].copy_from_slice(&[0xA1, 0x00, 0x05]);
    i += 3;
    // cmp ax, 0xDEAD
    sector[i..i + 3].copy_from_slice(&[0x3D, 0xAD, 0xDE]);
    i += 3;
    // jne -8 (back to mov ax, [0x0500])
    sector[i..i + 2].copy_from_slice(&[0x75, 0xF8]);
    i += 2;

    // Attempt to set an invalid mode (should fail). Use no-clear=0 so the machine-side
    // synchronization must not accidentally clear the *current* framebuffer when the BIOS call
    // fails.
    //
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x3FFF (invalid mode; no-clear bit 15 is clear)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0xFF, 0x3F]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[test]
fn bios_vbe_sync_mode_and_lfb_base() {
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        for (mode, (w, h)) in [
            (0x115u16, (800, 600)),
            (0x118u16, (1024, 768)),
            (0x160u16, (1280, 720)),
        ] {
            m.set_disk_image(build_vbe_boot_sector(mode).to_vec())
                .unwrap();
            m.reset();
            run_until_halt(&mut m);

            // VBE mode info block was written to 0x0000:0x0500 by INT 10h AX=4F01.
            let mode_info_addr = 0x0500u64;
            let attrs = m.read_physical_u16(mode_info_addr);
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
            assert_eq!(attrs & REQUIRED_MODE_ATTRS, REQUIRED_MODE_ATTRS);

            assert_eq!(m.read_physical_u16(mode_info_addr + 18), w as u16); // XResolution
            assert_eq!(m.read_physical_u16(mode_info_addr + 20), h as u16); // YResolution
            assert_eq!(m.read_physical_u16(mode_info_addr + 16), (w as u16) * 4); // BytesPerScanLine
            assert_eq!(m.read_physical_u8(mode_info_addr + 25), 32); // BitsPerPixel
            assert_eq!(m.read_physical_u8(mode_info_addr + 27), 0x06); // MemoryModel (direct color)
            assert_eq!(m.read_physical_u8(mode_info_addr + 31), 8); // RedMaskSize
            assert_eq!(m.read_physical_u8(mode_info_addr + 32), 16); // RedFieldPosition
            assert_eq!(m.read_physical_u8(mode_info_addr + 33), 8); // GreenMaskSize
            assert_eq!(m.read_physical_u8(mode_info_addr + 34), 8); // GreenFieldPosition
            assert_eq!(m.read_physical_u8(mode_info_addr + 35), 8); // BlueMaskSize
            assert_eq!(m.read_physical_u8(mode_info_addr + 36), 0); // BlueFieldPosition
            assert_eq!(m.read_physical_u8(mode_info_addr + 37), 8); // ReservedMaskSize
            assert_eq!(m.read_physical_u8(mode_info_addr + 38), 24); // ReservedFieldPosition

            let phys_base_ptr = m.read_physical_u32(mode_info_addr + 40);
            assert_eq!(phys_base_ptr, m.vbe_lfb_base_u32());

            if enable_aerogpu {
                // Phase 2: AeroGPU-owned boot display. LFB base should be derived from BAR1.
                let aerogpu_bdf = m.aerogpu().expect("aerogpu enabled implies device present");
                let bar1_base = m
                    .pci_bar_base(aerogpu_bdf, 1)
                    .expect("aerogpu BAR1 base should be assigned");
                let expected = bar1_base + aero_machine::VBE_LFB_OFFSET as u64;
                assert_eq!(m.vbe_lfb_base(), expected);
            } else {
                let vga = m.vga().expect("pc platform should include VGA");
                let mut vga = vga.borrow_mut();
                vga.present();
                assert_eq!(vga.get_resolution(), (w, h));
            }

            m.display_present();
            assert_eq!(m.display_resolution(), (w, h));

            // Write a single red pixel at (0,0) in packed 32bpp BGRX.
            m.write_physical_u32(m.vbe_lfb_base(), 0x00FF_0000);
            m.display_present();
            assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
        }
    }
}

#[test]
fn bios_vbe_sync_mode_and_custom_lfb_base() {
    // Pick a base outside the BIOS PCI BAR allocator default window
    // (`PciResourceAllocatorConfig::default().mmio_base..+mmio_size`, currently
    // `0xE000_0000..0xF000_0000`) to ensure the custom-base path does not rely on that range.
    let custom_lfb_base = 0xD000_0000;
    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        vga_lfb_base: Some(custom_lfb_base),
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(build_vbe_boot_sector(0x118).to_vec())
        .unwrap();
    m.reset();
    run_until_halt(&mut m);

    // VBE mode info block was written to 0x0000:0x0500 by INT 10h AX=4F01.
    let phys_base_ptr = m.read_physical_u32(0x0500 + 40);
    assert_eq!(phys_base_ptr, custom_lfb_base);

    let vga = m.vga().expect("pc platform should include VGA");
    {
        let mut vga = vga.borrow_mut();
        vga.present();
        assert_eq!(vga.get_resolution(), (1024, 768));
    }

    // Write a single red pixel at (0,0) in packed 32bpp BGRX.
    m.write_physical_u32(u64::from(custom_lfb_base), 0x00FF_0000);
    let pixel0 = {
        let mut vga = vga.borrow_mut();
        vga.present();
        vga.get_framebuffer()[0]
    };
    assert_eq!(pixel0, 0xFF00_00FF);
}

#[test]
fn bios_text_cursor_sync_updates_vga_crtc() {
    let row = 12u8;
    let col = 34u8;
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.set_disk_image(build_set_cursor_boot_sector(row, col).to_vec())
            .unwrap();
        m.reset();
        run_until_halt(&mut m);

        let cols = m.read_physical_u16(BDA_SCREEN_COLS_ADDR).max(1);
        let expected = (u16::from(row)).saturating_mul(cols) + u16::from(col);

        m.io_write(0x3D4, 1, 0x0E);
        let hi = m.io_read(0x3D5, 1) as u8;
        m.io_write(0x3D4, 1, 0x0F);
        let lo = m.io_read(0x3D5, 1) as u8;

        let got = ((hi as u16) << 8) | (lo as u16);
        assert_eq!(got, expected);
    }
}

#[test]
fn bios_vbe_palette_sync_updates_vga_dac() {
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.set_disk_image(build_vbe_palette_boot_sector().to_vec())
            .unwrap();
        m.reset();
        run_until_halt(&mut m);

        // Write palette index 1 to the first pixel in the 8bpp framebuffer.
        m.write_physical_u8(m.vbe_lfb_base(), 1);
        m.display_present();
        let pixel0 = m.display_framebuffer()[0];

        // Palette entry 1 was set to red (B=0,G=0,R=0x3F).
        assert_eq!(pixel0, 0xFF00_00FF);

        if !enable_aerogpu {
            let vga = m.vga().expect("machine should include VGA");
            let pixel0_vga = {
                let mut vga = vga.borrow_mut();
                vga.present();
                vga.get_framebuffer()[0]
            };
            assert_eq!(pixel0_vga, pixel0);
        }
    }
}

#[test]
fn bios_vbe_palette_sync_accepts_8bit_input_in_6bit_mode() {
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.set_disk_image(build_vbe_palette_boot_sector_6bit_accepts_8bit_input().to_vec())
            .unwrap();
        m.reset();
        run_until_halt(&mut m);

        // Write palette index 1 to the first pixel in the 8bpp framebuffer.
        m.write_physical_u8(m.vbe_lfb_base(), 1);

        m.display_present();
        let pixel0 = m.display_framebuffer()[0];

        // Palette entry 1 was set via `4F09h set` with an 8-bit component value 0xAA in 6-bit DAC mode.
        // The firmware should downscale it to 6-bit (0x2A), which renders back to 8-bit 0xAA.
        assert_eq!(pixel0, 0xFF00_00AA);
    }
}

#[test]
fn bios_vbe_failed_mode_set_does_not_clear_existing_framebuffer() {
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 64 * 1024 * 1024,
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();
        m.set_disk_image(build_vbe_failed_mode_set_does_not_clear_boot_sector().to_vec())
            .unwrap();
        m.reset();

        // Run until the guest has set mode 0x118 and is spinning, waiting for host input.
        for _ in 0..200 {
            if m.read_physical_u16(0x0500) == 0xBEEF {
                break;
            }
            match m.run_slice(50_000) {
                RunExit::Completed { .. } => {}
                other => panic!("unexpected exit while waiting for guest rendezvous: {other:?}"),
            }
        }
        assert_eq!(
            m.read_physical_u16(0x0500),
            0xBEEF,
            "guest never reached rendezvous after setting VBE mode"
        );

        // Now that the VBE mode is active, pre-fill the first pixel. The guest will attempt (and
        // fail) to set an invalid VBE mode next; the existing framebuffer contents must be
        // preserved across the failing INT 10h call.
        m.write_physical_u32(m.vbe_lfb_base(), 0x00FF_0000);

        // Release the guest to perform the failing mode set.
        m.write_physical_u16(0x0500, 0xDEAD);
        run_until_halt(&mut m);

        m.display_present();
        assert_eq!(m.display_resolution(), (1024, 768));
        assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);

        if !enable_aerogpu {
            let vga = m.vga().expect("machine should include VGA");
            let mut vga = vga.borrow_mut();
            vga.present();
            assert_eq!(vga.get_resolution(), (1024, 768));
            assert_eq!(vga.get_framebuffer()[0], 0xFF00_00FF);
        }
    }
}
