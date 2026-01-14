use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciConfigPorts, PciConfigSpace, PciDevice, PciEcamConfig,
    PciEcamMmio, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use memory::Bus;
use std::cell::RefCell;
use std::rc::Rc;

mod bar_probe_masks;

use bar_probe_masks::mmio32_probe_mask;

const BAR0_SIZE: u32 = 0x1000;

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
                        size: BAR0_SIZE,
                        prefetchable: false,
                    },
                );
                cfg.set_bar_definition(1, PciBarDefinition::Io { size: 0x20 });
                // Interrupt Pin (0x3D) is read-only from the guest's perspective. Set it via the
                // device-facing API so we can assert that 32-bit writes to 0x3C do not clobber it.
                cfg.set_interrupt_pin(0x01);
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
    // The Interrupt Pin byte is read-only (and should not be clobbered by dword writes that
    // overlap Interrupt Line), so the second byte should remain at the device-programmed value.
    let expected = (0x1122_3344 & !0x0000_ff00) | (0x01u32 << 8);
    assert_eq!(reg_ports, expected);
    let reg_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x3C), 4) as u32;
    assert_eq!(reg_ecam, expected);

    // BAR0 probe/program should also be coherent between the two config mechanisms.
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x10));
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_DATA_PORT, 4, 0xFFFF_FFFF);
    let bar0_probe_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    let bar0_probe_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32;
    let expected_probe = mmio32_probe_mask(BAR0_SIZE, false);
    assert_eq!(bar0_probe_ports, expected_probe);
    assert_eq!(bar0_probe_ecam, expected_probe);

    // Program BAR0 using a single ECAM byte write to the highest byte (after probe). This should
    // not "inherit" the probe mask's low bits.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x13), 1, 0xE0);
    let bar0_ports = cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4);
    assert_eq!(bar0_ports, 0xE000_0000);
    let bar0_ecam = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32;
    assert_eq!(bar0_ecam, 0xE000_0000);
}

#[test]
fn pci_bar_probe_via_ecam_dword_write_sets_probe_flag() {
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
                        size: BAR0_SIZE,
                        prefetchable: false,
                    },
                );
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

    // Perform the BAR size probe via a single 32-bit ECAM store.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4, 0xFFFF_FFFF);

    // The guest-visible value is not sufficient to distinguish probe mode from a programmed base
    // of all ones (after masking to BAR alignment). Assert that the internal probe flag is set.
    let probe = {
        let mut ports = cfg_ports.borrow_mut();
        let cfg = ports.bus_mut().device_config(bdf).expect("device missing");
        cfg.snapshot_state().bar_probe[0]
    };
    assert!(probe, "ECAM dword write should enter BAR probe mode");
}

#[test]
fn pci_ecam_unaligned_cross_dword_bar_write_updates_both_bars() {
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
                        size: BAR0_SIZE,
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

    // Perform an unaligned 16-bit config write that straddles BAR0's last byte and BAR1's first
    // byte. This is possible via ECAM (MMCONFIG) and should be treated like a byte-enable access,
    // updating both BAR dwords rather than being dropped.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x13), 2, 0xA55A);

    // Byte 0x5A is written to BAR0[31:24]; BAR0 is MMIO32 so flags are 0 and the base is masked
    // to the BAR alignment.
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32,
        0x5A00_0000
    );

    // Byte 0xA5 is written to BAR1[7:0]. BAR1 is an IO BAR, so bit0 must remain set and the base
    // is masked to the IO window alignment.
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x14), 4) as u32,
        0x0000_00A1
    );

    // Verify the same values are observable via the legacy config mechanism #1 ports (shared bus).
    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x10));
    assert_eq!(
        cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4),
        0x5A00_0000
    );

    cfg_ports
        .borrow_mut()
        .io_write(PCI_CFG_ADDR_PORT, 4, cfg_addr(0, 2, 0, 0x14));
    assert_eq!(
        cfg_ports.borrow_mut().io_read(PCI_CFG_DATA_PORT, 4),
        0x0000_00A1
    );
}

#[test]
fn pci_mmio64_bar_probe_via_ecam_qword_write_is_observed() {
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

    // Pick a BAR size >4GiB so the probe mask's high dword is not trivially all ones.
    let bar0_size = 0x2_0000_0000u64; // 8GiB

    cfg_ports.borrow_mut().bus_mut().add_device(
        bdf,
        Box::new(Stub {
            cfg: {
                let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
                cfg.set_bar_definition(
                    0,
                    PciBarDefinition::Mmio64 {
                        size: bar0_size,
                        prefetchable: false,
                    },
                );
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

    // Probe the 64-bit BAR using a single 64-bit ECAM store of all ones.
    mem.write(ecam_addr(ecam_base, 0, 2, 0, 0x10), 8, u64::MAX);

    let expected_mask_lo = {
        let mut mask = (!(bar0_size.saturating_sub(1)) as u32) & 0xFFFF_FFF0;
        // bits 2:1 = 0b10 indicate 64-bit BAR.
        mask |= 0b10 << 1;
        mask
    };
    let expected_mask_hi = (!(bar0_size.saturating_sub(1)) >> 32) as u32;

    let lo = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x10), 4) as u32;
    let hi = mem.read(ecam_addr(ecam_base, 0, 2, 0, 0x14), 4) as u32;
    assert_eq!(lo, expected_mask_lo);
    assert_eq!(hi, expected_mask_hi);

    // Assert that the internal probe flag is set, not just a particular register value.
    let probe = {
        let mut ports = cfg_ports.borrow_mut();
        let cfg = ports.bus_mut().device_config(bdf).expect("device missing");
        cfg.snapshot_state().bar_probe[0]
    };
    assert!(probe, "ECAM qword write should enter BAR probe mode");
}
