use aero_usb::hid::passthrough::{UsbHidPassthrough, UsbHidPassthroughOutputReport};
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::SetupPacket;
use aero_usb::GuestMemory;

const REG_USBCMD: u16 = 0x00;
const REG_USBINTR: u16 = 0x04;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;

const USBCMD_RUN: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;

const PORTSC_PR: u16 = 1 << 9;

const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

const PID_SETUP: u8 = 0x2D;
const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xE1;

#[derive(Default)]
struct TestIrq {
    raised: bool,
}

impl InterruptController for TestIrq {
    fn raise_irq(&mut self, _irq: u8) {
        self.raised = true;
    }

    fn lower_irq(&mut self, _irq: u8) {
        self.raised = false;
    }
}

struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn read_u32(&self, addr: u32) -> u32 {
        let addr = addr as usize;
        u32::from_le_bytes(self.data[addr..addr + 4].try_into().unwrap())
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        let addr = addr as usize;
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }
}

impl GuestMemory for TestMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        let addr = addr as usize;
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
    }

    fn write(&mut self, addr: u32, buf: &[u8]) {
        let addr = addr as usize;
        self.data[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

#[derive(Default)]
struct Alloc {
    next: u32,
}

impl Alloc {
    fn new(base: u32) -> Self {
        Self { next: base }
    }

    fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let aligned = (self.next + (align - 1)) & !(align - 1);
        self.next = aligned + size;
        aligned
    }
}

fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
    let max_len_field = if max_len == 0 {
        0x7FFu32
    } else {
        (max_len as u32) - 1
    };
    (pid as u32)
        | ((addr as u32) << TD_TOKEN_DEVADDR_SHIFT)
        | ((ep as u32) << TD_TOKEN_ENDPT_SHIFT)
        | (if toggle { TD_TOKEN_D } else { 0 })
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT)
}

fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

fn actlen(ctrl_sts: u32) -> usize {
    let field = ctrl_sts & TD_CTRL_ACTLEN_MASK;
    if field == 0x7FF {
        0
    } else {
        (field as usize) + 1
    }
}

fn install_frame_list(mem: &mut TestMemory, fl_base: u32, qh_addr: u32) {
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }
}

fn write_qh(mem: &mut TestMemory, addr: u32, head: u32, element: u32) {
    mem.write_u32(addr, head);
    mem.write_u32(addr + 4, element);
}

fn write_td(
    mem: &mut TestMemory,
    addr: u32,
    link_ptr: u32,
    ctrl_sts: u32,
    token: u32,
    buffer: u32,
) {
    mem.write_u32(addr, link_ptr);
    mem.write_u32(addr + 4, ctrl_sts);
    mem.write_u32(addr + 8, token);
    mem.write_u32(addr + 12, buffer);
}

struct UhciTestHarness<'a> {
    ctrl: &'a mut UhciController,
    mem: &'a mut TestMemory,
    irq: &'a mut TestIrq,
    alloc: &'a mut Alloc,
    fl_base: u32,
}

