use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::rc::Rc;

use emulator::io::usb::hid::composite::UsbCompositeHidInputHandle;
use emulator::io::usb::hid::gamepad::UsbHidGamepadHandle;
use emulator::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use emulator::io::usb::hid::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use emulator::io::usb::core::{UsbInResult, UsbOutResult};
use emulator::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};
use emulator::io::usb::uhci::regs::{REG_USBCMD, USBCMD_MAXP, USBCMD_RS};
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};
use emulator::io::usb::uhci::regs::{USBINTR_SHORT_PACKET, USBSTS_USBERRINT, USBSTS_USBINT};
use emulator::io::PortIO;
use memory::MemoryBus;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;
const TD1: u32 = 0x3020;
const TD2: u32 = 0x3040;
const TD3: u32 = 0x3060;
const TD4: u32 = 0x3080;
const TD5: u32 = 0x30a0;
const TD6: u32 = 0x30c0;
const TD7: u32 = 0x30e0;
const TD8: u32 = 0x3100;
const TD9: u32 = 0x3120;

const BUF_SETUP: u32 = 0x4000;
const BUF_DATA: u32 = 0x5000;
const BUF_INT: u32 = 0x6000;

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_STATUS_STALLED: u32 = 1 << 22;
const TD_STATUS_NAK: u32 = 1 << 19;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_SPD: u32 = 1 << 29;

const USB_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
const USB_DESCRIPTOR_TYPE_DEVICE: u8 = 0x01;
const USB_DESCRIPTOR_TYPE_CONFIGURATION: u8 = 0x02;
const USB_DESCRIPTOR_TYPE_HID_REPORT: u8 = 0x22;

// UHCI root hub PORTSC bits (Intel UHCI spec / Linux uhci-hcd).
const PORTSC_CCS: u16 = 0x0001;
const PORTSC_CSC: u16 = 0x0002;
const PORTSC_PED: u16 = 0x0004;
const PORTSC_PEDC: u16 = 0x0008;
const PORTSC_LS_MASK: u16 = 0x0030;
const PORTSC_LS_J_FS: u16 = 0x0010;
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

#[derive(Clone, Debug)]
struct DummyInterruptOutDevice {
    received: Rc<RefCell<Vec<(u8, Vec<u8>)>>>,
}

impl DummyInterruptOutDevice {
    fn new() -> Self {
        Self {
            received: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn received(&self) -> Vec<(u8, Vec<u8>)> {
        self.received.borrow().clone()
    }
}

impl UsbDeviceModel for DummyInterruptOutDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }

    fn handle_interrupt_out(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        self.received.borrow_mut().push((ep_addr, data.to_vec()));
        UsbOutResult::Ack
    }
}

struct TestInterruptInDevice {
    ep: u8,
    data: Vec<u8>,
}

impl TestInterruptInDevice {
    fn new(ep: u8, data: Vec<u8>) -> Self {
        Self { ep, data }
    }
}

impl UsbDeviceModel for TestInterruptInDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn poll_interrupt_in(&mut self, ep: u8) -> Option<Vec<u8>> {
        (ep == self.ep).then(|| self.data.clone())
    }
}

#[derive(Clone, Debug)]
struct DynamicDescriptorDevice {
    device_descriptor: Vec<u8>,
    config_descriptor: Vec<u8>,
}

impl DynamicDescriptorDevice {
    fn new(device_descriptor: Vec<u8>, config_descriptor: Vec<u8>) -> Self {
        Self {
            device_descriptor,
            config_descriptor,
        }
    }
}

