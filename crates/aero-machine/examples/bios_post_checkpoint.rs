use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::SECTOR_SIZE;

fn build_serial_boot_sector(message: &[u8]) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
    let mut i = 0usize;

    // mov dx, 0x3f8
    sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
    i += 3;

    for &b in message {
        // mov al, imm8
        sector[i..i + 2].copy_from_slice(&[0xB0, b]);
        i += 2;
        // out dx, al
        sector[i] = 0xEE;
        i += 1;
    }

    // cli; hlt
    sector[i] = 0xFA;
    sector[i + 1] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn main() {
    let boot = build_serial_boot_sector(b"OK\n");
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    })
    .expect("machine construction failed");

    m.set_disk_image(boot.to_vec())
        .expect("disk image must be a multiple of 512 bytes");
    m.reset();

    loop {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => break,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }

    let out = m.take_serial_output();
    print!("{}", String::from_utf8_lossy(&out));
}
