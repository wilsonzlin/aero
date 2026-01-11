use std::ops::Range;

use emulator::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use emulator::io::usb::hub::UsbHubDevice;
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
const BUF_HUB_INT: u32 = 0x6000;
const BUF_KBD_INT: u32 = 0x6100;
const BUF_KBD2_INT: u32 = 0x6120;
const BUF_KBD3_INT: u32 = 0x6140;

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_STATUS_STALLED: u32 = 1 << 22;
const TD_STATUS_NAK: u32 = 1 << 19;
const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;
const TD_CTRL_IOC: u32 = 1 << 24;

// UHCI root hub PORTSC bits (Intel UHCI spec / Linux uhci-hcd).
const PORTSC_CSC: u16 = 0x0002;
const PORTSC_PED: u16 = 0x0004;
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
    // Preserve the port enable bit when clearing change bits, matching typical UHCI drivers.
    let cur = read_portsc(uhci, portsc);
    let value = (cur & PORTSC_PED) | w1c;
    write_portsc(uhci, portsc, value);
}

fn reset_root_port(uhci: &mut UhciPciDevice, mem: &mut TestMemBus, portsc: u16) {
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

fn control_in(
    uhci: &mut UhciPciDevice,
    mem: &mut TestMemBus,
    addr: u8,
    setup: [u8; 8],
    data_buf: u32,
    data_len: usize,
) {
    mem.write_physical(BUF_SETUP as u64, &setup);

    write_td(
        mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, addr, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        mem,
        TD1,
        TD2,
        td_status(true, false),
        td_token(PID_IN, addr, 0, 1, data_len),
        data_buf,
    );
    write_td(
        mem,
        TD2,
        1,
        td_status(true, true),
        td_token(PID_OUT, addr, 0, 1, 0),
        0,
    );

    run_one_frame(uhci, mem, TD0);
}

fn control_no_data(uhci: &mut UhciPciDevice, mem: &mut TestMemBus, addr: u8, setup: [u8; 8]) {
    mem.write_physical(BUF_SETUP as u64, &setup);

    write_td(
        mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, addr, 0, 0, 8),
        BUF_SETUP,
    );
    write_td(
        mem,
        TD1,
        1,
        td_status(true, true),
        td_token(PID_IN, addr, 0, 1, 0),
        0,
    );

    run_one_frame(uhci, mem, TD0);
}

