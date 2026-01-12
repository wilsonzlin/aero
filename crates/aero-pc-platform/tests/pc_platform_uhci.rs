use aero_devices::pci::profile::{ISA_PIIX3, USB_UHCI_PIIX3};
use aero_devices::pci::{
    PciInterruptPin, PciIntxRouter, PciIntxRouterConfig, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_interrupts::apic::IOAPIC_MMIO_BASE;
use aero_pc_platform::PcPlatform;
use aero_platform::interrupts::{
    InterruptController, PlatformInterruptMode, IMCR_DATA_PORT, IMCR_INDEX, IMCR_SELECT_PORT,
};
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::DeviceId;
use aero_usb::hid::composite::UsbCompositeHidInputHandle;
use aero_usb::hid::gamepad::UsbHidGamepadHandle;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::mouse::UsbHidMouseHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::regs::*;
use aero_usb::uhci::UhciController;
use memory::MemoryBus as _;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;
const TD1: u32 = 0x3020;
const TD2: u32 = 0x3040;
const BUF_SETUP: u32 = 0x4000;
const BUF_INT: u32 = 0x5000;
const BUF_CTRL: u32 = 0x6000;

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_STATUS_STALLED: u32 = 1 << 22;
const TD_STATUS_DATA_BUFFER_ERROR: u32 = 1 << 21;
const TD_STATUS_NAK: u32 = 1 << 19;
const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_SPD: u32 = 1 << 29;

// UHCI root hub PORTSC bits (Intel UHCI spec / Linux uhci-hcd).
const PORTSC_CSC: u16 = 0x0002;
const PORTSC_PED: u16 = 0x0004;
const PORTSC_PR: u16 = 0x0200;
const PORTSC_SUSP: u16 = 0x1000;

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

fn read_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    let port = PCI_CFG_DATA_PORT + u16::from(offset & 3);
    pc.io.read(port, 1) as u8
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_uhci_bar4_raw(pc: &mut PcPlatform) -> u32 {
    let bdf = USB_UHCI_PIIX3.bdf;
    read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x20)
}

fn read_uhci_bar4_base(pc: &mut PcPlatform) -> u16 {
    (read_uhci_bar4_raw(pc) & 0xffff_fffc) as u16
}

fn program_ioapic_entry(pc: &mut PcPlatform, gsi: u32, low: u32, high: u32) {
    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, low);
    pc.memory.write_u32(IOAPIC_MMIO_BASE, redtbl_high);
    pc.memory.write_u32(IOAPIC_MMIO_BASE + 0x10, high);
}

fn td_token(pid: u8, addr: u8, ep: u8, toggle: u8, max_len: usize) -> u32 {
    let max_field = if max_len == 0 {
        0x7ffu32
    } else {
        (max_len as u32) - 1
    };
    (pid as u32)
        | ((addr as u32) << 8)
        | ((ep as u32) << 15)
        | ((toggle as u32) << 19)
        | (max_field << 21)
}

fn td_status(active: bool) -> u32 {
    let mut v = 0x7ffu32;
    if active {
        v |= TD_STATUS_ACTIVE;
    }
    v
}

fn td_actlen(ctrl_sts: u32) -> usize {
    let field = ctrl_sts & 0x7ff;
    if field == 0x7ff {
        0
    } else {
        (field as usize) + 1
    }
}

fn write_td(pc: &mut PcPlatform, addr: u32, link: u32, status: u32, token: u32, buffer: u32) {
    pc.memory.write_u32(addr as u64, link);
    pc.memory.write_u32(addr.wrapping_add(4) as u64, status);
    pc.memory.write_u32(addr.wrapping_add(8) as u64, token);
    pc.memory.write_u32(addr.wrapping_add(12) as u64, buffer);
}

fn write_qh(pc: &mut PcPlatform, elem: u32) {
    pc.memory.write_u32(QH_ADDR as u64, 1); // horiz terminate
    pc.memory.write_u32(QH_ADDR.wrapping_add(4) as u64, elem);
}

fn init_frame_list(pc: &mut PcPlatform) {
    for i in 0..1024u32 {
        pc.memory
            .write_u32((FRAME_LIST_BASE + i * 4) as u64, QH_ADDR | 0x2);
    }
}

fn run_one_frame(pc: &mut PcPlatform, first_td: u32) {
    write_qh(pc, first_td);
    pc.tick(1_000_000);
}

fn read_portsc(pc: &mut PcPlatform, bar4_base: u16, portsc_offset: u16) -> u16 {
    pc.io.read(bar4_base + portsc_offset, 2) as u16
}

fn write_portsc(pc: &mut PcPlatform, bar4_base: u16, portsc_offset: u16, value: u16) {
    pc.io.write(bar4_base + portsc_offset, 2, u32::from(value));
}

