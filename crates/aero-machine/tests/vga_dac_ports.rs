use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

#[test]
fn vga_dac_ports_program_palette_entry_and_read_back() {
    // The VGA port window is exposed in multiple machine configurations:
    // - `enable_vga=true`: standalone VGA/VBE device model (with or without the PC platform).
    // - `enable_aerogpu=true`: canonical AeroGPU device, which must also decode legacy VGA ports.
    //
    // Exercise each wiring path to ensure DAC ports always behave plausibly (Windows probes these).

    // Standalone VGA/VBE device model.
    for enable_pc_platform in [false, true] {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_pc_platform,
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

        run_dac_port_roundtrip(&mut m, /*expect_display_palette*/ true);
    }

    // AeroGPU legacy VGA port decode path (enable_aerogpu=true).
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();
    run_dac_port_roundtrip(&mut m, /*expect_display_palette*/ true);
}

fn run_dac_port_roundtrip(m: &mut Machine, expect_display_palette: bool) {
    // PEL mask is a simple R/W storage register.
    m.io_write(0x3C6, 1, 0x5A);
    assert_eq!(m.io_read(0x3C6, 1) as u8, 0x5A);

    // Program two consecutive palette entries starting at index 0x2A:
    // - first entry uses 8-bit component writes (a common guest behavior),
    // - second entry uses classic VGA 6-bit component writes.
    let base_index: u8 = 0x2A;
    m.io_write(0x3C8, 1, base_index as u32);

    // Entry 0x2A: 8-bit RGB. Intentionally include components <= 63 so we exercise the
    // downscaling behavior even for "dark" 8-bit values.
    let (r8, g8, b8) = (0x20u8, 0x10u8, 0xFFu8);
    m.io_write(0x3C9, 1, r8 as u32);
    m.io_write(0x3C9, 1, g8 as u32);
    m.io_write(0x3C9, 1, b8 as u32);

    // Entry 0x2B: 6-bit RGB.
    let (r6, g6, b6) = (0x01u8, 0x02u8, 0x03u8);
    m.io_write(0x3C9, 1, r6 as u32);
    m.io_write(0x3C9, 1, g6 as u32);
    m.io_write(0x3C9, 1, b6 as u32);

    // Write index auto-increments after every 3 components.
    assert_eq!(m.io_read(0x3C8, 1) as u8, base_index.wrapping_add(2));

    // Read back both entries via DAC read mode.
    m.io_write(0x3C7, 1, base_index as u32);
    let out_r0 = m.io_read(0x3C9, 1) as u8;
    let out_g0 = m.io_read(0x3C9, 1) as u8;
    let out_b0 = m.io_read(0x3C9, 1) as u8;
    let out_r1 = m.io_read(0x3C9, 1) as u8;
    let out_g1 = m.io_read(0x3C9, 1) as u8;
    let out_b1 = m.io_read(0x3C9, 1) as u8;

    // Aero's VGA frontend stores palette entries as 6-bit values; 8-bit writes are
    // downscaled via `>> 2` (matching what most VGA software does explicitly).
    assert_eq!([out_r0, out_g0, out_b0], [r8 >> 2, g8 >> 2, b8 >> 2]);
    assert_eq!([out_r1, out_g1, out_b1], [r6, g6, b6]);

    // Read index auto-increments after every 3 components.
    assert_eq!(m.io_read(0x3C7, 1) as u8, base_index.wrapping_add(2));

    if !expect_display_palette {
        return;
    }

    // ---------------------------------------------------------------------
    // Optional: ensure palette programming affects visible text mode output.
    // ---------------------------------------------------------------------
    // Use a fully-enabled PEL mask so index 1 is not masked away.
    m.io_write(0x3C6, 1, 0xFF);

    // Reprogram palette entry 1 (normally blue) to pure red using classic 6-bit values.
    m.io_write(0x3C8, 1, 0x01);
    m.io_write(0x3C9, 1, 63); // R
    m.io_write(0x3C9, 1, 0); // G
    m.io_write(0x3C9, 1, 0); // B

    // Put a blank cell with background color 1 at the top-left.
    m.write_physical_u8(0xB8000, b' ');
    m.write_physical_u8(0xB8001, 0x10); // bg=1, fg=0

    m.display_present();
    assert_eq!(m.display_resolution(), (720, 400));
    // RGBA8888 little-endian u32: [R, G, B, A].
    assert_eq!(m.display_framebuffer()[0], 0xFF00_00FF);
}