impl UsbDeviceModel for DynamicDescriptorDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        match (setup.request_type(), setup.recipient()) {
            (RequestType::Standard, RequestRecipient::Device) => match setup.b_request {
                USB_REQUEST_GET_DESCRIPTOR => {
                    if setup.request_direction() != RequestDirection::DeviceToHost {
                        return ControlResponse::Stall;
                    }
                    let data = match setup.descriptor_type() {
                        USB_DESCRIPTOR_TYPE_DEVICE => Some(self.device_descriptor.clone()),
                        USB_DESCRIPTOR_TYPE_CONFIGURATION => Some(self.config_descriptor.clone()),
                        _ => None,
                    };
                    data.map(ControlResponse::Data)
                        .unwrap_or(ControlResponse::Stall)
                }
                _ => ControlResponse::Stall,
            },
            _ => ControlResponse::Stall,
        }
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Clone, Debug)]
struct PendingControlInDevice {
    ready: Rc<RefCell<bool>>,
    data: Vec<u8>,
}

impl PendingControlInDevice {
    fn new(ready: Rc<RefCell<bool>>, data: Vec<u8>) -> Self {
        Self { ready, data }
    }
}

impl UsbDeviceModel for PendingControlInDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        if setup.bm_request_type == 0xc0 && setup.b_request == 0x01 {
            if *self.ready.borrow() {
                ControlResponse::Data(self.data.clone())
            } else {
                ControlResponse::Nak
            }
        } else {
            ControlResponse::Stall
        }
    }
}

#[derive(Clone, Debug)]
struct PendingControlOutDevice {
    ready: Rc<RefCell<bool>>,
    received: Rc<RefCell<Vec<u8>>>,
    started: bool,
}

impl PendingControlOutDevice {
    fn new(ready: Rc<RefCell<bool>>, received: Rc<RefCell<Vec<u8>>>) -> Self {
        Self {
            ready,
            received,
            started: false,
        }
    }
}

impl UsbDeviceModel for PendingControlOutDevice {
    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        if setup.bm_request_type != 0x40 || setup.b_request != 0x02 {
            return ControlResponse::Stall;
        }

        let Some(data_stage) = data_stage else {
            return ControlResponse::Stall;
        };

        if !self.started {
            self.received.borrow_mut().extend_from_slice(data_stage);
            self.started = true;
        }

        if *self.ready.borrow() {
            ControlResponse::Ack
        } else {
            ControlResponse::Nak
        }
    }
}

#[derive(Clone, Debug)]
struct BulkEndpointDevice {
    in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
    out_received: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl BulkEndpointDevice {
    fn new(
        in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
        out_received: Rc<RefCell<Vec<Vec<u8>>>>,
    ) -> Self {
        Self {
            in_queue,
            out_received,
        }
    }
}

impl UsbDeviceModel for BulkEndpointDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        if ep_addr != 0x81 {
            return UsbInResult::Stall;
        }

        let Some(mut data) = self.in_queue.borrow_mut().pop_front() else {
            return UsbInResult::Nak;
        };
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        if ep_addr != 0x01 {
            return UsbOutResult::Stall;
        }

        self.out_received.borrow_mut().push(data.to_vec());
        UsbOutResult::Ack
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
    assert_eq!(st & PORTSC_LS_MASK, PORTSC_LS_J_FS);

    write_portsc(&mut uhci, 0x10, PORTSC_PR);
    let st = read_portsc(&uhci, 0x10);
    assert_ne!(st & PORTSC_PR, 0);
    assert_eq!(st & PORTSC_LSDA, 0);
    assert_eq!(st & PORTSC_LS_MASK, 0);

    for _ in 0..50 {
        uhci.tick_1ms(&mut mem);
    }

