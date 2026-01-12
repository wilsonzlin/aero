use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn pci_cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

#[test]
fn snapshot_round_trip_preserves_pci_config_ports_and_interrupt_controller_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Mutate PCI config space through the standard 0xCF8/0xCFC config ports.
    //
    // Use the host bridge at 00:00.0 and change the COMMAND register (offset 0x04).
    let cfg_addr = pci_cfg_addr(0, 0, 0, 0x04);
    src.io_write(0xCF8, 4, cfg_addr);
    let command: u16 = 0x0007;
    src.io_write(0xCFC, 2, u32::from(command));
    assert_eq!(src.io_read(0xCFC, 2) as u16, command);

    // Mutate interrupt controller state.
    //
    // - Change the PIC master interrupt mask (port 0x21).
    // - Switch IMCR to APIC mode via ports 0x22/0x23 (and keep the selector latched).
    let pic_mask: u8 = 0xAB;
    src.io_write(0x21, 1, u32::from(pic_mask));
    assert_eq!(src.io_read(0x21, 1) as u8, pic_mask);

    src.io_write(0x22, 1, 0x70);
    src.io_write(0x23, 1, 0x01);
    assert_eq!(src.io_read(0x23, 1) as u8, 0x01);

    let snap = src.take_snapshot_full().unwrap();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    // Confirm the PCI config address register and the modified COMMAND value survived snapshot.
    assert_eq!(restored.io_read(0xCFC, 2) as u16, command);

    // Confirm PIC mask survived.
    assert_eq!(restored.io_read(0x21, 1) as u8, pic_mask);

    // Confirm IMCR select+data survived (read depends on latched selector).
    assert_eq!(restored.io_read(0x23, 1) as u8, 0x01);
}
