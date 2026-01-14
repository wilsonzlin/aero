use aero_machine::{Machine, MachineConfig};

#[test]
fn aerogpu_pci_device_exposes_expected_ids_and_bar0_mmio() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        // Keep the machine minimal/deterministic for this unit test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .expect("Machine::new");

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let bdf = aero_devices::pci::profile::AEROGPU.bdf;

    // PCI identity: A3A0:0001.
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        assert_eq!(bus.read_config(bdf, 0x00, 2) as u16, 0xA3A0);
        assert_eq!(bus.read_config(bdf, 0x02, 2) as u16, 0x0001);
    }

    // BAR0 MMIO routing: read MAGIC.
    let bar0_base = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .device_config(bdf)
            .and_then(|cfg| cfg.bar_range(0))
            .expect("missing AeroGPU BAR0")
            .base
    };
    assert_ne!(bar0_base, 0);

    let magic = m.read_physical_u32(bar0_base);
    assert_eq!(magic, 0x5550_4741);
}
