use std::ops::Range;

use emulator::io::usb::hid::composite::UsbCompositeHidInputHandle;
use emulator::io::usb::hid::gamepad::UsbHidGamepadHandle;
use emulator::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use emulator::io::usb::uhci::regs::{REG_USBCMD, USBCMD_MAXP, USBCMD_RS};
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};
use emulator::io::PortIO;
use memory::MemoryBus;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;
const TD1: u32 = 0x3020;
const TD2: u32 = 0x3040;

const BUF_SETUP: u32 = 0x4000;
const BUF_DATA: u32 = 0x5000;
const BUF_INT: u32 = 0x6000;

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;

// UHCI root hub PORTSC bits (Intel UHCI spec / Linux uhci-hcd).
const PORTSC_CCS: u16 = 0x0001;
const PORTSC_CSC: u16 = 0x0002;
const PORTSC_PED: u16 = 0x0004;
const PORTSC_PEDC: u16 = 0x0008;
const PORTSC_LSDA: u16 = 0x0100;
const PORTSC_PR: u16 = 0x0200;

struct TestMemBus {
    mem: Vec<u8>,
}

impl TestMemBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn slice(&self, range: Range<usize>) -> &[u8] {
        &self.mem[range]
    }
}

impl MemoryBus for TestMemBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        buf.copy_from_slice(&self.mem[start..start + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        self.mem[start..start + buf.len()].copy_from_slice(buf);
    }
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

fn td_status(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7ffu32;
    if active {
        v |= TD_STATUS_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

fn write_td(mem: &mut TestMemBus, addr: u32, link: u32, status: u32, token: u32, buffer: u32) {
    mem.write_u32(addr as u64, link);
    mem.write_u32(addr.wrapping_add(4) as u64, status);
    mem.write_u32(addr.wrapping_add(8) as u64, token);
    mem.write_u32(addr.wrapping_add(12) as u64, buffer);
}

fn write_qh(mem: &mut TestMemBus, addr: u32, elem: u32) {
    mem.write_u32(addr as u64, 1); // horiz terminate
    mem.write_u32(addr.wrapping_add(4) as u64, elem);
}

fn init_frame_list(mem: &mut TestMemBus, qh_addr: u32) {
    for i in 0..1024u32 {
        mem.write_u32((FRAME_LIST_BASE + i * 4) as u64, qh_addr | 0x2);
    }
}

fn run_one_frame(uhci: &mut UhciPciDevice, mem: &mut TestMemBus, first_td: u32) {
    write_qh(mem, QH_ADDR, first_td);
    uhci.tick_1ms(mem);
}

fn read_portsc(uhci: &UhciPciDevice, portsc: u16) -> u16 {
    uhci.port_read(portsc, 2) as u16
}

fn write_portsc(uhci: &mut UhciPciDevice, portsc: u16, value: u16) {
    uhci.port_write(portsc, 2, value as u32);
}

fn write_portsc_w1c(uhci: &mut UhciPciDevice, portsc: u16, w1c: u16) {
    // Preserve the port enable bit when clearing change bits, matching the usual
    // read-modify-write pattern of UHCI drivers.
    let cur = read_portsc(uhci, portsc);
    let value = (cur & PORTSC_PED) | w1c;
    write_portsc(uhci, portsc, value);
}

fn reset_port(uhci: &mut UhciPciDevice, mem: &mut TestMemBus, portsc: u16) {
    // Clear connection status change if present.
    if read_portsc(uhci, portsc) & PORTSC_CSC != 0 {
        write_portsc_w1c(uhci, portsc, PORTSC_CSC);
    }

    // Trigger port reset and wait the UHCI-mandated ~50ms.
    write_portsc(uhci, portsc, PORTSC_PR);
    for _ in 0..50 {
        uhci.tick_1ms(mem);
    }
}

#[test]
fn uhci_root_hub_portsc_reset_enables_port() {
    let mut mem = TestMemBus::new(0x1000);
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    let st = read_portsc(&uhci, 0x10);
    assert_eq!(st & (PORTSC_CCS | PORTSC_CSC), PORTSC_CCS | PORTSC_CSC);

    write_portsc(&mut uhci, 0x10, PORTSC_PR);
    let st = read_portsc(&uhci, 0x10);
    assert_ne!(st & PORTSC_PR, 0);
    assert_eq!(st & PORTSC_LSDA, 0);

    for _ in 0..50 {
        uhci.tick_1ms(&mut mem);
    }

    let st = read_portsc(&uhci, 0x10);
    assert_eq!(st & PORTSC_PR, 0);
    assert_ne!(st & PORTSC_PED, 0);
    assert_ne!(st & PORTSC_PEDC, 0);
}

#[test]
fn uhci_usbcmd_default_enables_max_packet_and_roundtrips() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    let usbcmd = uhci.port_read(REG_USBCMD, 2) as u16;
    assert!(usbcmd & USBCMD_MAXP != 0);

    uhci.port_write(REG_USBCMD, 2, (USBCMD_MAXP | USBCMD_RS) as u32);
    let usbcmd = uhci.port_read(REG_USBCMD, 2) as u16;
    assert_eq!(usbcmd & (USBCMD_MAXP | USBCMD_RS), USBCMD_MAXP | USBCMD_RS);
}

#[test]
fn uhci_control_get_descriptor_device() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    mem.write_physical(
        BUF_SETUP as u64,
        &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
    );

    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        TD2,
        td_status(true, false),
        td_token(PID_IN, 0, 0, 1, 18),
        BUF_DATA,
    );
    write_td(
        &mut mem,
        TD2,
        1,
        td_status(true, true),
        td_token(PID_OUT, 0, 0, 1, 0),
        0,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let expected = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x00, 0x01,
    ];
    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + 18),
        expected
    );

    let st0 = mem.read_u32(TD0 as u64 + 4);
    let st1 = mem.read_u32(TD1 as u64 + 4);
    let st2 = mem.read_u32(TD2 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st2 & TD_STATUS_ACTIVE, 0);
}