impl UhciTestHarness<'_> {
    fn port_write(&mut self, port: u16, size: usize, value: u32) {
        self.ctrl.port_write(port, size, value, &mut *self.irq);
    }

    fn step_frames(&mut self, frames: usize) {
        for _ in 0..frames {
            self.ctrl.step_frame(&mut *self.mem, &mut *self.irq);
        }
    }

    fn control_in(&mut self, devaddr: u8, max_packet: usize, setup: SetupPacket) -> Vec<u8> {
        let ctrl = &mut *self.ctrl;
        let mem = &mut *self.mem;
        let irq = &mut *self.irq;
        let alloc = &mut *self.alloc;
        let fl_base = self.fl_base;

        let qh_addr = alloc.alloc(0x20, 0x10);
        let setup_buf = alloc.alloc(8, 0x10);
        let setup_td = alloc.alloc(0x20, 0x10);

        let mut bytes = [0u8; 8];
        bytes[0] = setup.request_type;
        bytes[1] = setup.request;
        bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
        bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
        bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
        mem.write(setup_buf, &bytes);

        let mut tds = Vec::new();
        tds.push((setup_td, setup_buf, 8usize, PID_SETUP, false)); // SETUP, DATA0

        let mut remaining = setup.length as usize;
        let mut toggle = true;
        while remaining != 0 {
            let chunk = remaining.min(max_packet);
            let buf = alloc.alloc(chunk as u32, 0x10);
            let td = alloc.alloc(0x20, 0x10);
            tds.push((td, buf, chunk, PID_IN, toggle)); // IN
            toggle = !toggle;
            remaining -= chunk;
        }

        // Status stage: OUT zero-length, DATA1.
        let status_td = alloc.alloc(0x20, 0x10);
        tds.push((status_td, 0, 0, PID_OUT, true));

        for i in 0..tds.len() {
            let (td_addr, buf_addr, len, pid, dtoggle) = tds[i];
            let link = if i + 1 == tds.len() {
                LINK_PTR_T
            } else {
                tds[i + 1].0
            };
            let ioc = i + 1 == tds.len();
            write_td(
                mem,
                td_addr,
                link,
                td_ctrl(true, ioc),
                td_token(pid, devaddr, 0, dtoggle, len),
                buf_addr,
            );
        }

        write_qh(mem, qh_addr, LINK_PTR_T, setup_td);
        install_frame_list(mem, fl_base, qh_addr);

        ctrl.step_frame(mem, irq);

        let mut out = Vec::new();
        for (td_addr, buf_addr, _len, pid, _) in tds {
            if pid != PID_IN {
                continue;
            }
            let ctrl_sts = mem.read_u32(td_addr + 4);
            let got = actlen(ctrl_sts);
            let mut tmp = vec![0u8; got];
            mem.read(buf_addr, &mut tmp);
            out.extend_from_slice(&tmp);
        }
        out
    }

    fn control_no_data(&mut self, devaddr: u8, setup: SetupPacket) {
        let ctrl = &mut *self.ctrl;
        let mem = &mut *self.mem;
        let irq = &mut *self.irq;
        let alloc = &mut *self.alloc;
        let fl_base = self.fl_base;

        let qh_addr = alloc.alloc(0x20, 0x10);
        let setup_buf = alloc.alloc(8, 0x10);
        let setup_td = alloc.alloc(0x20, 0x10);
        let status_td = alloc.alloc(0x20, 0x10);

        let mut bytes = [0u8; 8];
        bytes[0] = setup.request_type;
        bytes[1] = setup.request;
        bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
        bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
        bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
        mem.write(setup_buf, &bytes);

        write_td(
            mem,
            setup_td,
            status_td,
            td_ctrl(true, false),
            td_token(PID_SETUP, devaddr, 0, false, 8),
            setup_buf,
        );
        // Status stage: IN zero-length, DATA1.
        write_td(
            mem,
            status_td,
            LINK_PTR_T,
            td_ctrl(true, true),
            td_token(PID_IN, devaddr, 0, true, 0),
            0,
        );
        write_qh(mem, qh_addr, LINK_PTR_T, setup_td);
        install_frame_list(mem, fl_base, qh_addr);

        ctrl.step_frame(mem, irq);
    }

    fn interrupt_in(&mut self, devaddr: u8, ep: u8, max_len: usize) -> Vec<u8> {
        let ctrl = &mut *self.ctrl;
        let mem = &mut *self.mem;
        let irq = &mut *self.irq;
        let alloc = &mut *self.alloc;
        let fl_base = self.fl_base;

        let qh_addr = alloc.alloc(0x20, 0x10);
        let td_addr = alloc.alloc(0x20, 0x10);
        let buf_addr = alloc.alloc(max_len as u32, 0x10);

        write_td(
            mem,
            td_addr,
            LINK_PTR_T,
            td_ctrl(true, true),
            td_token(PID_IN, devaddr, ep, false, max_len),
            buf_addr,
        );
        write_qh(mem, qh_addr, LINK_PTR_T, td_addr);
        install_frame_list(mem, fl_base, qh_addr);

        ctrl.step_frame(mem, irq);

        let ctrl_sts = mem.read_u32(td_addr + 4);
        let got = actlen(ctrl_sts);
        let mut out = vec![0u8; got];
        mem.read(buf_addr, &mut out);
        out
    }

    fn interrupt_out(&mut self, devaddr: u8, ep: u8, payload: &[u8]) {
        let ctrl = &mut *self.ctrl;
        let mem = &mut *self.mem;
        let irq = &mut *self.irq;
        let alloc = &mut *self.alloc;
        let fl_base = self.fl_base;

        let qh_addr = alloc.alloc(0x20, 0x10);
        let td_addr = alloc.alloc(0x20, 0x10);
        let buf_addr = alloc.alloc(payload.len() as u32, 0x10);

        mem.write(buf_addr, payload);

        write_td(
            mem,
            td_addr,
            LINK_PTR_T,
            td_ctrl(true, true),
            td_token(PID_OUT, devaddr, ep, false, payload.len()),
            buf_addr,
        );
        write_qh(mem, qh_addr, LINK_PTR_T, td_addr);
        install_frame_list(mem, fl_base, qh_addr);

        ctrl.step_frame(mem, irq);
    }
}

