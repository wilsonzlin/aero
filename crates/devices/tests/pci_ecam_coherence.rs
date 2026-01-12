use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciConfigPorts, PciConfigSpace, PciDevice, PciEcamConfig,
    PciEcamMmio, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use memory::Bus;
use std::cell::RefCell;
use std::rc::Rc;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn ecam_addr(base: u64, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
    base + (u64::from(bus) << 20)
        + (u64::from(device) << 15)
        + (u64::from(function) << 12)
        + u64::from(offset)
}

#[test]
fn pci_config_space_is_coherent_between_mech1_ports_and_ecam_mmio() {
    struct Stub {
        cfg: PciConfigSpace,
    }

    impl PciDevice for Stub {
        fn config(&self) -> &PciConfigSpace {
            &self.cfg
        }

        fn config_mut(&mut self) -> &mut PciConfigSpace {
            &mut self.cfg
        }
    }

    let cfg_ports = Rc::new(RefCell::new(PciConfigPorts::new()));
    let bdf = PciBdf::new(0, 2, 0);

    cfg_ports.borrow_mut().bus_mut().add_device(
        bdf,
        Box::new(Stub {
            cfg: {
                let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
                cfg.set_bar_definition(
                    0,
                    PciBarDefinition::Mmio32 {
                        size: 0x1000,
                        prefetchable: false,
                    },
                );
                cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
                cfg
            },
        }),
    );

    let ecam_base = 0xC000_0000;
    let ecam_cfg = PciEcamConfig {
        segment: 0,
        start_bus: 0,
        end_bus: 0,
    };

    let mut mem = Bus::new(0);
    mem.map_mmio(
        ecam_base,
        ecam_cfg.window_size_bytes(),
        Box::new(PciEcamMmio::new(cfg_ports.clone(), ecam_cfg)),
    );

    // Vendor/device ID is visible through both mechanisms.
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x00));
    let id_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    let id_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x00), 4) as u32;
    assert_eq!(id_ports, 0x5678_1234);
    assert_eq!(id_ecam, 0x5678_1234);

    // Write command register through config mechanism #1, read it back via ECAM.
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x04));
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_DATA_PORT, 2, 0x0005);
    let cmd_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x04), 2) as u16;
    assert_eq!(cmd_ecam, 0x0005);

    // Write interrupt line register through ECAM, read it back through ports.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x3C), 1, 0x44);
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x3C));
    let line_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 1) as u8;
    assert_eq!(line_ports, 0x44);

    // And verify a 4-byte write remains coherent.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x3C), 4, 0x1122_3344);
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x3C));
    let reg_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    assert_eq!(reg_ports, 0x1122_3344);

    // BAR0 probe/program should also be coherent between the two config mechanisms.
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x10));
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_DATA_PORT, 4, 0xFFFF_FFFF);
    let bar0_probe_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    let bar0_probe_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32;
    assert_eq!(bar0_probe_ports, 0xFFFF_F000);
    assert_eq!(bar0_probe_ecam, 0xFFFF_F000);

    // Program BAR0 using a single ECAM byte write to the highest byte (after probe). This should
    // not "inherit" the probe mask's low bits.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x13), 1, 0xE0);
    let bar0_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    assert_eq!(bar0_ports, 0xE000_0000);
    let bar0_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32;
    assert_eq!(bar0_ecam, 0xE000_0000);
}