fn write_portsc_w1c(pc: &mut PcPlatform, bar4_base: u16, portsc_offset: u16, w1c: u16) {
    // Preserve the port enable bit when clearing change bits, matching the usual
    // read-modify-write pattern of UHCI drivers.
    let cur = read_portsc(pc, bar4_base, portsc_offset);
    let value = (cur & PORTSC_PED) | w1c;
    write_portsc(pc, bar4_base, portsc_offset, value);
}

fn reset_port(pc: &mut PcPlatform, bar4_base: u16, portsc_offset: u16) {
    // Clear connection status change if present.
    if read_portsc(pc, bar4_base, portsc_offset) & PORTSC_CSC != 0 {
        write_portsc_w1c(pc, bar4_base, portsc_offset, PORTSC_CSC);
    }

    // Trigger port reset and wait the UHCI-mandated ~50ms.
    write_portsc(pc, bar4_base, portsc_offset, PORTSC_PR);
    pc.tick(50_000_000);
}

fn control_no_data(pc: &mut PcPlatform, devaddr: u8, setup: [u8; 8]) {
    pc.memory.write_physical(BUF_SETUP as u64, &setup);
    write_td(
        pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, devaddr, 0, 0, 8),
        BUF_SETUP,
    );
    // Status stage: IN ZLP, DATA1.
    write_td(
        pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, devaddr, 0, 1, 0),
        0,
    );
    run_one_frame(pc, TD0);

    const ERR_MASK: u32 = TD_STATUS_STALLED | TD_STATUS_DATA_BUFFER_ERROR | TD_STATUS_CRC_TIMEOUT;

    let st0 = pc.memory.read_u32(TD0 as u64 + 4);
    let st1 = pc.memory.read_u32(TD1 as u64 + 4);

    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st0 & ERR_MASK, 0, "setup TD should complete without error");
    assert_eq!(st1 & ERR_MASK, 0, "status TD should complete without error");
}

fn control_in(pc: &mut PcPlatform, devaddr: u8, setup: [u8; 8], data_buf: u32) -> Vec<u8> {
    let w_length = u16::from_le_bytes([setup[6], setup[7]]) as usize;
    assert!(w_length > 0, "control_in helper requires wLength>0");

    pc.memory.write_physical(BUF_SETUP as u64, &setup);
    write_td(
        pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, devaddr, 0, 0, 8),
        BUF_SETUP,
    );
    // DATA stage: IN, DATA1.
    write_td(
        pc,
        TD1,
        TD2,
        td_status(true),
        td_token(PID_IN, devaddr, 0, 1, w_length),
        data_buf,
    );
    // Status stage: OUT ZLP, DATA1.
    write_td(
        pc,
        TD2,
        1,
        td_status(true),
        td_token(PID_OUT, devaddr, 0, 1, 0),
        0,
    );
    run_one_frame(pc, TD0);

    const ERR_MASK: u32 = TD_STATUS_STALLED | TD_STATUS_DATA_BUFFER_ERROR | TD_STATUS_CRC_TIMEOUT;
    let st0 = pc.memory.read_u32(TD0 as u64 + 4);
    let st1 = pc.memory.read_u32(TD1 as u64 + 4);
    let st2 = pc.memory.read_u32(TD2 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st2 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st0 & ERR_MASK, 0, "setup TD should complete without error");
    assert_eq!(st1 & ERR_MASK, 0, "data TD should complete without error");
    assert_eq!(st2 & ERR_MASK, 0, "status TD should complete without error");

    let got = td_actlen(st1);
    let mut out = vec![0u8; got];
    pc.memory.read_physical(data_buf as u64, &mut out);
    out
}

