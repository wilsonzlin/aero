use aero_cpu_core::mem::CpuBus;
use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_devices::pci::{PciBarKind, PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::{PcCpuBus, PcPlatform};
use memory::MemoryBus as _;

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((bdf.bus as u32) << 16)
        | ((bdf.device as u32) << 11)
        | ((bdf.function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(bus: &mut PcCpuBus, bdf: PciBdf, offset: u8) -> u32 {
    bus.io_write(PCI_CFG_ADDR_PORT, 4, u64::from(cfg_addr(bdf, offset)))
        .unwrap();
    bus.io_read(PCI_CFG_DATA_PORT, 4).unwrap() as u32
}

fn write_cfg_u32(bus: &mut PcCpuBus, bdf: PciBdf, offset: u8, value: u32) {
    bus.io_write(PCI_CFG_ADDR_PORT, 4, u64::from(cfg_addr(bdf, offset)))
        .unwrap();
    bus.io_write(PCI_CFG_DATA_PORT, 4, u64::from(value))
        .unwrap();
}

#[test]
fn pc_cpu_bus_routes_pci_io_bar_and_tracks_bar_reprogramming() {
    let platform = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let mut bus = PcCpuBus::new(platform);

    let bdf = NIC_E1000_82540EM.bdf;

    let bar0_base = read_cfg_u32(&mut bus, bdf, 0x10) as u64 & 0xffff_fff0;
    assert_ne!(bar0_base, 0);

    let bar1 = read_cfg_u32(&mut bus, bdf, 0x14);
    let bar1_base = (bar1 & 0xffff_fffc) as u16;
    assert_ne!(bar1_base, 0);
    assert_eq!(bar1_base % 0x40, 0);

    let status_mmio = bus.platform.memory.read_u32(bar0_base + 0x08);
    assert_ne!(status_mmio, 0xffff_ffff);

    // Program IOADDR at BAR1 base.
    bus.io_write(bar1_base, 4, 0x0000_0008).unwrap();
    let status_io = bus.io_read(bar1_base + 0x04, 4).unwrap() as u32;
    assert_eq!(status_io, status_mmio);

    // Move BAR1 within the platform's PCI I/O window and ensure the CPU bus routing follows.
    let new_base = bar1_base.wrapping_add(0x80);
    assert_eq!(new_base % 0x40, 0);
    write_cfg_u32(&mut bus, bdf, 0x14, u32::from(new_base));

    let bar1_after = read_cfg_u32(&mut bus, bdf, 0x14);
    let bar1_range = {
        let mut pci_cfg = bus.platform.pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(bdf)
            .expect("e1000 config should exist");
        cfg.bar_range(1).expect("BAR1 should exist")
    };
    assert_eq!(bar1_range.kind, PciBarKind::Io);
    assert_eq!(bar1_range.base, u64::from(new_base));
    assert_eq!(bar1_after & 0xffff_fffc, u32::from(new_base));

    // Old base should no longer decode.
    let old_base_status = bus.io_read(bar1_base + 0x04, 4).unwrap() as u32;
    assert_eq!(old_base_status, 0xffff_ffff);

    // New base should decode via the CPU bus PCI I/O BAR router.
    bus.io_write(new_base, 4, 0x0000_0008).unwrap();
    let new_status = bus.io_read(new_base + 0x04, 4).unwrap() as u32;
    assert_eq!(new_status, status_mmio);
}
