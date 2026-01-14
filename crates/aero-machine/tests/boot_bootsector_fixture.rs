use aero_machine::{Machine, MachineConfig, RunExit};
use pretty_assertions::assert_eq;

const BOOTSECTOR_BIN: &[u8] = include_bytes!("../../../tests/fixtures/bootsector.bin");

fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected run exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn boots_bootsector_fixture_and_validates_vga_mode13_and_serial_output() {
    // Keep the machine deterministic and minimal for a fast, stable end-to-end boot test.
    //
    // We only need VGA + COM1 + the BIOS disk path; disable unrelated legacy devices.
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_vga: true,
        enable_aerogpu: false,
        enable_serial: true,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(BOOTSECTOR_BIN.to_vec()).unwrap();
    m.reset();

    run_until_halt(&mut m);

    let serial = m.take_serial_output();
    let serial_str = String::from_utf8_lossy(&serial);
    assert!(
        serial_str.contains("AERO_BOOTSECTOR_OK"),
        "serial output did not contain expected substring: {serial_str:?}"
    );

    m.display_present();
    assert_eq!(m.display_resolution(), (320, 200));
    let fb = m.display_framebuffer();
    let w = 320usize;
    let (top_left, top_right, bottom_left, bottom_right) = (
        fb[0],
        fb[w - 1],
        fb[(200 - 1) * w],
        fb[(200 - 1) * w + (w - 1)],
    );

    // The bootsector fills VRAM with:
    // - palette index 0x11 in the top half, and
    // - palette index 0x22 in the bottom half.
    //
    // In the default VGA 6x6x6 color cube palette:
    // - 0x11 = (r=0, g=0, b=1) => RGB(0, 0, 51)
    // - 0x22 = (r=0, g=3, b=0) => RGB(0, 153, 0)
    let expected_top = rgba(0x00, 0x00, 0x33);
    let expected_bottom = rgba(0x00, 0x99, 0x00);

    assert_eq!(top_left, expected_top);
    assert_eq!(top_right, expected_top);
    assert_eq!(bottom_left, expected_bottom);
    assert_eq!(bottom_right, expected_bottom);
    assert_ne!(top_left, bottom_left);
}