fn enumerate_hub(uhci: &mut UhciTestHarness<'_>) {
    // GET_DESCRIPTOR(Device)
    let dev_desc = uhci.control_in(
        0,
        64,
        SetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0100,
            index: 0,
            length: 18,
        },
    );
    assert_eq!(dev_desc.len(), 18);
    assert_eq!(dev_desc[4], 0x09, "bDeviceClass should be Hub (0x09)");

    // SET_ADDRESS(1)
    uhci.control_no_data(
        0,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // GET_DESCRIPTOR(Configuration)
    let cfg_desc = uhci.control_in(
        1,
        64,
        SetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0200,
            index: 0,
            length: 25,
        },
    );
    assert_eq!(cfg_desc.len(), 25);
    assert_eq!(cfg_desc[1], 0x02);

    // SET_CONFIGURATION(1)
    uhci.control_no_data(
        1,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // GET_DESCRIPTOR(Hub, type=0x29) via class request.
    let hub_desc = uhci.control_in(
        1,
        64,
        SetupPacket {
            request_type: 0xa0,
            request: 0x06,
            value: 0x2900,
            index: 0,
            length: 64,
        },
    );
    assert!(!hub_desc.is_empty());
    assert_eq!(hub_desc[1], 0x29);
}

fn power_reset_and_clear_hub_port(uhci: &mut UhciTestHarness<'_>, hub_addr: u8, port: u8) {
    // SET_FEATURE(PORT_POWER)
    uhci.control_no_data(
        hub_addr,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 8,
            index: port as u16,
            length: 0,
        },
    );

    // SET_FEATURE(PORT_RESET)
    uhci.control_no_data(
        hub_addr,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 4,
            index: port as u16,
            length: 0,
        },
    );

    // Advance time until reset completes.
    uhci.step_frames(50);

    // Clear change bits so subsequent interrupt polling is deterministic.
    for feature in [20u16, 16u16, 17u16] {
        uhci.control_no_data(
            hub_addr,
            SetupPacket {
                request_type: 0x23,
                request: 0x01,
                value: feature,
                index: port as u16,
                length: 0,
            },
        );
    }
}

fn enumerate_passthrough_device(
    uhci: &mut UhciTestHarness<'_>,
    address: u8,
    expected_vendor_id: u16,
    expected_product_id: u16,
) {
    let device_desc = uhci.control_in(
        0,
        64,
        SetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0100,
            index: 0,
            length: 18,
        },
    );
    assert_eq!(&device_desc[..2], &[0x12, 0x01]);
    let vendor_id = u16::from_le_bytes([device_desc[8], device_desc[9]]);
    let product_id = u16::from_le_bytes([device_desc[10], device_desc[11]]);
    assert_eq!(vendor_id, expected_vendor_id);
    assert_eq!(product_id, expected_product_id);

    // SET_ADDRESS(address)
    uhci.control_no_data(
        0,
        SetupPacket {
            request_type: 0x00,
            request: 0x05,
            value: address as u16,
            index: 0,
            length: 0,
        },
    );

    // GET_DESCRIPTOR(Configuration) at new address.
    let cfg = uhci.control_in(
        address,
        64,
        SetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0200,
            index: 0,
            length: 64,
        },
    );
    assert_eq!(cfg[1], 0x02);

    // SET_CONFIGURATION(1)
    uhci.control_no_data(
        address,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );
}

