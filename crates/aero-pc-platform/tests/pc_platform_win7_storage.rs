use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9};
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::PcPlatform;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

#[test]
fn pc_platform_win7_storage_has_ahci_and_ide_on_canonical_bdfs() {
    let mut pc = PcPlatform::new_with_win7_storage(2 * 1024 * 1024);

    // PIIX3 ISA bridge at 00:01.0. This function must set the multifunction bit so guests discover
    // the IDE function at 00:01.1.
    {
        let bdf = ISA_PIIX3.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(ISA_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(ISA_PIIX3.device_id));

        let header_type = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x0c);
        let header_type = ((header_type >> 16) & 0xff) as u8;
        assert_ne!(
            header_type & 0x80,
            0,
            "PIIX3 ISA should advertise multi-function"
        );
    }

    // AHCI at 00:02.0
    {
        let bdf = SATA_AHCI_ICH9.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(SATA_AHCI_ICH9.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(SATA_AHCI_ICH9.device_id));
    }

    // IDE at 00:01.1
    {
        let bdf = IDE_PIIX3.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));
    }

    // NVMe at 00:03.0 is optional and is off by default for Win7 (no inbox NVMe driver).
    {
        let bdf = NVME_CONTROLLER.bdf;
        let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id, 0xffff_ffff);
    }
}