fn control_out(pc: &mut PcPlatform, devaddr: u8, setup: [u8; 8], data: &[u8]) {
    let w_length = u16::from_le_bytes([setup[6], setup[7]]) as usize;
    assert_eq!(
        data.len(),
        w_length,
        "control_out helper expects data.len == wLength"
    );

    pc.memory.write_physical(BUF_SETUP as u64, &setup);
    pc.memory.write_physical(BUF_CTRL as u64, data);

    write_td(
        pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, devaddr, 0, 0, 8),
        BUF_SETUP,
    );
    // DATA stage: OUT, DATA1.
    write_td(
        pc,
        TD1,
        TD2,
        td_status(true),
        td_token(PID_OUT, devaddr, 0, 1, data.len()),
        BUF_CTRL,
    );
    // Status stage: IN ZLP, DATA1.
    write_td(
        pc,
        TD2,
        1,
        td_status(true),
        td_token(PID_IN, devaddr, 0, 1, 0),
        0,
    );
    run_one_frame(pc, TD0);

    const ERR_MASK: u32 = TD_STATUS_STALLED | TD_STATUS_DATA_BUFFER_ERROR | TD_STATUS_CRC_TIMEOUT;
    let st0 = pc.memory.read_u32(TD0 as u64 + 4);
    let st1 = pc.memory.read_u32(TD1 as u64 + 4);
    let st2 = pc.memory.read_u32(TD2 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st2 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st0 & ERR_MASK, 0, "setup TD should complete without error");
    assert_eq!(st1 & ERR_MASK, 0, "data TD should complete without error");
    assert_eq!(st2 & ERR_MASK, 0, "status TD should complete without error");
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
    assert_ne!(
        bar4_base, 0,
        "UHCI BAR4 should be assigned during BIOS POST"
    );
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
fn pc_platform_sets_uhci_intx_line_and_pin_registers() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;

    let line = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
    let pin = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);

    let expected_pin = USB_UHCI_PIIX3
        .interrupt_pin
        .expect("profile should provide interrupt pin");
    assert_eq!(pin, expected_pin.to_config_u8());

    let router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let expected_gsi = router.gsi_for_intx(bdf, expected_pin);
    assert_eq!(line, u8::try_from(expected_gsi).unwrap());
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
fn pc_platform_gates_uhci_io_on_pci_command_register() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Ensure I/O decoding is enabled initially and writes take effect.
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 64);
    pc.io.write(bar4_base + REG_SOFMOD, 1, 12);
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 12);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    assert_ne!(
        command & 0x1,
        0,
        "UHCI should have I/O decoding enabled by BIOS POST"
    );

    // Disable I/O decoding (bit 0): reads should float high and writes should be ignored.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command & !0x1,
    );
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1), 0xFF);
    pc.io.write(bar4_base + REG_SOFMOD, 1, 34);

    // Re-enable I/O decoding; the write above must not have reached the device.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | 0x1,
    );
    assert_eq!(pc.io.read(bar4_base + REG_SOFMOD, 1) as u8, 12);
}

#[test]
fn pc_platform_routes_uhci_io_after_bar4_reprogramming() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;

    let old_base = read_uhci_bar4_base(&mut pc);
    assert_ne!(old_base, 0);

    // Write a recognizable value via the initial BAR.
    pc.io.write(old_base + REG_SOFMOD, 1, 12);
    assert_eq!(pc.io.read(old_base + REG_SOFMOD, 1) as u8, 12);

    // Relocate BAR4 within the platform's PCI I/O window (size 0x20, alignment 0x20).
    let new_base = old_base.wrapping_add(0x200);
    assert_eq!(new_base as u32 % 0x20, 0);
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x20,
        u32::from(new_base),
    );
    assert_eq!(read_uhci_bar4_base(&mut pc), new_base);

    // Old base should no longer decode.
    assert_eq!(pc.io.read(old_base + REG_SOFMOD, 1), 0xFF);

    // New base should decode and preserve device register state.
    assert_eq!(pc.io.read(new_base + REG_SOFMOD, 1) as u8, 12);
    pc.io.write(new_base + REG_SOFMOD, 1, 34);
    assert_eq!(pc.io.read(new_base + REG_SOFMOD, 1) as u8, 34);
}

#[test]
fn pc_platform_uhci_tick_advances_frnum_deterministically() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Start the controller running.
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

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

#[test]
fn pc_platform_uhci_dma_writes_mark_dirty_pages_when_enabled() {
    let mut pc = PcPlatform::new_with_dirty_tracking(2 * 1024 * 1024);
    let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
    let bdf = USB_UHCI_PIIX3.bdf;

    // Enable Bus Mastering so UHCI DMA reaches guest memory.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    // Propagate the updated PCI command register into the UHCI model; the platform maintains
    // a separate canonical config space for enumeration.
    pc.tick(0);

    const FRAME_LIST_BASE: u64 = 0x3000;
    const TD_ADDR: u64 = 0x4000;
    const LINK_PTR_TERMINATE: u32 = 1;
    const TD_STATUS_ACTIVE: u32 = 1 << 23;
    const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;

    // Frame list entry 0 points at our test TD.
    pc.memory
        .write_physical(FRAME_LIST_BASE, &(TD_ADDR as u32).to_le_bytes());

    // TD layout: link, status/control, token, buffer.
    pc.memory
        .write_physical(TD_ADDR, &LINK_PTR_TERMINATE.to_le_bytes());
    pc.memory
        .write_physical(TD_ADDR + 4, &TD_STATUS_ACTIVE.to_le_bytes());

    // Token: PID=IN (0x69), dev_addr=1 (no device attached), endpoint=0, max_len_field=0x7ff.
    let token = 0x69u32 | (1u32 << 8) | (0x7ffu32 << 21);
    pc.memory.write_physical(TD_ADDR + 8, &token.to_le_bytes());
    pc.memory.write_physical(TD_ADDR + 12, &0u32.to_le_bytes());

    {
        // Program controller registers directly; I/O routing is tested separately.
        let mut dev = uhci.borrow_mut();
        dev.controller_mut()
            .io_write(REG_FLBASEADD, 4, FRAME_LIST_BASE as u32);
        dev.controller_mut().io_write(REG_FRNUM, 2, 0);
        dev.controller_mut()
            .io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    }

    // Clear dirty tracking for CPU-initiated setup writes; we want to observe only the writes the
    // UHCI scheduler performs when completing the TD.
    pc.memory.clear_dirty();

    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_eq!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should be completed (active bit cleared)"
    );
    assert_ne!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should record a CRC/timeout error when no device is attached"
    );

    let page_size = u64::from(pc.memory.dirty_page_size());
    let expected_page = TD_ADDR / page_size;

    let dirty = pc
        .memory
        .take_dirty_pages()
        .expect("dirty tracking enabled");
    assert!(
        dirty.contains(&expected_page),
        "dirty pages should include TD page (got {dirty:?})"
    );
}

