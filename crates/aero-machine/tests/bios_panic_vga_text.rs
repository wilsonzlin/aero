use aero_machine::{Machine, MachineConfig, RunExit};
use aero_storage::SECTOR_SIZE;

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("machine did not halt within budget");
}

#[test]
fn bios_panic_renders_to_vga_text_memory_on_invalid_boot_signature() {
    // A single-sector "disk" with an invalid 0x55AA signature at the end of the boot sector.
    let disk = vec![0u8; SECTOR_SIZE];

    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();
    m.set_disk_image(disk).unwrap();
    m.reset();

    run_until_halt(&mut m);
    assert!(m.cpu().halted);

    // VGA text mode buffer: 80x25 cells, each cell is (ascii, attr).
    let vram = m.read_physical_bytes(0xB8000, 80 * 25 * 2);
    let text: Vec<u8> = vram.chunks_exact(2).map(|pair| pair[0]).collect();

    let needle = b"Invalid boot signature";
    assert!(
        text.windows(needle.len()).any(|w| w == needle),
        "expected VGA text to contain {:?}, got {:?}",
        std::str::from_utf8(needle).unwrap(),
        String::from_utf8_lossy(&text[..text.len().min(160)])
    );
}
