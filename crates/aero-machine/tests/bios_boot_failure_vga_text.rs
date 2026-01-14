use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

#[test]
fn bios_boot_failure_renders_message_to_vga_text_memory() {
    // Keep the machine minimal: no PC platform/PCI, but with the legacy VGA window mapped.
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    // Sector 0 exists but is missing the 0x55AA signature.
    let bad_boot_sector = [0u8; aero_storage::SECTOR_SIZE];
    m.set_disk_image(bad_boot_sector.to_vec()).unwrap();
    m.reset();

    // BIOS should have halted the CPU during POST.
    assert!(matches!(m.run_slice(1), RunExit::Halted { .. }));

    let expected = b"Invalid boot signature";
    let vga = m.read_physical_bytes(0xB8000, expected.len() * 2);

    let rendered: Vec<u8> = vga.iter().copied().step_by(2).collect();
    assert_eq!(&rendered, expected);

    for attr in vga.iter().copied().skip(1).step_by(2) {
        assert_eq!(attr, 0x07);
    }
}
