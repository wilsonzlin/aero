use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_palette_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4105 (mode 0x105 + LFB requested, 1024x768x8bpp)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x05, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // mov ax, 0x4F08 (VBE Set/Get DAC Palette Format)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x08, 0x4F]);
    i += 3;
    // mov bx, 0x0800 (BL=0 "set", BH=8 bits/component)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x00, 0x08]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write one palette entry (B,G,R,0) to 0x0000:0x0500.
    // We'll set palette index 1 to (R,G,B)=(0x20,0x10,0x30) using 8-bit DAC semantics; the
    // machine should downscale to 6-bit when mirroring into the emulated VGA DAC ports.
    //
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov byte [di], 0x30 (B)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x30]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x10 (G)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x10]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x20 (R)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x20]);
    i += 3;
    // inc di
    sector[i] = 0x47;
    i += 1;
    // mov byte [di], 0x00 (reserved)
    sector[i..i + 3].copy_from_slice(&[0xC6, 0x05, 0x00]);
    i += 3;

    // mov ax, 0x4F09 (VBE Set/Get Palette Data)
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

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boot_int10_aerogpu_vbe_palette_sync_updates_dac_ports() {
    let boot = build_int10_vbe_palette_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 64 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Read palette entry 1 back via VGA DAC ports (R,G,B).
    m.io_write(0x3C7, 1, 0x01);
    let r = m.io_read(0x3C9, 1) as u8;
    let g = m.io_read(0x3C9, 1) as u8;
    let b = m.io_read(0x3C9, 1) as u8;

    assert_eq!([r, g, b], [0x20 >> 2, 0x10 >> 2, 0x30 >> 2]);
}
