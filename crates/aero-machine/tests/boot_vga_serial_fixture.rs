use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

// Reuse the fixture source-of-truth for expected bytes so the test stays in sync with
// `cargo xtask fixtures`.
#[path = "../../../tests/fixtures/boot/boot_vga_serial.rs"]
mod boot_vga_serial;

const BOOT_VGA_SERIAL_BIN: &[u8] =
    include_bytes!("../../../tests/fixtures/boot/boot_vga_serial.bin");

#[test]
fn boots_fixture_and_captures_vga_text_and_serial_bytes() {
    // Keep the machine deterministic and minimal for a fast, stable end-to-end test.
    //
    // We only need VGA + COM1 + the BIOS disk path; disable unrelated devices.
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_serial: true,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(BOOT_VGA_SERIAL_BIN.to_vec()).unwrap();
    m.reset();

    // The fixture loops forever (`jmp $`) after emitting its output, so run a bounded instruction
    // budget and then validate the resulting guest-visible state.
    match m.run_slice(1_000) {
        RunExit::Completed { .. } => {}
        other => panic!("unexpected run exit: {other:?}"),
    }

    let serial = m.take_serial_output();
    assert_eq!(&serial[..], &boot_vga_serial::EXPECTED_SERIAL_BYTES[..]);

    let vga = m.read_physical_bytes(0xB8000, boot_vga_serial::EXPECTED_VGA_TEXT_BYTES.len());
    assert_eq!(&vga[..], &boot_vga_serial::EXPECTED_VGA_TEXT_BYTES[..]);
}