fn assert_tds_ok(mem: &mut TestMemBus, tds: &[u32]) {
    for &td in tds {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
}

fn power_reset_and_clear_hub_port(
    uhci: &mut UhciPciDevice,
    mem: &mut TestMemBus,
    hub_addr: u8,
    port: u8,
) {
    // SET_FEATURE(PORT_POWER)
    control_no_data(
        uhci,
        mem,
        hub_addr,
        [0x23, 0x03, 0x08, 0x00, port, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(mem, &[TD0, TD1]);

    // SET_FEATURE(PORT_RESET)
    control_no_data(
        uhci,
        mem,
        hub_addr,
        [0x23, 0x03, 0x04, 0x00, port, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(mem, &[TD0, TD1]);

    // Advance time until reset completes.
    for _ in 0..50 {
        uhci.tick_1ms(mem);
    }

    // Clear change bits so subsequent interrupt polling is deterministic.
    // CLEAR_FEATURE(C_PORT_RESET)
    control_no_data(
        uhci,
        mem,
        hub_addr,
        [0x23, 0x01, 0x14, 0x00, port, 0x00, 0x00, 0x00],
    );
    // CLEAR_FEATURE(C_PORT_CONNECTION)
    control_no_data(
        uhci,
        mem,
        hub_addr,
        [0x23, 0x01, 0x10, 0x00, port, 0x00, 0x00, 0x00],
    );
    // CLEAR_FEATURE(C_PORT_ENABLE)
    control_no_data(
        uhci,
        mem,
        hub_addr,
        [0x23, 0x01, 0x11, 0x00, port, 0x00, 0x00, 0x00],
    );
}

fn enumerate_keyboard(uhci: &mut UhciPciDevice, mem: &mut TestMemBus, address: u8) {
    let expected_keyboard_device_descriptor = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01, 0x02,
        0x00, 0x01,
    ];

    // GET_DESCRIPTOR(Device) at address 0 (default-address state).
    control_in(
        uhci,
        mem,
        0,
        [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
        BUF_DATA,
        18,
    );
    assert_tds_ok(mem, &[TD0, TD1, TD2]);
    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + 18),
        expected_keyboard_device_descriptor
    );

    // SET_ADDRESS(address)
    control_no_data(
        uhci,
        mem,
        0,
        [0x00, 0x05, address, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(mem, &[TD0, TD1]);

    // GET_DESCRIPTOR(Configuration) at new address.
    control_in(
        uhci,
        mem,
        address,
        [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 34, 0x00],
        BUF_DATA,
        34,
    );
    assert_tds_ok(mem, &[TD0, TD1, TD2]);

    // SET_CONFIGURATION(1)
    control_no_data(
        uhci,
        mem,
        address,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(mem, &[TD0, TD1]);
}

#[test]
fn uhci_external_hub_enumerates_downstream_hid() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Root port 0 has an external USB hub, with a keyboard on downstream port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().attach(0, Box::new(hub));

    // Enable the root port via PORTSC reset sequence (typical UHCI enumeration).
    reset_root_port(&mut uhci, &mut mem, 0x10);

    // Start the controller + frame list.
    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // --- Enumerate the hub itself at address 0 -> address 1. ---

    // GET_DESCRIPTOR(Device)
    control_in(
        &mut uhci,
        &mut mem,
        0,
        [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
        BUF_DATA,
        18,
    );
    for td in [TD0, TD1, TD2] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
    assert_eq!(mem.slice(BUF_DATA as usize..BUF_DATA as usize + 2), [0x12, 0x01]); // device desc
    assert_eq!(mem.mem[BUF_DATA as usize + 4], 0x09); // bDeviceClass = HUB

    // SET_ADDRESS(1)
    control_no_data(
        &mut uhci,
        &mut mem,
        0,
        [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    for td in [TD0, TD1] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }

    // GET_DESCRIPTOR(Configuration)
    control_in(
        &mut uhci,
        &mut mem,
        1,
        [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 25, 0x00],
        BUF_DATA,
        25,
    );
    for td in [TD0, TD1, TD2] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
    assert_eq!(mem.mem[BUF_DATA as usize + 1], 0x02); // config desc

    // SET_CONFIGURATION(1)
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    for td in [TD0, TD1] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }

    // GET_DESCRIPTOR(Hub, type=0x29) via class request.
    control_in(
        &mut uhci,
        &mut mem,
        1,
        [0xa0, 0x06, 0x00, 0x29, 0x00, 0x00, 9, 0x00],
        BUF_DATA,
        9,
    );
    for td in [TD0, TD1, TD2] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
    assert_eq!(mem.mem[BUF_DATA as usize + 1], 0x29); // hub descriptor type

    // --- Prove the downstream device is unreachable before the hub port is enabled. ---
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
    );
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & TD_STATUS_ACTIVE, 0);
    assert_ne!(st & TD_STATUS_CRC_TIMEOUT, 0);

    // --- Power + reset hub downstream port 1. ---
    // SET_FEATURE(PORT_POWER), port=1.
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x23, 0x03, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00],
    );
    for td in [TD0, TD1] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }

    // SET_FEATURE(PORT_RESET), port=1.
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x23, 0x03, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00],
    );
    for td in [TD0, TD1] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }

    // Advance time until reset completes.
    for _ in 0..50 {
        uhci.tick_1ms(&mut mem);
    }

    // Poll the hub interrupt endpoint (ep1 IN) for port-change bitmap.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 1, 1, 0, 1),
        BUF_HUB_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED | TD_STATUS_NAK), 0);
    assert_ne!(mem.mem[BUF_HUB_INT as usize] & 0x02, 0); // bit1 = port1 change

    // GET_STATUS(port1) should report enabled + C_RESET (and usually C_CONNECTION).
    control_in(
        &mut uhci,
        &mut mem,
        1,
        [0xa3, 0x00, 0x00, 0x00, 0x01, 0x00, 0x04, 0x00],
        BUF_DATA,
        4,
    );
    for td in [TD0, TD1, TD2] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
    let port_status = u16::from_le_bytes([mem.mem[BUF_DATA as usize], mem.mem[BUF_DATA as usize + 1]]);
    let port_change =
        u16::from_le_bytes([mem.mem[BUF_DATA as usize + 2], mem.mem[BUF_DATA as usize + 3]]);
    assert_ne!(port_status & (1 << 1), 0); // PORT_ENABLE
    assert_ne!(port_change & (1 << 4), 0); // C_PORT_RESET

    // Clear change bits so the hub would NAK on further polls.
    // CLEAR_FEATURE(C_PORT_RESET), port=1.
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x23, 0x01, 0x14, 0x00, 0x01, 0x00, 0x00, 0x00],
    );
    // CLEAR_FEATURE(C_PORT_CONNECTION), port=1.
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x23, 0x01, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00],
    );
    // CLEAR_FEATURE(C_PORT_ENABLE), port=1.
    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x23, 0x01, 0x11, 0x00, 0x01, 0x00, 0x00, 0x00],
    );

    // --- Enumerate the downstream keyboard behind the hub. ---
    control_in(
        &mut uhci,
        &mut mem,
        0,
        [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
        BUF_DATA,
        18,
    );
    for td in [TD0, TD1, TD2] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED), 0);
    }
    let expected_keyboard_device_descriptor = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x00, 0x01,
    ];
    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + 18),
        expected_keyboard_device_descriptor
    );

    // SET_ADDRESS(5).
    control_no_data(
        &mut uhci,
        &mut mem,
        0,
        [0x00, 0x05, 0x05, 0x00, 0x00, 0x00, 0x00, 0x00],
    );

    // GET_DESCRIPTOR(Configuration) at address 5.
    control_in(
        &mut uhci,
        &mut mem,
        5,
        [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 34, 0x00],
        BUF_DATA,
        34,
    );

    // SET_CONFIGURATION(1) at address 5.
    control_no_data(
        &mut uhci,
        &mut mem,
        5,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );

    // --- Functional proof: interrupt-IN report from keyboard behind hub. ---
    keyboard.key_event(0x04, true); // 'a'
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_KBD_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED | TD_STATUS_NAK), 0);
    assert_eq!(
        mem.slice(BUF_KBD_INT as usize..BUF_KBD_INT as usize + 8),
        [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );
}

