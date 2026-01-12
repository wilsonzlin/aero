use aero_gpu_vga::{DisplayOutput, SVGA_LFB_BASE};
use aero_machine::{Machine, MachineConfig, RunExit};
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

fn build_vbe_boot_sector() -> [u8; 512] {
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
    // mov cx, 0x0118
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x18, 0x01]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
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

#[test]
fn bios_vbe_sync_mode_and_lfb_base() {
    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(build_vbe_boot_sector().to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // VBE mode info block was written to 0x0000:0x0500 by INT 10h AX=4F01.
    let phys_base_ptr = m.read_physical_u32(0x0500 + 40);
    assert_eq!(phys_base_ptr, SVGA_LFB_BASE);

    let vga = m.vga().expect("pc platform should include VGA");
    {
        let mut vga = vga.borrow_mut();
        vga.present();
        assert_eq!(vga.get_resolution(), (1024, 768));
    }

    // Write a single red pixel at (0,0) in packed 32bpp BGRX.
    m.write_physical_u32(u64::from(SVGA_LFB_BASE), 0x00FF_0000);
    let pixel0 = {
        let mut vga = vga.borrow_mut();
        vga.present();
        vga.get_framebuffer()[0]
    };
    assert_eq!(pixel0, 0xFF00_00FF);
}

#[test]
fn bios_text_cursor_sync_updates_vga_crtc() {
    let cfg = MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let row = 12u8;
    let col = 34u8;
    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(build_set_cursor_boot_sector(row, col).to_vec())
        .unwrap();
    m.reset();
    run_until_halt(&mut m);

    let cols = m.read_physical_u16(0x044A).max(1);
    let expected = (u16::from(row)).saturating_mul(cols) + u16::from(col);

    m.io_write(0x3D4, 1, 0x0E);
    let hi = m.io_read(0x3D5, 1) as u8;
    m.io_write(0x3D4, 1, 0x0F);
    let lo = m.io_read(0x3D5, 1) as u8;

    let got = ((hi as u16) << 8) | (lo as u16);
    assert_eq!(got, expected);
}
