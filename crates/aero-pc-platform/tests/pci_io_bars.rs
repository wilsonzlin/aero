use aero_devices::pci::{PciBarDefinition, PciBdf, PciConfigSpace, PciDevice};
use aero_pc_platform::PcPlatform;
use aero_platform::io::PortIoDevice;

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((bdf.bus as u32) << 16)
        | ((bdf.device as u32) << 11)
        | ((bdf.function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u32 {
    pc.io.write(0xCF8, 4, cfg_addr(bdf, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u16) {
    pc.io.write(0xCF8, 4, cfg_addr(bdf, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u32) {
    pc.io.write(0xCF8, 4, cfg_addr(bdf, offset));
    pc.io.write(0xCFC, 4, value);
}

fn find_free_bdf(pc: &mut PcPlatform) -> PciBdf {
    // Avoid low device numbers reserved for canonical chipset devices (PIIX3, AHCI, etc.).
    for dev in 12u8..32 {
        let bdf = PciBdf::new(0, dev, 0);
        let present = pc
            .pci_cfg
            .borrow_mut()
            .bus_mut()
            .device_config(bdf)
            .is_some();
        if !present {
            return bdf;
        }
    }
    panic!("no free PCI BDF found for test device");
}

struct TestPciConfigDevice {
    cfg: PciConfigSpace,
}

impl PciDevice for TestPciConfigDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[derive(Default)]
struct TestIoBar;

impl PortIoDevice for TestIoBar {
    fn read(&mut self, _port: u16, _size: u8) -> u32 {
        0
    }

    fn write(&mut self, _port: u16, _size: u8, _value: u32) {}
}

#[test]
fn pci_io_bar4_probe_returns_size_mask_and_relocation_updates_io_decode() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = find_free_bdf(&mut pc);

    // Add a PCI function that exposes a single 16-byte I/O BAR at BAR4.
    let mut cfg = PciConfigSpace::new(0x1234, 0x0001);
    cfg.set_bar_definition(4, PciBarDefinition::Io { size: 0x10 });
    pc.pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestPciConfigDevice { cfg }));

    // Register an I/O handler for BAR4 so we can observe I/O decode behavior.
    pc.register_pci_io_bar(bdf, 4, Box::new(TestIoBar));

    // Re-run BIOS POST so the new device gets BARs assigned and I/O decoding enabled.
    pc.reset_pci();

    // Initial BAR4 value is base|IO.
    let bar4 = read_cfg_u32(&mut pc, bdf, 0x20);
    assert_ne!(bar4, 0);
    assert_ne!(bar4 & 0x1, 0);
    let old_base = (bar4 & 0xFFFF_FFFC) as u16;
    assert_eq!(old_base % 0x10, 0, "BAR4 base should be 16-byte aligned");

    // With I/O decoding enabled, reads should hit the device.
    assert_eq!(pc.io.read(old_base, 1), 0);

    // Multi-byte port I/O should only decode when the full access fits inside the BAR.
    // (e.g. a 16-bit access starting at the last byte of a BAR should not decode.)
    assert_eq!(pc.io.read(old_base.wrapping_add(0x0c), 4), 0);
    assert_eq!(pc.io.read(old_base.wrapping_add(0x0f), 2), 0xFFFF);
    assert_eq!(pc.io.read(old_base.wrapping_add(0x0e), 4), 0xFFFF_FFFF);

    // Disable PCI I/O decoding: reads should float high.
    write_cfg_u16(&mut pc, bdf, 0x04, 0x0000);
    assert_eq!(pc.io.read(old_base, 1), 0xFF);

    // Re-enable decoding: reads should hit again.
    write_cfg_u16(&mut pc, bdf, 0x04, 0x0001);
    assert_eq!(pc.io.read(old_base, 1), 0);

    // Probe BAR4 size mask.
    write_cfg_u32(&mut pc, bdf, 0x20, 0xFFFF_FFFF);
    assert_eq!(read_cfg_u32(&mut pc, bdf, 0x20), 0xFFFF_FFF1);

    // Relocate to a new base within the platform's PCI I/O window.
    let new_base: u16 = 0xD000;
    write_cfg_u32(&mut pc, bdf, 0x20, u32::from(new_base));
    assert_eq!(read_cfg_u32(&mut pc, bdf, 0x20), u32::from(new_base) | 0x01);

    // The BAR decode should now only respond to the relocated base.
    assert_eq!(pc.io.read(old_base, 1), 0xFF);
    assert_eq!(pc.io.read(new_base, 1), 0);
}