#[test]
fn uhci_interrupt_in_polling_reads_hid_reports() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // SET_ADDRESS(5).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    // SET_CONFIGURATION(1).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    keyboard.key_event(0x04, true); // 'a'

    // Poll interrupt endpoint 1 at address 5.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 8),
        [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );

    // Poll again without new input: should NAK and remain active.
    mem.write_u32(TD0 as u64 + 4, td_status(true, false));
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert!(st & TD_STATUS_ACTIVE != 0);
    assert!(st & (1 << 19) != 0); // NAK
}

#[test]
fn uhci_interrupt_in_polling_reads_gamepad_reports() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let gamepad = UsbHidGamepadHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(gamepad.clone()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // SET_ADDRESS(5).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    // SET_CONFIGURATION(1).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    gamepad.set_axes(10, -10, 5, -5);
    gamepad.button_event(1, true);

    // Poll interrupt endpoint 1 at address 5.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 8),
        [0x00, 0x00, 0x08, 10u8, 246u8, 5u8, 251u8, 0x00]
    );

    // Poll again - should receive the button change.
    mem.write_u32(TD0 as u64 + 4, td_status(true, false));
    run_one_frame(&mut uhci, &mut mem, TD0);
    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 8),
        [0x01, 0x00, 0x08, 10u8, 246u8, 5u8, 251u8, 0x00]
    );

    // Poll again without new input: should NAK and remain active.
    mem.write_u32(TD0 as u64 + 4, td_status(true, false));
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert!(st & TD_STATUS_ACTIVE != 0);
    assert!(st & (1 << 19) != 0); // NAK
}

#[test]
fn uhci_composite_hid_device_exposes_keyboard_mouse_gamepad() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let composite = UsbCompositeHidInputHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(composite.clone()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // SET_ADDRESS(5).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    // SET_CONFIGURATION(1).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 5, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 0, 1, 0),
        0,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    // Fetch report descriptors for each interface and verify the first few bytes.
    // bmRequestType = 0x81 (DeviceToHost, Standard, Interface)
    // wValue = 0x2200 (Report descriptor)
    for (iface, expected_usage) in [(0u16, 0x06u8), (1, 0x02u8), (2, 0x05u8)] {
        mem.write_physical(
            BUF_SETUP as u64,
            &[0x81, 0x06, 0x00, 0x22, iface as u8, 0x00, 0x40, 0x00],
        );
        write_td(
            &mut mem,
            TD0,
            TD1,
            td_status(true, false),
            td_token(PID_SETUP, 5, 0, 0, 8),
            BUF_SETUP,
        );
        write_td(
            &mut mem,
            TD1,
            TD2,
            td_status(true, false),
            td_token(PID_IN, 5, 0, 1, 64),
            BUF_DATA,
        );
        write_td(
            &mut mem,
            TD2,
            1,
            td_status(true, true),
            td_token(PID_OUT, 5, 0, 1, 0),
            0,
        );
        run_one_frame(&mut uhci, &mut mem, TD0);

        let prefix = mem.slice(BUF_DATA as usize..BUF_DATA as usize + 4);
        assert_eq!(prefix[0], 0x05); // Usage Page
        assert_eq!(prefix[1], 0x01); // Generic Desktop
        assert_eq!(prefix[2], 0x09); // Usage
        assert_eq!(prefix[3], expected_usage);
    }

    composite.key_event(0x04, true); // 'a'
    composite.mouse_movement(10, -5);
    composite.gamepad_button_event(0x0001, true);

    // Poll keyboard interrupt endpoint 1.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 8),
        [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );

    // Poll mouse interrupt endpoint 2.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 2, 0, 4),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 4),
        [0x00, 10u8, (-5i8) as u8, 0x00]
    );

    // Poll gamepad interrupt endpoint 3.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 3, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 8),
        [0x01, 0x00, 0, 0, 0, 0, 0, 0]
    );
}