    let st = read_portsc(&uhci, 0x10);
    assert_eq!(st & PORTSC_PR, 0);
    assert_ne!(st & PORTSC_PED, 0);
    assert_ne!(st & PORTSC_PEDC, 0);
    assert_eq!(st & PORTSC_LS_MASK, PORTSC_LS_J_FS);
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
fn uhci_control_short_packet_detect_stops_qh_for_frame() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x04, 2, USBINTR_SHORT_PACKET as u32);
    uhci.port_write(0x00, 2, 0x0001);

    // GET_DESCRIPTOR(Device) with wLength = 64. The HID keyboard only returns 18 bytes, so the
    // third 8-byte IN TD will see a short packet (2 bytes).
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 64, 0x00],
    );

    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_SETUP, 0, 0, 0, 8),
        BUF_SETUP,
    );

    // Chain 8 IN TDs for the full 64 bytes (8 * 8).
    let in_tds = [TD1, TD2, TD3, TD4, TD5, TD6, TD7, TD8];
    for (i, &td) in in_tds.iter().enumerate() {
        let next = if i + 1 < in_tds.len() {
            in_tds[i + 1]
        } else {
            TD9
        };
        write_td(
            &mut mem,
            td,
            next,
            td_status(true, false) | TD_CTRL_SPD,
            td_token(PID_IN, 0, 0, (i as u8 + 1) & 1, 8),
            BUF_DATA + (i as u32) * 8,
        );
    }

    // Status stage (OUT ZLP). This should not be reached in the first frame due to SPD stopping
    // at the short packet.
    write_td(
        &mut mem,
        TD9,
        1,
        td_status(true, false),
        td_token(PID_OUT, 0, 0, 1, 0),
        0,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    // Only the first three IN TDs should complete: 8 + 8 + 2 bytes = 18 total.
    for td in [TD1, TD2, TD3] {
        let st = mem.read_u32(td as u64 + 4);
        assert_eq!(st & TD_STATUS_ACTIVE, 0, "TD {td:#x} should have completed");
    }
    for td in [TD4, TD5, TD6, TD7, TD8] {
        let st = mem.read_u32(td as u64 + 4);
        assert_ne!(st & TD_STATUS_ACTIVE, 0, "TD {td:#x} should remain active");
    }

    // Short-packet interrupt should be raised; no error interrupt.
    let usbsts = uhci.controller.regs().usbsts;
    assert_ne!(usbsts & USBSTS_USBINT, 0);
    assert_eq!(usbsts & USBSTS_USBERRINT, 0);

    // QH element pointer should point to the first unprocessed TD (4th IN TD).
    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD4);
}

#[test]
fn uhci_control_get_descriptor_device_runtime_descriptor() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let device_desc = vec![
        0x12, 0x01, 0x00, 0x02, 0xff, 0x00, 0x00, 0x40, 0x34, 0x12, 0x02, 0x00, 0x00, 0x01,
        0x01, 0x02, 0x00, 0x01,
    ];
    // Minimal config descriptor (total length 9).
    let config_desc = vec![0x09, 0x02, 0x09, 0x00, 0x00, 0x01, 0x00, 0x80, 50];
    uhci.controller.hub_mut().attach(
        0,
        Box::new(DynamicDescriptorDevice::new(
            device_desc.clone(),
            config_desc.clone(),
        )),
    );
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

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + 18),
        device_desc
    );

    // GET_DESCRIPTOR(Configuration) to ensure the config descriptor can also be provided at runtime.
    let config_len = config_desc.len() as u16;
    mem.write_physical(
        BUF_SETUP as u64,
        &[
            0x80,
            USB_REQUEST_GET_DESCRIPTOR,
            0x00,
            USB_DESCRIPTOR_TYPE_CONFIGURATION,
            0x00,
            0x00,
            (config_len & 0x00ff) as u8,
            (config_len >> 8) as u8,
        ],
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
        td_token(PID_IN, 0, 0, 1, config_desc.len()),
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

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + config_desc.len()),
        config_desc
    );
}