#[test]
fn pc_platform_gates_uhci_dma_on_pci_bus_master_enable() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
    let bdf = USB_UHCI_PIIX3.bdf;

    const FRAME_LIST_BASE: u64 = 0x3000;
    const TD_ADDR: u64 = 0x4000;
    const LINK_PTR_TERMINATE: u32 = 1;
    const TD_STATUS_ACTIVE: u32 = 1 << 23;
    const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;

    // Frame list entry 0 points at our test TD.
    pc.memory
        .write_physical(FRAME_LIST_BASE, &(TD_ADDR as u32).to_le_bytes());

    // TD layout: link, status/control, token, buffer.
    pc.memory
        .write_physical(TD_ADDR, &LINK_PTR_TERMINATE.to_le_bytes());
    pc.memory
        .write_physical(TD_ADDR + 4, &TD_STATUS_ACTIVE.to_le_bytes());

    // Token: PID=IN (0x69), dev_addr=1 (no device attached), endpoint=0, max_len_field=0x7ff.
    let token = 0x69u32 | (1u32 << 8) | (0x7ffu32 << 21);
    pc.memory.write_physical(TD_ADDR + 8, &token.to_le_bytes());
    pc.memory.write_physical(TD_ADDR + 12, &0u32.to_le_bytes());

    // Disable Bus Mastering (but keep I/O decoding enabled) so UHCI DMA is gated off.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    let command_no_bme = command & !(1 << 2);
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command_no_bme,
    );
    // Propagate the updated PCI command register into the UHCI model.
    pc.tick(0);

    // Program controller registers directly; I/O routing is tested elsewhere.
    {
        let mut dev = uhci.borrow_mut();
        dev.controller_mut()
            .io_write(REG_FLBASEADD, 4, FRAME_LIST_BASE as u32);
        dev.controller_mut().io_write(REG_FRNUM, 2, 0);
        dev.controller_mut()
            .io_write(REG_USBCMD, 2, u32::from(USBCMD_RS | USBCMD_MAXP));
    }

    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_ne!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should remain active when DMA is gated off"
    );
    assert_eq!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should not be updated when DMA is gated off"
    );

    // Reset FRNUM so the next tick uses frame list entry 0 again.
    uhci.borrow_mut().controller_mut().io_write(REG_FRNUM, 2, 0);

    // Enable bus mastering and retry; the pending TD should now be processed.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command_no_bme | (1 << 2),
    );
    pc.tick(0);
    uhci.borrow_mut().tick_1ms(&mut pc.memory);

    let status = pc.memory.read_u32(TD_ADDR + 4);
    assert_eq!(
        status & TD_STATUS_ACTIVE,
        0,
        "TD should complete once bus mastering is enabled"
    );
    assert_ne!(
        status & TD_STATUS_CRC_TIMEOUT,
        0,
        "TD should record a CRC/timeout error when no device is attached"
    );
}

#[test]
fn pc_platform_routes_uhci_intx_via_pic_in_legacy_mode() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc
        .pci_intx
        .gsi_for_intx(USB_UHCI_PIIX3.bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe the UHCI interrupt through the
    // legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable IOC interrupts in the UHCI controller.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));

    // Force a USBINT status bit so the controller asserts its IRQ line.
    {
        let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
        uhci.borrow_mut()
            .controller_mut()
            .set_usbsts_bits(USBSTS_USBINT);
    }

    pc.poll_pci_intx_lines();

    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after UHCI asserts INTx");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI the interrupt so we can observe deassertion cleanly.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the status bit (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_respects_pci_interrupt_disable_bit_for_uhci_intx() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe the UHCI interrupt through the
    // legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable IOC interrupts in the UHCI controller and force a USBINT status bit so the controller
    // asserts its IRQ line.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));
    {
        let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
        uhci.borrow_mut()
            .controller_mut()
            .set_usbsts_bits(USBSTS_USBINT);
        assert!(uhci.borrow().irq_level());
    }

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;

    // Disable INTx in PCI command register (bit 10) while leaving I/O decode enabled.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 10),
    );
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts.borrow().pic().get_pending_vector(),
        None,
        "INTx should be suppressed when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx and ensure the asserted line is delivered.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command & !(1 << 10),
    );
    pc.poll_pci_intx_lines();
    assert_eq!(
        pc.interrupts
            .borrow()
            .pic()
            .get_pending_vector()
            .and_then(|v| pc.interrupts.borrow().pic().vector_to_irq(v)),
        Some(irq)
    );
}