#[test]
fn uhci_external_hub_enumerates_multiple_passthrough_hid_devices() {
    let io_base = 0x5000;
    let mut ctrl = UhciController::new(io_base, 11);

    ctrl.connect_device(0, Box::new(UsbHubDevice::new()));

    // Attach 3 passthrough HID devices behind the hub (ports 1..3).
    let report_descriptor = vec![
        0x06, 0x00, 0xff, // Usage Page (Vendor-defined 0xFF00)
        0x09, 0x01, // Usage (1)
        0xa1, 0x01, // Collection (Application)
        0x15, 0x00, // Logical Minimum (0)
        0x26, 0xff, 0x00, // Logical Maximum (255)
        0x75, 0x08, // Report Size (8)
        0x95, 0x08, // Report Count (8)
        0x09, 0x01, // Usage (1)
        0x81, 0x02, // Input (Data,Var,Abs)
        0xc0, // End Collection
    ];

    ctrl.bus_mut().attach_at_path(
        &[0, 1],
        Box::new(UsbHidPassthrough::new(
            0x1234,
            0x0001,
            "Vendor".to_string(),
            "Device 1".to_string(),
            None,
            report_descriptor.clone(),
            true,
            None,
            None,
            None,
        )),
    );
    ctrl.bus_mut().attach_at_path(
        &[0, 2],
        Box::new(UsbHidPassthrough::new(
            0x1234,
            0x0002,
            "Vendor".to_string(),
            "Device 2".to_string(),
            None,
            report_descriptor.clone(),
            true,
            None,
            None,
            None,
        )),
    );
    ctrl.bus_mut().attach_at_path(
        &[0, 3],
        Box::new(UsbHidPassthrough::new(
            0x1234,
            0x0003,
            "Vendor".to_string(),
            "Device 3".to_string(),
            None,
            report_descriptor,
            true,
            None,
            None,
            None,
        )),
    );

    let mut mem = TestMemory::new(0x40000);
    let mut irq = TestIrq::default();
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    let mut uhci = UhciTestHarness {
        ctrl: &mut ctrl,
        mem: &mut mem,
        irq: &mut irq,
        alloc: &mut alloc,
        fl_base,
    };

    uhci.port_write(io_base + REG_FRBASEADD, 4, fl_base);
    uhci.port_write(io_base + REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable root port 1.
    uhci.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32);
    uhci.step_frames(50);

    uhci.port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32);

    // Enumerate and configure the hub itself at address 0 -> 1.
    enumerate_hub(&mut uhci);

    // Enable port 1 and validate the hub interrupt endpoint reports change.
    uhci.control_no_data(
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 8, // PORT_POWER
            index: 1,
            length: 0,
        },
    );
    uhci.control_no_data(
        1,
        SetupPacket {
            request_type: 0x23,
            request: 0x03,
            value: 4, // PORT_RESET
            index: 1,
            length: 0,
        },
    );
    uhci.step_frames(50);
    let bitmap = uhci.interrupt_in(1, 1, 1);
    assert_eq!(bitmap.len(), 1);
    assert_ne!(
        bitmap[0] & 0x02,
        0,
        "expected port1 change bit in hub bitmap"
    );

    // Clear change bits for port1 and then enumerate device 1 at address 5.
    for feature in [20u16, 16u16, 17u16] {
        uhci.control_no_data(
            1,
            SetupPacket {
                request_type: 0x23,
                request: 0x01,
                value: feature,
                index: 1,
                length: 0,
            },
        );
    }

    enumerate_passthrough_device(&mut uhci, 5, 0x1234, 0x0001);

    // Power + reset port2 and enumerate device2 at address 6.
    power_reset_and_clear_hub_port(&mut uhci, 1, 2);
    enumerate_passthrough_device(&mut uhci, 6, 0x1234, 0x0002);

    // Power + reset port3 and enumerate device3 at address 7.
    power_reset_and_clear_hub_port(&mut uhci, 1, 3);
    enumerate_passthrough_device(&mut uhci, 7, 0x1234, 0x0003);

    // Functional proof: each device has independent interrupt IN and OUT endpoints.
    let report1 = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let report2 = [0x11u8, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18];
    let report3 = [0x21u8, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28];

    for (addr, report) in [(5u8, report1), (6, report2), (7, report3)] {
        let dev = uhci
            .ctrl
            .bus_mut()
            .device_mut_for_address(addr)
            .unwrap()
            .as_any_mut()
            .downcast_mut::<UsbHidPassthrough>()
            .unwrap();
        dev.push_input_report(0, &report);
    }

    let got1 = uhci.interrupt_in(5, 1, 8);
    assert_eq!(got1, report1);
    let got2 = uhci.interrupt_in(6, 1, 8);
    assert_eq!(got2, report2);
    let got3 = uhci.interrupt_in(7, 1, 8);
    assert_eq!(got3, report3);

    let out1 = [0xaa, 0xbb, 0xcc];
    let out2 = [0x10, 0x20];
    let out3 = [0xde, 0xad, 0xbe, 0xef];

    uhci.interrupt_out(5, 1, &out1);
    uhci.interrupt_out(6, 1, &out2);
    uhci.interrupt_out(7, 1, &out3);

    let dev1 = uhci
        .ctrl
        .bus_mut()
        .device_mut_for_address(5)
        .unwrap()
        .as_any_mut()
        .downcast_mut::<UsbHidPassthrough>()
        .unwrap();
    assert_eq!(
        dev1.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out1.to_vec(),
        })
    );

    let dev2 = uhci
        .ctrl
        .bus_mut()
        .device_mut_for_address(6)
        .unwrap()
        .as_any_mut()
        .downcast_mut::<UsbHidPassthrough>()
        .unwrap();
    assert_eq!(
        dev2.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out2.to_vec(),
        })
    );

    let dev3 = uhci
        .ctrl
        .bus_mut()
        .device_mut_for_address(7)
        .unwrap()
        .as_any_mut()
        .downcast_mut::<UsbHidPassthrough>()
        .unwrap();
    assert_eq!(
        dev3.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out3.to_vec(),
        })
    );

    assert!(uhci.irq.raised);
}
