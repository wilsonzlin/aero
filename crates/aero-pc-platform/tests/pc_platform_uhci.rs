use aero_devices::pci::profile::{ISA_PIIX3, USB_UHCI_PIIX3};
use aero_devices::usb::uhci::regs;
use aero_pc_platform::PcPlatform;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn read_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    pc.io.write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 2) as u16
}

#[test]
fn pc_platform_enumerates_uhci_and_routes_bar4_io() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Ensure function 0 exists and is marked multi-function so OSes will probe function 2.
    let isa_hdr = read_cfg_u32(
        &mut pc,
        ISA_PIIX3.bdf.bus,
        ISA_PIIX3.bdf.device,
        ISA_PIIX3.bdf.function,
        0x0c,
    );
    let header_type = ((isa_hdr >> 16) & 0xff) as u8;
    assert_ne!(header_type & 0x80, 0, "PIIX3 function 0 should be multi-function");

    let bdf = USB_UHCI_PIIX3.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(USB_UHCI_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(USB_UHCI_PIIX3.device_id));

    let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!((class >> 8) & 0x00ff_ffff, 0x000c_0300);

    // BIOS POST should enable I/O decoding.
    let command = read_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04);
    assert_ne!(command & 0x1, 0);

    // BAR4 should be assigned and aligned.
    let bar4 = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20);
    assert_ne!(bar4 & !0x3, 0);
    let io_base = (bar4 & 0xffff_fffc) as u16;
    assert_eq!(
        (io_base as u32) % 0x20,
        0,
        "UHCI BAR4 must be 0x20-aligned"
    );

    // Smoke test: SOFMOD defaults to 64 and should be writable via the programmed BAR.
    assert_eq!(pc.io.read(io_base + regs::REG_SOFMOD, 1) as u8, 64);
    pc.io.write(io_base + regs::REG_SOFMOD, 1, 12);
    assert_eq!(pc.io.read(io_base + regs::REG_SOFMOD, 1) as u8, 12);
}