#[test]
fn pc_platform_resyncs_uhci_pci_command_before_polling_intx_level() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe the UHCI interrupt through the
    // legacy PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable IOC interrupts in the UHCI controller and force a USBINT status bit so the controller
    // asserts its IRQ line.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));
    let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
    uhci.borrow_mut()
        .controller_mut()
        .set_usbsts_bits(USBSTS_USBINT);

    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;

    // Disable INTx in PCI command register (bit 10) while leaving I/O decode enabled. Then force a
    // platform tick (with 0 elapsed time) so the UHCI model's internal PCI config copy observes the
    // INTx disable bit.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 10),
    );
    pc.tick(0);
    assert!(
        !uhci.borrow().irq_level(),
        "UHCI device model should suppress its IRQ when COMMAND.INTX_DISABLE is set"
    );

    // Re-enable INTx in the guest-facing PCI config space without ticking. This leaves the UHCI
    // device model with a stale copy of the PCI command register that still has INTx disabled.
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command & !(1 << 10),
    );
    assert!(
        !uhci.borrow().irq_level(),
        "UHCI model should still see stale INTx disable bit until the platform resyncs PCI config"
    );

    // Polling INTx lines must resync PCI command state before querying the device model so the
    // cleared INTx disable bit takes effect immediately.
    pc.poll_pci_intx_lines();
    assert!(
        uhci.borrow().irq_level(),
        "poll_pci_intx_lines should resync PCI command and expose the pending UHCI interrupt"
    );
    assert_eq!(
        pc.interrupts
            .borrow()
            .pic()
            .get_pending_vector()
            .and_then(|v| pc.interrupts.borrow().pic().vector_to_irq(v)),
        Some(irq)
    );
}

#[test]
fn pc_platform_routes_uhci_intx_via_ioapic_in_apic_mode() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bar4_base = read_uhci_bar4_base(&mut pc);

    // Switch the platform into APIC mode via IMCR.
    pc.io.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
    pc.io.write_u8(IMCR_DATA_PORT, 0x01);
    assert_eq!(pc.interrupts.borrow().mode(), PlatformInterruptMode::Apic);

    // Route the UHCI INTx line to vector 0x60, level-triggered + active-low.
    let vector = 0x60u32;
    let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered, unmasked
    let bdf = USB_UHCI_PIIX3.bdf;
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    program_ioapic_entry(&mut pc, gsi, low, 0);

    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));
    {
        let uhci = pc.uhci.as_ref().expect("UHCI should be enabled").clone();
        uhci.borrow_mut()
            .controller_mut()
            .set_usbsts_bits(USBSTS_USBINT);
    }

    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().get_pending(), Some(vector as u8));

    // Acknowledge the interrupt (vector in service).
    pc.interrupts.borrow_mut().acknowledge(vector as u8);

    // Clear the controller IRQ and propagate the deassertion before sending EOI, so we don't
    // immediately retrigger due to the level-triggered line remaining high.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();

    pc.interrupts.borrow_mut().eoi(vector as u8);
    assert_eq!(pc.interrupts.borrow().get_pending(), None);
}

#[test]
fn pc_platform_uhci_interrupt_in_reads_hid_keyboard_reports_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // SET_ADDRESS(5).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    assert_eq!(pc.memory.read_u32(TD0 as u64 + 4) & TD_STATUS_ACTIVE, 0);
    assert_eq!(pc.memory.read_u32(TD1 as u64 + 4) & TD_STATUS_ACTIVE, 0);

    // SET_CONFIGURATION(1).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    keyboard.key_event(0x04, true); // 'a'

    // Poll interrupt endpoint 1 at address 5.
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    let mut report = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut report);
    assert_eq!(report, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);

    // Poll again without new input: should NAK and remain active.
    pc.memory.write_u32(TD0 as u64 + 4, td_status(true));
    run_one_frame(&mut pc, TD0);
    let st = pc.memory.read_u32(TD0 as u64 + 4);
    assert_ne!(st & TD_STATUS_ACTIVE, 0);
    assert_ne!(st & TD_STATUS_NAK, 0);
}

