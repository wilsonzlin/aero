use aero_devices::pci::profile::{IDE_PIIX3, ISA_PIIX3, SATA_AHCI_ICH9};
use aero_devices::pci::PciBdf;
use aero_machine::Machine;
use pretty_assertions::assert_eq;

fn pci_cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn read_cfg_u32(m: &mut Machine, bdf: PciBdf, offset: u8) -> u32 {
    m.io_write(0xCF8, 4, pci_cfg_addr(bdf, offset));
    m.io_read(0xCFC, 4)
}

#[test]
fn machine_helper_enables_canonical_win7_storage_topology_pci_functions() {
    let mut m = Machine::new_with_win7_storage(2 * 1024 * 1024).unwrap();

    let ahci_id = read_cfg_u32(&mut m, SATA_AHCI_ICH9.bdf, 0x00);
    assert_eq!(ahci_id & 0xFFFF, u32::from(SATA_AHCI_ICH9.vendor_id));
    assert_eq!(ahci_id >> 16, u32::from(SATA_AHCI_ICH9.device_id));

    // IDE controller is a PIIX3 multi-function device; function 0 must exist so OSes enumerate
    // the IDE function at 00:01.1 (see `docs/05-storage-topology-win7.md`).
    let isa_id = read_cfg_u32(&mut m, ISA_PIIX3.bdf, 0x00);
    assert_eq!(isa_id & 0xFFFF, u32::from(ISA_PIIX3.vendor_id));
    assert_eq!(isa_id >> 16, u32::from(ISA_PIIX3.device_id));

    let ide_id = read_cfg_u32(&mut m, IDE_PIIX3.bdf, 0x00);
    assert_eq!(ide_id & 0xFFFF, u32::from(IDE_PIIX3.vendor_id));
    assert_eq!(ide_id >> 16, u32::from(IDE_PIIX3.device_id));
}
