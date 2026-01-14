use aero_devices::pci::{
    PciBdf, PciConfigPorts, PciConfigSpace, PciDevice, PciEcamConfig, PciEcamMmio,
};
use memory::Bus;
use std::cell::RefCell;
use std::rc::Rc;

fn ecam_addr(base: u64, bus: u8, device: u8, function: u8, offset: u16) -> u64 {
    base + (u64::from(bus) << 20)
        + (u64::from(device) << 15)
        + (u64::from(function) << 12)
        + u64::from(offset)
}

#[test]
fn pci_ecam_extended_reads_are_zero_for_present_and_all_ones_for_absent_functions() {
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

    // Populate the very end of the 256-byte config space with a recognizable pattern so reads that
    // straddle 0xFF/0x100 can be validated.
    let bdf_present = PciBdf::new(0, 2, 0);
    cfg_ports.borrow_mut().bus_mut().add_device(
        bdf_present,
        Box::new(Stub {
            cfg: {
                let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
                // bytes[0xFC..=0xFF] = 0x11 0x22 0x33 0x44
                cfg.write(0xFC, 4, 0x4433_2211);
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

    // For a present function, config bytes >= 0x100 are treated as zero-filled.
    for off in 0x100u16..0x120u16 {
        for size in [1usize, 2, 4] {
            let value = mem.read(ecam_addr(ecam_base, 0, 2, 0, off), size);
            assert_eq!(
                value, 0,
                "present function read at offset {off:#x} size {size} returned {value:#x}",
            );
        }
    }

    // Multi-byte reads that straddle the 0xFF/0x100 boundary should return real config bytes in
    // the low part, and zeros in the high part.
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0xFC), 4) as u32,
        0x4433_2211,
        "sanity check: pattern should be readable from implemented config space",
    );
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0xFD), 4) as u32,
        0x0044_3322,
        "4-byte read at 0xFD should include bytes 0xFD..0xFF plus a zero byte at 0x100",
    );
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0xFE), 4) as u32,
        0x0000_4433,
        "4-byte read at 0xFE should include bytes 0xFE..0xFF plus two zero bytes at 0x100..0x101",
    );
    assert_eq!(
        mem.read(ecam_addr(ecam_base, 0, 2, 0, 0xFF), 2) as u16,
        0x0044,
        "2-byte read at 0xFF should include byte 0xFF plus a zero byte at 0x100",
    );

    // For an absent function, config space reads should float high (all-ones) regardless of
    // whether the access is in the first 256 bytes or beyond.
    let bdf_absent = PciBdf::new(0, 2, 1);
    for off in [0x00u16, 0x10u16, 0x120u16] {
        for size in [1usize, 2, 4] {
            let expected: u64 = match size {
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => unreachable!(),
            };
            let value = mem.read(
                ecam_addr(
                    ecam_base,
                    bdf_absent.bus,
                    bdf_absent.device,
                    bdf_absent.function,
                    off,
                ),
                size,
            );
            assert_eq!(
                value, expected,
                "absent function read at offset {off:#x} size {size} returned {value:#x}",
            );
        }
    }
}