#[test]
fn pc_platform_uhci_control_transfers_can_set_and_get_keyboard_led_report_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate keyboard at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // HID SET_REPORT (Output, reportId=0) with 1 byte of LED state.
    control_out(
        &mut pc,
        5,
        [0x21, 0x09, 0x00, 0x02, 0x00, 0x00, 0x01, 0x00], // SET_REPORT(Output)
        &[0x01],
    );

    // HID GET_REPORT (Output) should return the LED byte we just set.
    let got = control_in(
        &mut pc,
        5,
        [0xA1, 0x01, 0x00, 0x02, 0x00, 0x00, 0x01, 0x00], // GET_REPORT(Output)
        BUF_CTRL,
    );
    assert_eq!(got, vec![0x01]);
}

#[test]
fn pc_platform_uhci_ioc_completion_asserts_intx_via_pic() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_IOC));
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // SET_ADDRESS(5).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    // SET_CONFIGURATION(1).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    // Generate an input report and schedule an IOC interrupt-IN TD to fetch it.
    keyboard.key_event(0x04, true); // 'a'
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true) | TD_CTRL_IOC,
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_USBINT,
        0,
        "UHCI schedule completion should set USBSTS.USBINT when TD has IOC set"
    );

    // Propagate the asserted INTx line into the PIC and observe the pending vector.
    pc.poll_pci_intx_lines();

    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after IOC TD completion");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI the interrupt so we can observe deassertion cleanly.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the status bit (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_short_packet_sets_usbint_and_asserts_intx_when_spd_enabled() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    // Enable short-packet interrupts (but not IOC), so USBINT gating is tested on the short-packet
    // cause.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_SHORT_PACKET));
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate keyboard at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Generate an 8-byte HID report and schedule an interrupt-IN TD that:
    // - requests a much larger max_len than the device will return (short packet), and
    // - sets SPD so the controller reports short packets via USBINT.
    keyboard.key_event(0x04, true); // 'a'
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true) | TD_CTRL_SPD,
        td_token(PID_IN, 5, 1, 0, 64),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_USBINT,
        0,
        "UHCI schedule completion should set USBSTS.USBINT on short packet when SPD is set"
    );

    // Propagate the asserted INTx line into the PIC and observe the pending vector.
    pc.poll_pci_intx_lines();

    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after short packet detection");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI the interrupt so we can observe deassertion cleanly.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear the status bit (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_usb_err_int_asserts_intx_on_crc_timeout() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_TIMEOUT_CRC));
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Issue an IN TD to a non-existent device address so the schedule reports a CRC/timeout error.
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 42, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    let td_st = pc.memory.read_u32(TD0 as u64 + 4);
    assert_eq!(td_st & TD_STATUS_ACTIVE, 0);
    assert_ne!(
        td_st & TD_STATUS_CRC_TIMEOUT,
        0,
        "expected missing device to set CRC/timeout error on TD"
    );

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_USBERRINT,
        0,
        "expected UHCI schedule error to set USBSTS.USBERRINT"
    );

    // Propagate INTx and observe the pending IRQ.
    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after USBERRINT");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear USBERRINT (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_USBERRINT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_force_global_resume_sets_resume_detect_and_asserts_intx() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    // Enable RESUME interrupts.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_RESUME));

    // Pulse USBCMD.FGR (Force Global Resume): should latch USBSTS.RESUMEDETECT.
    let usbcmd = pc.io.read(bar4_base + REG_USBCMD, 2) as u16;
    pc.io
        .write(bar4_base + REG_USBCMD, 2, u32::from(usbcmd | USBCMD_FGR));

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0,
        "expected USBCMD.FGR to latch USBSTS.RESUMEDETECT"
    );

    // Propagate INTx and observe the pending IRQ.
    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after RESUMEDETECT");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI the interrupt.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    // Clear RESUMEDETECT (W1C) and ensure the line deasserts.
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_RESUMEDETECT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_interrupt_in_reads_hid_mouse_reports_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let mouse = UsbHidMouseHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(mouse.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // SET_ADDRESS(5).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    // SET_CONFIGURATION(1).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    mouse.movement(5, -3);

    // Poll interrupt endpoint 1 at address 5 (4-byte mouse report).
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 4),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    let mut report = [0u8; 4];
    pc.memory.read_physical(BUF_INT as u64, &mut report);
    assert_eq!(report, [0x00, 5, 0xFD, 0x00]);

    // Poll again without new input: should NAK and remain active.
    pc.memory.write_u32(TD0 as u64 + 4, td_status(true));
    run_one_frame(&mut pc, TD0);
    let st = pc.memory.read_u32(TD0 as u64 + 4);
    assert_ne!(st & TD_STATUS_ACTIVE, 0);
    assert_ne!(st & TD_STATUS_NAK, 0);
}