#[test]
fn uhci_control_in_pending_naks_data_td_until_ready() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let ready = Rc::new(RefCell::new(false));
    let response_data = vec![0x11, 0x22, 0x33, 0x44];
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(PendingControlInDevice::new(ready.clone(), response_data.clone())));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // Vendor DeviceToHost control transfer with a 4-byte data stage.
    mem.write_physical(
        BUF_SETUP as u64,
        &[0xc0, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00],
    );

    let sentinel = [0xa5, 0xa5, 0xa5, 0xa5];
    mem.write_physical(BUF_DATA as u64, &sentinel);

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
        td_token(PID_IN, 0, 0, 1, response_data.len()),
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

    let st0 = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    let st1 = mem.read_u32(TD1 as u64 + 4);
    assert_ne!(st1 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st1 & TD_STATUS_NAK, 0);

    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD1);
    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + sentinel.len()),
        sentinel
    );

    *ready.borrow_mut() = true;
    uhci.tick_1ms(&mut mem);

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + response_data.len()),
        response_data
    );

    let st1 = mem.read_u32(TD1 as u64 + 4);
    let st2 = mem.read_u32(TD2 as u64 + 4);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st2 & TD_STATUS_ACTIVE, 0);
    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, 1);
}

#[test]
fn uhci_control_out_pending_acks_data_stage_and_naks_status_in() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let ready = Rc::new(RefCell::new(false));
    let received = Rc::new(RefCell::new(Vec::new()));
    uhci.controller.hub_mut().attach(
        0,
        Box::new(PendingControlOutDevice::new(ready.clone(), received.clone())),
    );
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // Vendor HostToDevice control transfer with a 4-byte OUT data stage.
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x40, 0x02, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00],
    );

    let payload = [0xde, 0xad, 0xbe, 0xef];
    mem.write_physical(BUF_DATA as u64, &payload);

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
        td_token(PID_OUT, 0, 0, 1, payload.len()),
        BUF_DATA,
    );
    write_td(
        &mut mem,
        TD2,
        1,
        td_status(true, true),
        td_token(PID_IN, 0, 0, 1, 0),
        0,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let st0 = mem.read_u32(TD0 as u64 + 4);
    let st1 = mem.read_u32(TD1 as u64 + 4);
    let st2 = mem.read_u32(TD2 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st2 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st2 & TD_STATUS_NAK, 0);

    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD2);
    assert_eq!(received.borrow().as_slice(), payload);

    *ready.borrow_mut() = true;
    uhci.tick_1ms(&mut mem);

    let st2 = mem.read_u32(TD2 as u64 + 4);
    assert_eq!(st2 & TD_STATUS_ACTIVE, 0);
    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, 1);
    assert_eq!(received.borrow().as_slice(), payload);
}

#[test]
fn uhci_bulk_in_out_smoke_test_with_nak() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    uhci.controller.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(in_queue.clone(), out_received.clone())),
    );
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    let out_payload = [0x10, 0x20, 0x30];
    mem.write_physical(BUF_DATA as u64, &out_payload);

    let sentinel = [0xa5, 0xa5, 0xa5, 0xa5];
    mem.write_physical(BUF_INT as u64, &sentinel);

    // Bulk OUT EP1 (ACK) followed by bulk IN EP1 (initially NAK).
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_OUT, 0, 1, 0, out_payload.len()),
        BUF_DATA,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, true),
        td_token(PID_IN, 0, 1, 0, 3),
        BUF_INT,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let st0 = mem.read_u32(TD0 as u64 + 4);
    let st1 = mem.read_u32(TD1 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st1 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st1 & TD_STATUS_NAK, 0);
    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD1);

    assert_eq!(out_received.borrow().as_slice(), &[out_payload.to_vec()]);
    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + sentinel.len()),
        sentinel
    );

    in_queue.borrow_mut().push_back(vec![1, 2, 3]);
    uhci.tick_1ms(&mut mem);

    let st1 = mem.read_u32(TD1 as u64 + 4);
    assert_eq!(st1 & TD_STATUS_ACTIVE, 0);
    assert_eq!(mem.slice(BUF_INT as usize..BUF_INT as usize + 3), [1, 2, 3]);
    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, 1);
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
    assert!(st & TD_STATUS_NAK != 0); // NAK
}

