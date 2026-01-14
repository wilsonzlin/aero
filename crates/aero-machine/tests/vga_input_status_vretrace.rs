use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_input_status1_vertical_retrace_bit_tracks_vblank_cadence() {
    // Legacy real-mode code frequently polls VGA Input Status 1 (0x3DA) bit 3 ("vertical retrace")
    // as a crude 60Hz timing source. Ensure we present a deterministic vblank pulse that toggles
    // with a stable cadence as guest time advances.
    fn run_case(label: &str, cfg: MachineConfig) {
        let mut m = Machine::new(cfg).unwrap();

        // Sample at 0.1ms resolution for 50ms (~3 frames at 60Hz).
        const STEP_NS: u64 = 100_000;
        const SAMPLES: usize = 500;

        let mut vretrace = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let color = m.io_read(0x3DA, 1) as u8;
            let mono = m.io_read(0x3BA, 1) as u8;

            let b3_color = (color & 0x08) != 0;
            let b3_mono = (mono & 0x08) != 0;
            assert_eq!(
                b3_color, b3_mono,
                "{label}: mono alias (0x3BA) should report the same vblank state as 0x3DA"
            );

            vretrace.push(b3_color);
            m.tick_platform(STEP_NS);
        }

        assert!(
            vretrace.iter().any(|&b| b),
            "{label}: expected to observe retrace bit set at least once"
        );
        assert!(
            vretrace.iter().any(|&b| !b),
            "{label}: expected to observe retrace bit clear at least once"
        );

        // Detect rising edges (0 -> 1) and ensure they're roughly periodic.
        let mut rising_edges_ns = Vec::new();
        let mut prev = vretrace[0];
        for (i, &b) in vretrace.iter().enumerate().skip(1) {
            if !prev && b {
                rising_edges_ns.push(i as u64 * STEP_NS);
            }
            prev = b;
        }
        assert!(
            rising_edges_ns.len() >= 2,
            "{label}: expected multiple vblank pulses (rising edges), got {}",
            rising_edges_ns.len()
        );

        // At 60Hz, the frame period is ~16.67ms. Allow wide tolerance so this test remains robust
        // even if the exact period/pulse width are tweaked.
        for win in rising_edges_ns.windows(2) {
            let dt = win[1] - win[0];
            assert!(
                (10_000_000..=25_000_000).contains(&dt),
                "{label}: unexpected vblank cadence: dt={dt}ns"
            );
        }

        // Ensure the asserted window is a small pulse, not stuck high.
        let mut max_run = 0usize;
        let mut cur_run = 0usize;
        for &b in &vretrace {
            if b {
                cur_run += 1;
                max_run = max_run.max(cur_run);
            } else {
                cur_run = 0;
            }
        }
        let max_run_ns = max_run as u64 * STEP_NS;
        assert!(
            max_run_ns < 5_000_000,
            "{label}: expected a short vblank pulse, but saw {max_run_ns}ns asserted window"
        );
    }

    let base_cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        // Keep the machine minimal/deterministic for this timing test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        // Make the VGA-focused cases robust to potential future changes in `MachineConfig` defaults
        // (e.g. if AeroGPU becomes default-enabled).
        enable_aerogpu: false,
        ..Default::default()
    };

    // VGA device model (no PC platform).
    run_case(
        "vga/no-pc-platform",
        MachineConfig {
            enable_pc_platform: false,
            enable_vga: true,
            ..base_cfg
        },
    );

    // VGA device model (PC platform enabled; VGA presented via PCI MMIO router).
    run_case(
        "vga/pc-platform",
        MachineConfig {
            enable_pc_platform: true,
            enable_vga: true,
            ..base_cfg
        },
    );

    // AeroGPU legacy VGA port window (0x3B0..0x3DF), including Input Status 1 at 0x3DA/0x3BA.
    run_case(
        "aerogpu/pc-platform",
        MachineConfig {
            enable_pc_platform: true,
            enable_aerogpu: true,
            enable_vga: false,
            ..base_cfg
        },
    );
}