#[test]
fn pc_platform_uhci_interrupt_in_reads_hid_gamepad_reports_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let gamepad = UsbHidGamepadHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(gamepad.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // SET_ADDRESS(5).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    // SET_CONFIGURATION(1).
    pc.memory.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut pc,
        TD0,
        TD1,
        td_status(true),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut pc,
        TD1,
        1,
        td_status(true),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut pc, TD0);

    gamepad.set_buttons(0x0001);

    // Poll interrupt endpoint 1 at address 5 (8-byte gamepad report).
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    let mut report = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut report);
    assert_eq!(report, [0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // Poll again without new input: should NAK and remain active.
    pc.memory.write_u32(TD0 as u64 + 4, td_status(true));
    run_one_frame(&mut pc, TD0);
    let st = pc.memory.read_u32(TD0 as u64 + 4);
    assert_ne!(st & TD_STATUS_ACTIVE, 0);
    assert_ne!(st & TD_STATUS_NAK, 0);
}

#[test]
fn pc_platform_uhci_external_hub_delivers_keyboard_report_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let keyboard = UsbHidKeyboardHandle::new();

    // Root port0: external USB hub.
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(UsbHubDevice::new()));

    // Attach a keyboard behind the hub (port 1).
    pc.uhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .unwrap();

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate hub itself at address 1.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(1)
    );
    control_no_data(
        &mut pc,
        1,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Power + reset downstream hub port 1.
    control_no_data(
        &mut pc,
        1,
        [0x23, 0x03, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_POWER) port=1
    );
    control_no_data(
        &mut pc,
        1,
        [0x23, 0x03, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_RESET) port=1
    );
    pc.tick(50_000_000);

    // Enumerate the downstream keyboard at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    keyboard.key_event(0x04, true); // 'a'

    // Poll interrupt endpoint 1 at address 5.
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);

    let mut report = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut report);
    assert_eq!(report, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);
}

