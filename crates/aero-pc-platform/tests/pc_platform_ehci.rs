use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::pci::{PciBdf, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};
use memory::MemoryBus as _;

// Keep the relocated BAR within the platform's default PCI MMIO window
// (0xE000_0000..0xF000_0000) and away from other devices that BIOS POST might have allocated near
// the start of the window.
const EHCI_BAR0_RELOC_BASE: u64 = 0xE200_0000;

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
    pc.io.write(PCI_CFG_DATA_PORT + u16::from(offset & 2), 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bdf: PciBdf, offset: u8, value: u32) {
    pc.io.write(PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_bar0_base(pc: &mut PcPlatform, bdf: PciBdf) -> u64 {
    let bar0 = read_cfg_u32(pc, bdf, 0x10);
    u64::from(bar0 & 0xffff_fff0)
}

#[test]
fn pc_platform_ehci_bar0_size_probe_reports_expected_mask() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_ehci: true,
            ..Default::default()
        },
    );

    let bdf = USB_EHCI_ICH9.bdf;

    // Standard PCI BAR size probing: write all 1s, then read back the size mask.
    write_cfg_u32(&mut pc, bdf, 0x10, 0xffff_ffff);
    let got = read_cfg_u32(&mut pc, bdf, 0x10);

    // EHCI BAR0 is a 4KiB non-prefetchable MMIO window; the expected mask is 0xFFFF_F000.
    assert_eq!(got, 0xffff_f000);
}

#[test]
fn pc_platform_gates_ehci_mmio_on_pci_command_mem_bit() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_ehci: true,
            ..Default::default()
        },
    );

    let bdf = USB_EHCI_ICH9.bdf;
    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(bar0_base, 0, "EHCI BAR0 should be allocated during BIOS POST");

    // Enable MEM decoding so BAR0 MMIO is routed (BIOS POST may already do this).
    let cmd = read_cfg_u16(&mut pc, bdf, 0x04) | 0x0002;
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);

    // USBCMD is at offset 0x20 (operational register block starts at CAPLENGTH=0x20).
    let usbcmd_addr = bar0_base + 0x20;
    pc.memory.write_u32(usbcmd_addr, 0xA5A5_5A5A);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), 0xA5A5_5A5A);

    // Disable memory decoding: reads float high and writes are ignored.
    write_cfg_u16(&mut pc, bdf, 0x04, cmd & !0x0002);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), 0xffff_ffff);
    pc.memory.write_u32(usbcmd_addr, 0);

    // Re-enable decoding: the write above must not have reached the device.
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);
    assert_eq!(pc.memory.read_u32(usbcmd_addr), 0xA5A5_5A5A);
}

#[test]
fn pc_platform_routes_ehci_mmio_after_bar0_reprogramming() {
    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            enable_ahci: false,
            enable_uhci: false,
            enable_ehci: true,
            ..Default::default()
        },
    );

    let bdf = USB_EHCI_ICH9.bdf;

    // Enable MEM decoding so BAR0 MMIO is routed.
    let cmd = read_cfg_u16(&mut pc, bdf, 0x04) | 0x0002;
    write_cfg_u16(&mut pc, bdf, 0x04, cmd);

    let bar0_base = read_bar0_base(&mut pc, bdf);
    assert_ne!(bar0_base, 0, "EHCI BAR0 should be allocated during BIOS POST");

    let usbcmd_off = 0x20;
    pc.memory
        .write_u32(bar0_base + usbcmd_off, 0x1234_5678);
    assert_eq!(pc.memory.read_u32(bar0_base + usbcmd_off), 0x1234_5678);

    // Move BAR0 within the PCI MMIO window.
    let new_base = if bar0_base == EHCI_BAR0_RELOC_BASE {
        EHCI_BAR0_RELOC_BASE + 0x1000
    } else {
        EHCI_BAR0_RELOC_BASE
    };
    write_cfg_u32(&mut pc, bdf, 0x10, new_base as u32);

    // Old base should no longer decode.
    assert_eq!(pc.memory.read_u32(bar0_base + usbcmd_off), 0xffff_ffff);

    // New base should decode and preserve register state.
    assert_eq!(pc.memory.read_u32(new_base + usbcmd_off), 0x1234_5678);
    pc.memory.write_u32(new_base + usbcmd_off, 0xDEAD_BEEF);
    assert_eq!(pc.memory.read_u32(new_base + usbcmd_off), 0xDEAD_BEEF);
}

