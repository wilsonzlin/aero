use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_bar1_and_legacy_vga_window_alias_the_same_vram() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal; AeroGPU owns the legacy VGA window in this configuration.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let bdf = m
        .aerogpu_bdf()
        .expect("AeroGPU should be present when enable_aerogpu=true");
    let bar1_base = m
        .pci_bar_base(bdf, aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
        .unwrap_or(0);
    assert_ne!(
        bar1_base, 0,
        "AeroGPU BAR1 base should be assigned by BIOS POST"
    );

    // Write a deterministic pattern through the legacy VGA window and ensure BAR1 reads observe it.
    let legacy_base = 0xA0000u64;
    for n in 0u64..256 {
        let v = (n as u8).wrapping_mul(37).wrapping_add(0x5A);
        m.write_physical_u8(legacy_base + n, v);
    }
    for n in 0u64..256 {
        let expected = (n as u8).wrapping_mul(37).wrapping_add(0x5A);
        let got = m.read_physical_u8(bar1_base + n);
        assert_eq!(got, expected, "mismatch at VRAM offset 0x{n:04x}");
    }
}
