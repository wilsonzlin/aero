use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_legacy_window_is_aliased_into_vram_aperture() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for this test.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        // Keep the machine minimal.
        enable_e1000: false,
        enable_virtio_net: false,
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

    // Legacy window bytes must be visible through the BAR1 VRAM aperture at offset
    // (paddr - 0xA0000).
    let off = 0xB8000u64 - 0xA0000u64;

    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);
    assert_eq!(m.read_physical_u8(bar1_base + off), b'A');
    assert_eq!(m.read_physical_u8(bar1_base + off + 1), 0x1F);

    // And the aliasing is bidirectional.
    m.write_physical_u8(bar1_base + off + 2, b'B');
    assert_eq!(m.read_physical_u8(0xB8002), b'B');
}