#[test]
fn uhci_interrupt_in_out_passthrough_device_queues_reports() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    let report_descriptor = vec![
        0x05, 0x01, // Usage Page (Generic Desktop)
        0x09, 0x00, // Usage (Undefined)
        0xa1, 0x01, // Collection (Application)
        0x85, 0x01, // Report ID (1)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x81, 0x02, // Input (Data,Var,Abs)
        0x85, 0x02, // Report ID (2)
        0x09, 0x00, // Usage (Undefined)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x02, // Report Count (2)
        0x91, 0x02, // Output (Data,Var,Abs)
        0xc0, // End Collection
    ];

    let report_len = report_descriptor.len() as u16;
    let passthrough = UsbHidPassthroughHandle::new(
        0x1234,
        0x5678,
        "Vendor".to_string(),
        "Product".to_string(),
        None,
        report_descriptor.clone(),
        true,
        None,
        None,
        None,
    );
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(passthrough.clone()));
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

    // Fetch the (runtime-provided) HID report descriptor.
    mem.write_physical(
        BUF_SETUP as u64,
        &[
            0x81,
            USB_REQUEST_GET_DESCRIPTOR,
            0x00,
            USB_DESCRIPTOR_TYPE_HID_REPORT,
            0x00,
            0x00,
            (report_len & 0x00ff) as u8,
            (report_len >> 8) as u8,
        ],
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
        td_token(PID_IN, 5, 0, 1, report_descriptor.len()),
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

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + report_descriptor.len()),
        report_descriptor
    );

    passthrough.push_input_report(1, &[0xaa, 0xbb]);

    // Poll interrupt endpoint 1 at address 5. (Report ID prefix + 2 bytes payload)
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 3),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    assert_eq!(
        mem.slice(BUF_INT as usize..BUF_INT as usize + 3),
        [1, 0xaa, 0xbb]
    );

    // Poll again without new input: should NAK and remain active.
    mem.write_u32(TD0 as u64 + 4, td_status(true, false));
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert!(st & TD_STATUS_ACTIVE != 0);
    assert!(st & TD_STATUS_NAK != 0); // NAK

    // Interrupt OUT: first byte is report ID since report IDs are present.
    let payload = [7u8, 0x10, 0x20];
    mem.write_physical(BUF_DATA as u64, &payload);
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_OUT, 5, 1, 0, payload.len()),
        BUF_DATA,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);

    assert_eq!(
        passthrough.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 7,
            data: vec![0x10, 0x20],
        })
    );
}

#[test]
fn uhci_interrupt_in_halted_endpoint_stalls() {
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

    // SET_FEATURE(ENDPOINT_HALT) for endpoint 0x81.
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x02, 0x03, 0x00, 0x00, 0x81, 0x00, 0x00, 0x00],
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

    // Poll interrupt endpoint 1: should STALL while halted.
    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_IN, 5, 1, 0, 8),
        BUF_INT,
    );
    run_one_frame(&mut uhci, &mut mem, TD0);
    let st = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st & TD_STATUS_ACTIVE, 0);
    assert_ne!(st & TD_STATUS_STALLED, 0);

    // CLEAR_FEATURE(ENDPOINT_HALT) for endpoint 0x81.
    mem.write_physical(
        BUF_SETUP as u64,
        &[0x02, 0x01, 0x00, 0x00, 0x81, 0x00, 0x00, 0x00],
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

    // Poll again: should deliver the pending 'a' report.
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
}

