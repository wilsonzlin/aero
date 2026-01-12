use aero_devices::pci::profile::{HDA_ICH6, NIC_E1000_82540EM};
use aero_devices::pci::{PciResourceAllocatorConfig, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io
        .write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_bar0_base(pc: &mut PcPlatform, bdf: aero_devices::pci::PciBdf) -> u64 {
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

#[test]
fn pci_mmio_window_routes_multiple_devices_and_tracks_bar_reprogramming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_hda: true,
            enable_e1000: true,
            ..PcPlatformConfig::default()
        },
    );

    let hda_bdf = HDA_ICH6.bdf;
    let e1000_bdf = NIC_E1000_82540EM.bdf;

    let hda_base = read_bar0_base(&mut pc, hda_bdf);
    let e1000_base = read_bar0_base(&mut pc, e1000_bdf);
    assert_ne!(hda_base, 0);
    assert_ne!(e1000_base, 0);
    assert_ne!(hda_base, e1000_base);

    // Assert the BAR allocations don't overlap (sanity check for the allocator).
    let hda_end = hda_base + 0x4000u64;
    let e1000_end = e1000_base + 0x20_000u64;
    assert!(
        hda_end <= e1000_base || e1000_end <= hda_base,
        "HDA BAR0 ({hda_base:#x}..{hda_end:#x}) overlaps E1000 BAR0 ({e1000_base:#x}..{e1000_end:#x})"
    );

    // Touch state in both devices.
    pc.memory.write_u32(hda_base + 0x08, 1); // HDA: GCTL.CRST
    assert_ne!(pc.memory.read_u32(hda_base + 0x08) & 1, 0);

    // E1000 CTRL bit 26 triggers a device reset; choose a value that doesn't set it.
    pc.memory.write_u32(e1000_base, 0x1111_2222);
    assert_eq!(pc.memory.read_u32(e1000_base), 0x1111_2222);

    // Move both BARs to non-overlapping addresses within the PCI MMIO window.
    let alloc_cfg = PciResourceAllocatorConfig::default();
    let new_hda_base = alloc_cfg.mmio_base + 0x0030_0000;
    let new_e1000_base = alloc_cfg.mmio_base + 0x0040_0000;
    assert_eq!(new_hda_base % 0x4000, 0);
    assert_eq!(new_e1000_base % 0x20_000, 0);

    write_cfg_u32(
        &mut pc,
        hda_bdf.bus,
        hda_bdf.device,
        hda_bdf.function,
        0x10,
        new_hda_base as u32,
    );
    write_cfg_u32(
        &mut pc,
        e1000_bdf.bus,
        e1000_bdf.device,
        e1000_bdf.function,
        0x10,
        new_e1000_base as u32,
    );

    // Old bases should no longer decode.
    assert_eq!(pc.memory.read_u32(hda_base + 0x08), 0xFFFF_FFFF);
    assert_eq!(pc.memory.read_u32(e1000_base), 0xFFFF_FFFF);

    // New bases should decode and preserve state independently.
    assert_ne!(pc.memory.read_u32(new_hda_base + 0x08) & 1, 0);
    assert_eq!(pc.memory.read_u32(new_e1000_base), 0x1111_2222);
}