#[test]
fn pc_platform_uhci_external_hub_delivers_multiple_hid_reports_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let keyboard = UsbHidKeyboardHandle::new();
    let mouse = UsbHidMouseHandle::new();
    let gamepad = UsbHidGamepadHandle::new();

    // Root port0: external USB hub.
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(UsbHubDevice::new()));

    // Attach 3 HID devices behind the hub (ports 1..3), matching the web runtime topology.
    {
        let uhci = pc.uhci.as_ref().unwrap().clone();
        let mut dev = uhci.borrow_mut();
        let hub = dev.controller_mut().hub_mut();
        hub.attach_at_path(&[0, 1], Box::new(keyboard.clone()))
            .unwrap();
        hub.attach_at_path(&[0, 2], Box::new(mouse.clone()))
            .unwrap();
        hub.attach_at_path(&[0, 3], Box::new(gamepad.clone()))
            .unwrap();
    }

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate hub itself at address 1.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(1)
    );
    control_no_data(
        &mut pc,
        1,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Power+reset each downstream port and enumerate each device sequentially. This avoids having
    // multiple address-0 devices reachable at once.
    for (port, addr) in [(1u8, 5u8), (2u8, 6u8), (3u8, 7u8)] {
        control_no_data(
            &mut pc,
            1,
            [0x23, 0x03, 0x08, 0x00, port, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_POWER)
        );
        control_no_data(
            &mut pc,
            1,
            [0x23, 0x03, 0x04, 0x00, port, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_RESET)
        );
        pc.tick(50_000_000);

        control_no_data(
            &mut pc,
            0,
            [0x00, 0x05, addr, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(addr)
        );
        control_no_data(
            &mut pc,
            addr,
            [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
        );
    }

    keyboard.key_event(0x04, true); // 'a'
    mouse.movement(5, -3);
    gamepad.set_buttons(0x0001);

    // Keyboard interrupt IN (addr 5, EP1, 8 bytes).
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut kbd = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut kbd);
    assert_eq!(kbd, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);

    // Mouse interrupt IN (addr 6, EP1, 4 bytes).
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 6, 1, 0, 4),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut mouse_report = [0u8; 4];
    pc.memory.read_physical(BUF_INT as u64, &mut mouse_report);
    assert_eq!(mouse_report, [0x00, 5, 0xFD, 0x00]);

    // Gamepad interrupt IN (addr 7, EP1, 8 bytes).
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 7, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut pad = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut pad);
    assert_eq!(pad, [0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn pc_platform_uhci_remote_wakeup_sets_resume_detect_and_triggers_intx() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate + configure the keyboard at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Enable remote wakeup on the device (standard SET_FEATURE DEVICE_REMOTE_WAKEUP).
    control_no_data(&mut pc, 5, [0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // Enable resume-detect interrupts.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_RESUME));

    // Suspend the root hub port, then inject a key event. The HID device should request remote
    // wakeup, which the root hub exposes via the PORTSC "resume detect" bit and the controller
    // latches into USBSTS.RESUMEDETECT (triggering an interrupt when USBINTR.RESUME is enabled).
    write_portsc(&mut pc, bar4_base, REG_PORTSC1, PORTSC_PED | PORTSC_SUSP);
    keyboard.key_event(0x04, true);
    pc.tick(1_000_000);

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0,
        "remote wakeup should latch USBSTS.RESUMEDETECT"
    );
    assert_ne!(
        read_portsc(&mut pc, bar4_base, REG_PORTSC1) & (1 << 6),
        0,
        "remote wakeup should set PORTSC.RD"
    );

    pc.poll_pci_intx_lines();

    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after RESUMEDETECT");
    let pending_irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(vector)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(pending_irq, irq);

    // Consume + EOI, then clear the status bit and observe deassertion.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }
    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_RESUMEDETECT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_external_hub_remote_wakeup_triggers_resume_detect_and_intx() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);
    let gsi = pc.pci_intx.gsi_for_intx(bdf, PciInterruptPin::IntA);
    let irq = u8::try_from(gsi).expect("UHCI INTx should route to a PIC IRQ in legacy mode");

    // Unmask IRQ2 (cascade) + the routed IRQ so we can observe UHCI interrupts through the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(irq, false);
    }

    let keyboard = UsbHidKeyboardHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(UsbHubDevice::new()));

    pc.uhci
        .as_ref()
        .unwrap()
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .unwrap();

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate the hub at address 1.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(1)
    );
    control_no_data(
        &mut pc,
        1,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Power + reset downstream hub port 1 so the keyboard becomes enabled/powered.
    control_no_data(
        &mut pc,
        1,
        [0x23, 0x03, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_POWER)
    );
    control_no_data(
        &mut pc,
        1,
        [0x23, 0x03, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00], // SET_FEATURE(PORT_RESET)
    );
    pc.tick(50_000_000);

    // Enumerate + configure the downstream keyboard at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Enable remote wakeup on the device (standard SET_FEATURE DEVICE_REMOTE_WAKEUP).
    control_no_data(&mut pc, 5, [0x00, 0x03, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]);

    // Enable resume-detect interrupts.
    pc.io
        .write(bar4_base + REG_USBINTR, 2, u32::from(USBINTR_RESUME));

    // Suspend the root hub port (suspends the hub + downstream devices), then inject a key event.
    write_portsc(&mut pc, bar4_base, REG_PORTSC1, PORTSC_PED | PORTSC_SUSP);
    keyboard.key_event(0x04, true);
    pc.tick(1_000_000);

    assert_ne!(
        pc.io.read(bar4_base + REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0,
        "remote wakeup through hub should latch USBSTS.RESUMEDETECT"
    );

    pc.poll_pci_intx_lines();
    let vector = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("UHCI IRQ should be pending after RESUMEDETECT");

    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(vector);
        interrupts.pic_mut().eoi(vector);
    }

    pc.io
        .write(bar4_base + REG_USBSTS, 2, u32::from(USBSTS_RESUMEDETECT));
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_uhci_interrupt_in_reads_composite_hid_reports_via_dma() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);
    let bdf = USB_UHCI_PIIX3.bdf;
    let bar4_base = read_uhci_bar4_base(&mut pc);

    let composite = UsbCompositeHidInputHandle::new();
    pc.uhci
        .as_ref()
        .expect("UHCI should be enabled")
        .borrow_mut()
        .controller_mut()
        .hub_mut()
        .attach(0, Box::new(composite.clone()));

    // Enable Bus Mastering so UHCI can DMA the schedule/TD state.
    let command = read_cfg_u32(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04) as u16;
    write_cfg_u16(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x04,
        command | (1 << 2),
    );
    pc.tick(0);

    init_frame_list(&mut pc);
    reset_port(&mut pc, bar4_base, REG_PORTSC1);

    pc.io.write(bar4_base + REG_FLBASEADD, 4, FRAME_LIST_BASE);
    pc.io.write(bar4_base + REG_FRNUM, 2, 0);
    pc.io.write(
        bar4_base + REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_MAXP),
    );

    // Enumerate + configure the device at address 5.
    control_no_data(
        &mut pc,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_ADDRESS(5)
    );
    control_no_data(
        &mut pc,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00], // SET_CONFIGURATION(1)
    );

    // Keyboard report (endpoint 1 / 0x81).
    composite.key_event(0x04, true); // 'a'
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut kbd = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut kbd);
    assert_eq!(kbd, [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]);

    // Mouse report (endpoint 2 / 0x82).
    composite.mouse_movement(5, -3);
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 2, 0, 4),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut mouse = [0u8; 4];
    pc.memory.read_physical(BUF_INT as u64, &mut mouse);
    assert_eq!(mouse, [0x00, 5, 0xFD, 0x00]);

    // Gamepad report (endpoint 3 / 0x83).
    composite.gamepad_set_buttons(0x0001);
    write_td(
        &mut pc,
        TD0,
        1,
        td_status(true),
        td_token(PID_IN, 5, 3, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut pc, TD0);
    let mut pad = [0u8; 8];
    pc.memory.read_physical(BUF_INT as u64, &mut pad);
    assert_eq!(pad, [0x01, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00]);
}
