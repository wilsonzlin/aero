use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_pc_platform::PcPlatform;
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
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn read_e1000_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NIC_E1000_82540EM.bdf;
    let bar0 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

#[test]
fn pc_platform_reset_resets_e1000_mmio_state() {
    let mut pc = PcPlatform::new_with_e1000(2 * 1024 * 1024);
    let bar0_base = read_e1000_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    // E1000 CTRL (offset 0x00) should be writable through the PCI MMIO router.
    pc.memory.write_u32(bar0_base, 0x1111_2222);
    assert_eq!(pc.memory.read_u32(bar0_base), 0x1111_2222);

    // Reset the platform: E1000 should return to its power-on baseline (CTRL cleared).
    pc.reset();

    let bar0_base_after = read_e1000_bar0_base(&mut pc);
    assert_ne!(bar0_base_after, 0);
    assert_eq!(pc.memory.read_u32(bar0_base_after), 0);
}
