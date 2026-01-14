use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_bar0_base_is_allocated_by_bios_post() {
    let cfg = MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the test minimal and deterministic.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let m = Machine::new(cfg).expect("machine should construct");
    assert!(m.aerogpu().is_some(), "expected aerogpu handle when enabled");

    let bar0 = m
        .aerogpu_bar0_base()
        .expect("expected AeroGPU BAR0 base after BIOS POST");
    assert_ne!(bar0, 0, "AeroGPU BAR0 base should be non-zero");
}