#[test]
fn uhci_qh_does_not_skip_inactive_tds() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(TestInterruptInDevice::new(0x81, vec![1, 2, 3, 4])));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    let sentinel = [0xa5, 0xa5, 0xa5, 0xa5];
    mem.write_physical(BUF_DATA as u64, &sentinel);

    // QH -> TD0(inactive) -> TD1(active IN), so the controller must stop at TD0 and not
    // advance the QH element pointer to TD1.
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(false, false),
        td_token(PID_IN, 0, 1, 0, sentinel.len()),
        BUF_DATA,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, true),
        td_token(PID_IN, 0, 1, 0, sentinel.len()),
        BUF_DATA,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD0);

    let st1 = mem.read_u32(TD1 as u64 + 4);
    assert!(st1 & TD_STATUS_ACTIVE != 0);

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + sentinel.len()),
        sentinel
    );
}

#[test]
fn uhci_qh_does_not_skip_nak_tds() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.controller.hub_mut().attach(
        0,
        Box::new(TestInterruptInDevice::new(0x82, vec![0x11, 0x22, 0x33, 0x44])),
    );
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    let sentinel = [0x5a, 0x5a, 0x5a, 0x5a];
    mem.write_physical(BUF_DATA as u64, &sentinel);

    // QH -> TD0(active IN, NAK) -> TD1(active IN, would return data). On NAK, UHCI must stop
    // processing the QH for this frame and must not advance the element pointer to TD1.
    write_td(
        &mut mem,
        TD0,
        TD1,
        td_status(true, false),
        td_token(PID_IN, 0, 1, 0, sentinel.len()),
        BUF_DATA,
    );
    write_td(
        &mut mem,
        TD1,
        1,
        td_status(true, true),
        td_token(PID_IN, 0, 2, 0, sentinel.len()),
        BUF_DATA,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let qh_elem = mem.read_u32(QH_ADDR as u64 + 4);
    assert_eq!(qh_elem, TD0);

    let st0 = mem.read_u32(TD0 as u64 + 4);
    assert!(st0 & TD_STATUS_ACTIVE != 0);
    assert!(st0 & (1 << 19) != 0); // NAK

    let st1 = mem.read_u32(TD1 as u64 + 4);
    assert!(st1 & TD_STATUS_ACTIVE != 0);

    assert_eq!(
        mem.slice(BUF_DATA as usize..BUF_DATA as usize + sentinel.len()),
        sentinel
    );
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
    assert!(st & TD_STATUS_NAK != 0); // NAK
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
    composite.gamepad_button_event(1, true);

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
        [0x01, 0x00, 0x08, 0, 0, 0, 0, 0]
    );
}

#[test]
fn uhci_interrupt_out_reaches_device_model() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    let device = DummyInterruptOutDevice::new();
    uhci.controller.hub_mut().attach(0, Box::new(device.clone()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // Give the device a non-zero address so the OUT TD exercises address routing.
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

    let payload = [0xde, 0xad, 0xbe, 0xef];
    mem.write_physical(BUF_DATA as u64, &payload);

    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_OUT, 5, 1, 0, payload.len()),
        BUF_DATA,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    assert_eq!(device.received(), vec![(0x01, payload.to_vec())]);

    let st0 = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_eq!(st0 & TD_STATUS_STALLED, 0);
}

#[test]
fn uhci_interrupt_out_unimplemented_endpoint_stalls() {
    let mut mem = TestMemBus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    reset_port(&mut uhci, &mut mem, 0x10);

    uhci.port_write(0x08, 4, FRAME_LIST_BASE);
    uhci.port_write(0x00, 2, 0x0001);

    // Give the device a non-zero address so the OUT TD exercises address routing.
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

    let payload = [0x01u8, 0x02, 0x03];
    mem.write_physical(BUF_DATA as u64, &payload);

    write_td(
        &mut mem,
        TD0,
        1,
        td_status(true, false),
        td_token(PID_OUT, 5, 1, 0, payload.len()),
        BUF_DATA,
    );

    run_one_frame(&mut uhci, &mut mem, TD0);

    let st0 = mem.read_u32(TD0 as u64 + 4);
    assert_eq!(st0 & TD_STATUS_ACTIVE, 0);
    assert_ne!(st0 & TD_STATUS_STALLED, 0);
}
