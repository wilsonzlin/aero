use emulator::devices::vga::{VgaDevice, VgaPlanarShift};
use emulator::io::PortIO;

#[test]
fn seq_index_and_data_ports_work() {
    let mut vga = VgaDevice::new();

    vga.port_write(0x3C4, 1, 0x02);
    assert_eq!(vga.port_read(0x3C4, 1) as u8, 0x02);

    vga.port_write(0x3C5, 1, 0xAA);
    assert_eq!(vga.port_read(0x3C5, 1) as u8, 0xAA);
}

#[test]
fn gc_index_and_data_ports_work() {
    let mut vga = VgaDevice::new();

    vga.port_write(0x3CE, 1, 0x06);
    assert_eq!(vga.port_read(0x3CE, 1) as u8, 0x06);

    vga.port_write(0x3CF, 1, 0x12);
    assert_eq!(vga.port_read(0x3CF, 1) as u8, 0x12);
}

#[test]
fn crtc_ports_follow_misc_output_io_select() {
    let mut vga = VgaDevice::new();

    // Default: colour I/O decode (0x3D4/0x3D5 active).
    vga.port_write(0x3D4, 1, 0x0E);
    vga.port_write(0x3D5, 1, 0x34);
    assert_eq!(vga.port_read(0x3D5, 1) as u8, 0x34);

    // Writes to the inactive mono ports must not affect the active CRTC set.
    vga.port_write(0x3B4, 1, 0x0E);
    vga.port_write(0x3B5, 1, 0x56);
    assert_eq!(vga.port_read(0x3D5, 1) as u8, 0x34);

    // Switch to mono I/O decode.
    vga.port_write(0x3C2, 1, 0x00);
    vga.port_write(0x3B4, 1, 0x0E);
    vga.port_write(0x3B5, 1, 0x77);
    assert_eq!(vga.port_read(0x3B5, 1) as u8, 0x77);
}

#[test]
fn attribute_flip_flop_resets_on_input_status_read() {
    let mut vga = VgaDevice::new();

    // Put AC into "data phase" by writing an index.
    vga.port_write(0x3C0, 1, 0x10);

    // Reading Input Status 1 resets the flip-flop back to index phase.
    vga.port_read(0x3DA, 1);

    // Now this write should be treated as an index, not data for 0x10.
    vga.port_write(0x3C0, 1, 0x0F);
    vga.port_write(0x3C0, 1, 0x22);

    assert_eq!(vga.port_read(0x3C1, 1) as u8, 0x22);

    // Register 0x10 should remain at its default value.
    vga.port_read(0x3DA, 1);
    vga.port_write(0x3C0, 1, 0x10);
    assert_eq!(vga.port_read(0x3C1, 1) as u8, 0x0C);
}

#[test]
fn input_status1_vertical_retrace_bit_changes() {
    let mut vga = VgaDevice::new();

    let a = vga.port_read(0x3DA, 1) as u8;
    vga.tick(vga.timing().frame_ns().saturating_sub(vga.timing().vblank_ns()));
    let b = vga.port_read(0x3DA, 1) as u8;

    assert_ne!(a & 0x08, b & 0x08);
}

#[test]
fn input_status1_inactive_port_does_not_reset_attribute_flip_flop() {
    let mut vga = VgaDevice::new();

    // Default mode 3 uses colour I/O decode, so 0x3BA is inactive.
    vga.port_write(0x3C0, 1, 0x10);
    vga.port_read(0x3BA, 1);

    // Still in data phase: this should write AC register 0x10, not set a new index.
    vga.port_write(0x3C0, 1, 0xAA);
    assert_eq!(vga.port_read(0x3C1, 1) as u8, 0xAA);
}

#[test]
fn out_of_range_indices_do_not_panic() {
    let mut vga = VgaDevice::new();

    vga.port_write(0x3C4, 1, 0xFF);
    vga.port_write(0x3C5, 1, 0xAA);
    assert_eq!(vga.port_read(0x3C5, 1) as u8, 0xAA);

    vga.port_write(0x3CE, 1, 0xFE);
    vga.port_write(0x3CF, 1, 0xBB);
    assert_eq!(vga.port_read(0x3CF, 1) as u8, 0xBB);

    vga.port_write(0x3D4, 1, 0xFC);
    vga.port_write(0x3D5, 1, 0xCC);
    assert_eq!(vga.port_read(0x3D5, 1) as u8, 0xCC);
}

#[test]
fn derived_state_tracks_memory_mode_chain4_and_odd_even() {
    let mut vga = VgaDevice::new();

    // SEQ Memory Mode register: index 0x04.
    vga.port_write(0x3C4, 1, 0x04);
    vga.port_write(0x3C5, 1, 0x08); // chain4=1, odd/even=1? (bit2=0)

    let state = vga.derived_state();
    assert!(state.chain4);
    assert!(state.odd_even);

    vga.port_write(0x3C5, 1, 0x0C); // chain4=1, odd/even disabled (bit2=1)
    let state = vga.derived_state();
    assert!(state.chain4);
    assert!(!state.odd_even);
}

#[test]
fn derived_state_detects_graphics_and_shift_controls() {
    let mut vga = VgaDevice::new();

    // GC Misc register: index 0x06; bit0 indicates graphics mode.
    vga.port_write(0x3CE, 1, 0x06);
    vga.port_write(0x3CF, 1, 0x01);
    assert!(vga.derived_state().is_graphics);

    // GC Mode register: index 0x05; bits 6:5 control shift.
    vga.port_write(0x3CE, 1, 0x05);
    vga.port_write(0x3CF, 1, 0x20); // shift_control=1
    assert_eq!(vga.derived_state().planar_shift, VgaPlanarShift::Shift256);
    assert_eq!(vga.derived_state().bpp_guess, 8);
}

#[test]
fn unimplemented_vga_ports_return_ff() {
    let vga = VgaDevice::new();

    // 0x3CB/0x3CD are reserved/unassigned in standard VGA I/O maps.
    assert_eq!(vga.port_read(0x3CB, 1) as u8, 0xFF);
    assert_eq!(vga.port_read(0x3CD, 1) as u8, 0xFF);
}
