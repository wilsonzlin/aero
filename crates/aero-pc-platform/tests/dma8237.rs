use aero_pc_platform::PcPlatform;

#[test]
fn pc_platform_registers_dma8237_ports_and_resets_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Unmapped ports float high; verify an adjacent port outside the DMA range behaves as open bus.
    assert_eq!(pc.io.read_u8(0x10), 0xFF);

    // DMA controller ports should be registered and default to 0 (register file stub).
    assert_eq!(pc.io.read_u8(0x00), 0);
    assert_eq!(pc.io.read_u8(0x08), 0);
    assert_eq!(pc.io.read_u8(0x80), 0);
    assert_eq!(pc.io.read_u8(0xC0), 0);

    // Writes should be observable on subsequent reads, including multi-byte access sizes.
    pc.io.write_u8(0x00, 0x12);
    pc.io.write_u8(0x01, 0x34);
    assert_eq!(pc.io.read_u8(0x00), 0x12);
    assert_eq!(pc.io.read(0x00, 2) as u16, 0x3412);

    // Platform reset should clear the DMA controller state for deterministic power-on behavior.
    pc.reset();
    assert_eq!(pc.io.read_u8(0x00), 0);
    assert_eq!(pc.io.read(0x00, 2) as u16, 0);
}
