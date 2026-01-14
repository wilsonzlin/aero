use aero_machine::{Machine, MachineConfig, RunExit};

fn build_int16_read_key_boot_sector(store_addr: u16) -> [u8; aero_storage::SECTOR_SIZE] {
    // Boot sector program:
    //   cli
    //   xor ax, ax
    //   mov ds, ax
    //   mov es, ax
    //   mov ss, ax
    //   mov sp, 0x7c00
    //   int 0x16        ; AH=00h: read keystroke
    //   mov [store_addr], ax
    //   hlt
    //
    // BIOS loads the boot sector at 0x0000:0x7C00 and sets CS=DS=ES=SS=0.
    let mut code: Vec<u8> = Vec::new();
    code.push(0xFA); // cli
    code.extend_from_slice(&[0x31, 0xC0]); // xor ax, ax
    code.extend_from_slice(&[0x8E, 0xD8]); // mov ds, ax
    code.extend_from_slice(&[0x8E, 0xC0]); // mov es, ax
    code.extend_from_slice(&[0x8E, 0xD0]); // mov ss, ax
    code.extend_from_slice(&[0xBC, 0x00, 0x7C]); // mov sp, 0x7c00
    code.extend_from_slice(&[0xCD, 0x16]); // int 0x16
    code.push(0xA3); // mov [imm16], ax
    code.extend_from_slice(&store_addr.to_le_bytes());
    code.push(0xF4); // hlt

    assert!(
        code.len() <= 510,
        "boot sector too large: {} bytes",
        code.len()
    );

    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    sector[..code.len()].copy_from_slice(&code);
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
fn inject_bios_key_is_observable_via_int16_read() {
    const STORE_ADDR: u64 = 0x0500;
    let boot = build_int16_read_key_boot_sector(STORE_ADDR as u16);

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_serial: false,
        enable_i8042: false,
        enable_vga: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();

    // BIOS key word for 'A': scan code 0x1E, ASCII 0x41.
    let key: u16 = 0x1E41;
    m.inject_bios_key(key);

    // Clear the destination first so a failure isn't masked by stale bytes.
    m.write_physical_u16(STORE_ADDR, 0);

    run_until_halt(&mut m);

    let got = m.read_physical_u16(STORE_ADDR);
    assert_eq!(got, key);
}
