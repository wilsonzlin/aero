use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

fn build_int10_vbe_get_controller_info_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
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

    // mov ax, 0x4F00 (VBE Get Controller Info)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0x4F]);
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

#[test]
fn boot_int10_vbe_4f00_mode_list_contains_required_modes() {
    let boot = build_int10_vbe_get_controller_info_boot_sector();

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Controller info block is written to 0x0000:0x0500 by the boot sector.
    let info_addr = 0x0500u64;
    let sig = m.read_physical_bytes(info_addr, 4);
    assert_eq!(&sig, b"VESA");

    // VideoModePtr at offset 0x0E.
    let mode_ptr = m.read_physical_u32(info_addr + 14);
    let seg = (mode_ptr >> 16) as u16;
    let off = (mode_ptr & 0xFFFF) as u16;
    let mode_list_paddr = (u64::from(seg) << 4).wrapping_add(u64::from(off));

    let mut modes = Vec::new();
    for i in 0..128u64 {
        let mode = m.read_physical_u16(mode_list_paddr + i * 2);
        if mode == 0xFFFF {
            break;
        }
        modes.push(mode);
    }

    // Required boot modes per docs/16-aerogpu-vga-vesa-compat.md.
    assert!(modes.contains(&0x115));
    assert!(modes.contains(&0x118));
    assert!(modes.contains(&0x160));
}
