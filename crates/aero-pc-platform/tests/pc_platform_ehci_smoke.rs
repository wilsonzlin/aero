use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use memory::MemoryBus as _;

fn cfg_addr(bdf: PciBdf, offset: u8) -> u32 {
    0x8000_0000
        | ((bdf.bus as u32) << 16)
        | ((bdf.device as u32) << 11)
        | ((bdf.function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u32 {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8) -> u16 {
    let shift = (offset & 2) * 8;
    (read_cfg_u32(pc, bdf, offset) >> shift) as u16
}

fn write_cfg_u16(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u16) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    // PCI config writes to 0xCFC always write a dword; the platform will apply byte enables based
    // on the port access size (2 here).
    pc.io.write(
        PCI_CFG_DATA_PORT + u16::from(offset & 2),
        2,
        u32::from(value),
    );
}

fn read_bar0_base(pc: &mut PcPlatform, bdf: PciBdf) -> u64 {
    let bar0 = read_cfg_u32(pc, bdf, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

#[test]
fn pc_platform_ehci_enumerates_and_routes_mmio() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            // Keep the test focused: disable other large MMIO devices.
            enable_ahci: false,
            enable_uhci: false,
            enable_ehci: true,
            ..Default::default()
        },
    );

    let bdf = USB_EHCI_ICH9.bdf;

    let id = read_cfg_u32(&mut pc, bdf, 0x00);
    assert_eq!(id & 0xffff, u32::from(USB_EHCI_ICH9.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(USB_EHCI_ICH9.device_id));

    // Ensure MEM decoding is enabled so BAR0 MMIO is routed.
    let mut cmd = read_cfg_u16(&mut pc, bdf, 0x04);
    cmd |= 0x0002; // MEM
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 should be allocated during BIOS POST"
    );

    // Capability registers: CAPLENGTH=0x20, HCIVERSION=0x0100.
    assert_eq!(pc.memory.read_u32(bar0_base), 0x0100_0020);
    assert_ne!(
        pc.memory.read_u32(bar0_base + 0x04) & 0xf,
        0,
        "EHCI should report at least one root port"
    );

    // Tick should be able to advance the platform without panicking when EHCI is present.
    pc.tick(1_000_000);
}
