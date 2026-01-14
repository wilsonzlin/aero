use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT};
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_display_start_boot_sector(lfb_base: u32) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov ax, 0x4F02 (VBE Set SuperVGA Video Mode)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;

    // mov bx, 0x4118 (mode 0x118 + LFB requested)
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Ensure DS=0 so 32-bit offsets address physical memory directly.
    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;

    // mov edi, lfb_base (66 BF imm32)
    sector[i..i + 2].copy_from_slice(&[0x66, 0xBF]);
    i += 2;
    sector[i..i + 4].copy_from_slice(&lfb_base.to_le_bytes());
    i += 4;

    // pixel(0,0) = red (B,G,R,X = 00,00,FF,00)
    // mov dword [edi], 0x00FF0000 (66 67 C7 07 imm32)
    sector[i..i + 4].copy_from_slice(&[0x66, 0x67, 0xC7, 0x07]);
    i += 4;
    sector[i..i + 4].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
    i += 4;

    // pixel(1,0) = green (B,G,R,X = 00,FF,00,00)
    // mov dword [edi+4], 0x0000FF00 (66 67 C7 47 04 imm32)
    sector[i..i + 5].copy_from_slice(&[0x66, 0x67, 0xC7, 0x47, 0x04]);
    i += 5;
    sector[i..i + 4].copy_from_slice(&0x0000_FF00u32.to_le_bytes());
    i += 4;

    // Set VBE display start (panning): x=1, y=0.
    //
    // mov ax, 0x4F07
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x07, 0x4F]);
    i += 3;
    // xor bx, bx (BL=0x00 set)
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov cx, 1
    sector[i..i + 3].copy_from_slice(&[0xB9, 0x01, 0x00]);
    i += 3;
    // xor dx, dx
    sector[i..i + 2].copy_from_slice(&[0x31, 0xD2]);
    i += 2;
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
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boot_int10_vbe_display_start() {
    for enable_aerogpu in [false, true] {
        let cfg = MachineConfig {
            enable_pc_platform: true,
            enable_vga: !enable_aerogpu,
            enable_aerogpu,
            // Keep the test output deterministic.
            enable_serial: false,
            enable_i8042: false,
            enable_a20_gate: false,
            enable_reset_ctrl: false,
            enable_e1000: false,
            enable_virtio_net: false,
            ..Default::default()
        };

        let mut m = Machine::new(cfg).unwrap();

        let boot = build_int10_vbe_display_start_boot_sector(m.vbe_lfb_base_u32());
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();
        run_until_halt(&mut m);

        // Verify the BIOS VBE display-start state is mirrored into the Bochs VBE_DISPI x/y offset
        // registers (regardless of whether the underlying device is the standalone VGA model or
        // the AeroGPU legacy Bochs VBE frontend).
        m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0008);
        let x_off = m.io_read(VBE_DISPI_DATA_PORT, 2) as u16;
        m.io_write(VBE_DISPI_INDEX_PORT, 2, 0x0009);
        let y_off = m.io_read(VBE_DISPI_DATA_PORT, 2) as u16;
        assert_eq!((x_off, y_off), (1, 0));

        m.display_present();
        assert_eq!(m.display_resolution(), (1024, 768));
        // The guest wrote red at (0,0) and green at (1,0), then panned to x=1.
        // The top-left visible pixel should therefore be green.
        assert_eq!(m.display_framebuffer()[0], 0xFF00_FF00);
    }
}