#[test]
fn uhci_external_hub_enumerates_multiple_downstream_hid_devices() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Root port 0 has an external hub with 3 keyboards on downstream ports 1..3.
    let keyboard1 = UsbHidKeyboardHandle::new();
    let keyboard2 = UsbHidKeyboardHandle::new();
    let keyboard3 = UsbHidKeyboardHandle::new();
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(keyboard1.clone()));
    hub.attach(2, Box::new(keyboard2.clone()));
    hub.attach(3, Box::new(keyboard3.clone()));
    uhci.controller.hub_mut().attach(0, Box::new(hub));

    // Enable the root port and start the controller.
    reset_root_port(&mut uhci, &mut mem, 0x10);
    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // Enumerate the hub itself at address 0 -> address 1, then configure it.
    control_in(
        &mut uhci,
        &mut mem,
        0,
        [0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00],
        BUF_DATA,
        18,
    );
    assert_tds_ok(&mut mem, &[TD0, TD1, TD2]);

    control_no_data(
        &mut uhci,
        &mut mem,
        0,
        [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(&mut mem, &[TD0, TD1]);

    control_in(
        &mut uhci,
        &mut mem,
        1,
        [0x80, 0x06, 0x00, 0x02, 0x00, 0x00, 25, 0x00],
        BUF_DATA,
        25,
    );
    assert_tds_ok(&mut mem, &[TD0, TD1, TD2]);

    control_no_data(
        &mut uhci,
        &mut mem,
        1,
        [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );
    assert_tds_ok(&mut mem, &[TD0, TD1]);

    // Power + reset each port, then enumerate each keyboard.
    power_reset_and_clear_hub_port(&mut uhci, &mut mem, 1, 1);
    enumerate_keyboard(&mut uhci, &mut mem, 5);

    power_reset_and_clear_hub_port(&mut uhci, &mut mem, 1, 2);
    enumerate_keyboard(&mut uhci, &mut mem, 6);

    power_reset_and_clear_hub_port(&mut uhci, &mut mem, 1, 3);
    enumerate_keyboard(&mut uhci, &mut mem, 7);

    // Functional proof: each device has an independent interrupt-in endpoint.
    keyboard1.key_event(0x04, true); // 'a'
    keyboard2.key_event(0x05, true); // 'b'
    keyboard3.key_event(0x06, true); // 'c'

    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_KBD_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED | TD_STATUS_NAK), 0);
    assert_eq!(
        mem.slice(BUF_KBD_INT as usize..BUF_KBD_INT as usize + 8),
        [0x00, 0x00, 0x04, 0, 0, 0, 0, 0]
    );

    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 6, 1, 0, 8),
        BUF_KBD2_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED | TD_STATUS_NAK), 0);
    assert_eq!(
        mem.slice(BUF_KBD2_INT as usize..BUF_KBD2_INT as usize + 8),
        [0x00, 0x00, 0x05, 0, 0, 0, 0, 0]
    );

    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 7, 1, 0, 8),
        BUF_KBD3_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & (TD_STATUS_ACTIVE | TD_STATUS_STALLED | TD_STATUS_NAK), 0);
    assert_eq!(
        mem.slice(BUF_KBD3_INT as usize..BUF_KBD3_INT as usize + 8),
        [0x00, 0x00, 0x06, 0, 0, 0, 0, 0]
    );
}
