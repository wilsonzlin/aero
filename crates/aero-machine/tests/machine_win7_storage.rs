use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, NIC_E1000_82540EM, SATA_AHCI_ICH9};
use aero_machine::Machine;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(m: &mut Machine, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    m.io_write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    m.io_read(0xCFC, 4)
}

#[test]
fn machine_win7_storage_helper_enables_canonical_pci_storage_bdfs() {
    let mut m = Machine::new_with_win7_storage(2 * 1024 * 1024).unwrap();

    // AHCI at 00:02.0
    {
        let bdf = SATA_AHCI_ICH9.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(SATA_AHCI_ICH9.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(SATA_AHCI_ICH9.device_id));
    }

    // PIIX3 ISA bridge at 00:01.0 (function 0) with multi-function bit set.
    {
        let bdf = ISA_PIIX3.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(ISA_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(ISA_PIIX3.device_id));

        let header = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x0c);
        let header_type = ((header >> 16) & 0xff) as u8;
        assert_ne!(header_type & 0x80, 0, "PIIX3 function 0 should advertise multi-function");
    }

    // IDE at 00:01.1
    {
        let bdf = IDE_PIIX3.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id & 0xffff, u32::from(IDE_PIIX3.vendor_id));
        assert_eq!((id >> 16) & 0xffff, u32::from(IDE_PIIX3.device_id));
    }

    // E1000 is off by default for this helper.
    {
        let bdf = NIC_E1000_82540EM.bdf;
        let id = read_cfg_u32(&mut m, bdf.bus, bdf.device, bdf.function, 0x00);
        assert_eq!(id, 0xffff_ffff);
    }
}

