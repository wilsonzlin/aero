use aero_devices::pci::profile::{ISA_PIIX3, USB_UHCI_PIIX3};
use aero_pc_platform::PcPlatform;
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::DeviceId;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::*;
use aero_usb::uhci::UhciController;

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

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 4, value);
}

fn read_uhci_bar4_raw(pc: &mut PcPlatform) -> u32 {
    let bdf = USB_UHCI_PIIX3.bdf;
    read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x20)
}

fn read_uhci_bar4_base(pc: &mut PcPlatform) -> u16 {
    (read_uhci_bar4_raw(pc) & 0xffff_fffc) as u16
}

#[test]
fn pc_platform_exposes_piix3_multifunction_isa_bridge_for_uhci() {
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
    assert_ne!(
        header_type & 0x80,
        0,
        "PIIX3 function 0 should be multi-function"
    );
}

#[test]
fn pc_platform_enumerates_uhci_and_assigns_bar4() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;

    let id = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x00);
    assert_eq!(id & 0xffff, u32::from(USB_UHCI_PIIX3.vendor_id));
    assert_eq!((id >> 16) & 0xffff, u32::from(USB_UHCI_PIIX3.device_id));

    // Class code should be 0x0c0300 (serial bus / USB / UHCI).
    let class = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x08);
    assert_eq!((class >> 8) & 0x00ff_ffff, 0x000c_0300);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    assert_ne!(command & 0x1, 0, "BIOS POST should enable I/O decoding");

    let bar4_raw = read_uhci_bar4_raw(&mut pc);
    let bar4_base = read_uhci_bar4_base(&mut pc);
    assert_ne!(bar4_base, 0, "UHCI BAR4 should be assigned during BIOS POST");
    assert_eq!(bar4_base as u32 % 0x20, 0, "UHCI BAR4 must be 0x20-aligned");
    assert_ne!(bar4_raw & 0x1, 0, "UHCI BAR4 must be an I/O BAR (bit0=1)");

    // BAR size probing should report 0x20 bytes.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x20,
        0xffff_ffff,
    );
    assert_eq!(
        read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20),
        0xffff_ffe1
    );

    // Restore the original BAR value so subsequent tests can read it back normally.
    write_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x20, bar4_raw);
    assert_eq!(read_uhci_bar4_raw(&mut pc), bar4_raw);

    // Smoke test: SOFMOD defaults to 64 and should be writable via the programmed BAR.
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 64);
    pc.io.write(bar4_base + REG_SOFMOD, 1, 12);
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 12);
}

#[test]
fn pc_platform_routes_uhci_io_through_bar4() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // USBCMD defaults to MAXP=1 (64-byte packets).
    let usbcmd = pc.io.read(bar4_base + REG_USBCMD, 2) as u16;
    assert_ne!(usbcmd & USBCMD_MAXP, 0);

    // FRNUM should be readable/writable (masked to 11 bits).
    pc.io.write(bar4_base + REG_FRNUM, 2, 0x1234);
    let frnum = pc.io.read(bar4_base + REG_FRNUM, 2) as u16;
    assert_eq!(frnum, 0x1234 & 0x07ff);

    // Out-of-range reads behave like open bus.
    assert_eq!(pc.io.read(bar4_base + 0x40, 4), 0xffff_ffff);
}

#[test]
fn pc_platform_uhci_tick_advances_frnum_deterministically() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Start the controller running.
    pc.io
        .write(bar4_base + REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));

    // FRNUM starts at 0 and should advance by 1 per millisecond tick.
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 0);

    // Two half-ms ticks should add up to one UHCI frame.
    pc.tick(500_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 0);
    pc.tick(500_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 1);

    // Large deltas should advance multiple frames deterministically.
    pc.tick(10_000_000);
    assert_eq!(pc.io.read(bar4_base + REG_FRNUM, 2) as u16, 11);
}

#[test]
fn uhci_snapshot_roundtrip_restores_regs_and_port_state() {
    struct ZeroMem;

    impl aero_usb::MemoryBus for ZeroMem {
        fn read_physical(&mut self, _paddr: u64, buf: &mut [u8]) {
            buf.fill(0);
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
    }

    let mut ctrl = UhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    ctrl.hub_mut().force_enable_for_tests(0);

    ctrl.io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    ctrl.io_write(REG_FRNUM, 2, 0x0100);

    let mut mem = ZeroMem;
    ctrl.tick_1ms(&mut mem);

    let expected_frnum = ctrl.regs().frnum;
    let expected_portsc0 = ctrl.hub().read_portsc(0);

    let state = device_state_from_io_snapshot(DeviceId::USB, &ctrl);
    assert_eq!(state.id, DeviceId::USB);

    // Restore into a new controller with the same host-side topology (device attached to port 0).
    let mut restored = UhciController::new();
    restored
        .hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    restored.hub_mut().force_enable_for_tests(0);

    apply_io_snapshot_to_device(&state, &mut restored).unwrap();

    assert_eq!(restored.regs().frnum, expected_frnum);
    assert_eq!(restored.hub().read_portsc(0), expected_portsc0);
}
