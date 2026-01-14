use aero_machine::{Machine, MachineConfig, RunExit};

fn build_outw_vga_sequencer_boot_sector() -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x3C4  ; VGA sequencer index port
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xC4, 0x03]);
    i += 3;
    // mov ax, 0x0F02 ; AL=0x02 (index), AH=0x0F (data)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x0F]);
    i += 3;
    // out dx, ax     ; write index+data in one OUTW
    sector[i] = 0xEF;
    i += 1;
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
fn boot_outw_vga_index_data() {
    let boot = build_outw_vga_sequencer_boot_sector();

    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        enable_aerogpu: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    // Read back sequencer reg 2 via standard byte accesses (index then data).
    m.io_write(0x3C4, 1, 0x02);
    let v = m.io_read(0x3C5, 1) as u8;
    assert_eq!(v, 0x0F);
}
