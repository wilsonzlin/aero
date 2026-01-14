use aero_cpu_core::state::gpr;
use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_set_mode_boot_sector(
    vbe_mode_with_flags: u16,
) -> [u8; aero_storage::SECTOR_SIZE] {
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

    // mov bx, imm16 (requested VBE mode + flags, e.g. LFB bit)
    sector[i..i + 3].copy_from_slice(&[
        0xBB,
        (vbe_mode_with_flags & 0x00FF) as u8,
        (vbe_mode_with_flags >> 8) as u8,
    ]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Query VBE mode info (so the host can discover PhysBasePtr).
    //
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;
    // mov ax, 0x4F01
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;
    // mov cx, mode (strip any set-mode flags like LFB/no-clear).
    let mode = vbe_mode_with_flags & 0x3FFF;
    let [mode_lo, mode_hi] = mode.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xB9, mode_lo, mode_hi]);
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
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

fn new_deterministic_test_machine(
    boot_sector: [u8; aero_storage::SECTOR_SIZE],
    enable_aerogpu: bool,
) -> Machine {
    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: !enable_aerogpu,
        enable_aerogpu,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot_sector.to_vec()).unwrap();
    m.reset();
    m
}

fn assert_vbe_mode_set_and_lfb_visible(vbe_mode_with_flags: u16, expected_res: (u32, u32)) {
    for enable_aerogpu in [false, true] {
        let boot = build_int10_vbe_set_mode_boot_sector(vbe_mode_with_flags);
        let mut m = new_deterministic_test_machine(boot, enable_aerogpu);

        run_until_halt(&mut m);

        let vbe_status = (m.cpu().gpr[gpr::RAX] & 0xFFFF) as u16;
        assert_eq!(
            vbe_status, 0x004F,
            "VBE set-mode should return AX=0x004F (success)"
        );

        m.display_present();
        assert_eq!(m.display_resolution(), expected_res);

        let phys_base_ptr = m.read_physical_u32(0x0500 + 40);
        assert_ne!(phys_base_ptr, 0);

        // Write a red pixel at (0,0) in packed 32bpp BGRX.
        let base = m.vbe_lfb_base();
        assert_eq!(
            u64::from(phys_base_ptr),
            base,
            "BIOS VBE mode info PhysBasePtr must match Machine::vbe_lfb_base()"
        );
        m.write_physical_u32(base, 0x00FF_0000);

        m.display_present();
        assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
    }
}

#[test]
fn boot_int10_vbe_sets_mode_0x118_and_lfb_is_visible() {
    // Mode 0x118 + LFB requested.
    assert_vbe_mode_set_and_lfb_visible(0x4118, (1024, 768));
}
