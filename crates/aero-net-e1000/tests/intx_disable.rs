use aero_net_e1000::{E1000Device, ICR_TXDW};

#[test]
fn e1000_irq_level_is_gated_by_pci_command_intx_disable() {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    // Enable TXDW interrupts and force a cause bit so the device asserts its IRQ line.
    dev.mmio_write_u32_reg(0x00D0, ICR_TXDW); // IMS
    dev.mmio_write_u32_reg(0x00C8, ICR_TXDW); // ICS

    dev.pci_config_write(0x04, 2, 0x0004); // COMMAND.BME
    assert!(
        dev.irq_level(),
        "IRQ should assert when an interrupt cause is pending"
    );

    // PCI command bit 10 disables legacy INTx assertion.
    dev.pci_config_write(0x04, 2, 0x0004 | (1 << 10));
    assert!(
        !dev.irq_level(),
        "IRQ must be suppressed when PCI COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx without touching E1000 register state: the pending interrupt should become
    // visible again.
    dev.pci_config_write(0x04, 2, 0x0004);
    assert!(dev.irq_level());
}
