use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::{UsbHidPassthroughHandle, UsbHidPassthroughOutputReport};
use aero_usb::hub::UsbHubDevice;
use aero_usb::uhci::regs;
use aero_usb::uhci::UhciController;
use aero_usb::SetupPacket;

mod util;

use util::{
    actlen, install_frame_list, td_ctrl, td_token, write_qh, write_td, Alloc, TestMemory,
    LINK_PTR_T, PORTSC_PR, REG_FRBASEADD, REG_PORTSC1, REG_USBCMD, REG_USBINTR, TD_CTRL_ACTIVE,
    USBCMD_RUN, USBINTR_IOC,
};

const PID_SETUP: u8 = 0x2D;
const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xE1;

fn control_no_data(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    devaddr: u8,
    setup: SetupPacket,
) {
    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    let mut bytes = [0u8; 8];
    bytes[0] = setup.bm_request_type;
    bytes[1] = setup.b_request;
    bytes[2..4].copy_from_slice(&setup.w_value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.w_index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.w_length.to_le_bytes());
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

    ctrl.tick_1ms(mem);
}

fn interrupt_in(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    devaddr: u8,
    ep: u8,
    len: usize,
) -> Vec<u8> {
    let qh_addr = alloc.alloc(0x20, 0x10);
    let buf = alloc.alloc(len as u32, 0x10);
    let td = alloc.alloc(0x20, 0x10);

    write_td(
        mem,
        td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(PID_IN, devaddr, ep, true, len),
        buf,
    );
    write_qh(mem, qh_addr, LINK_PTR_T, td);
    install_frame_list(mem, fl_base, qh_addr);

    ctrl.tick_1ms(mem);

    let ctrl_sts = mem.read_u32(td + 4);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
    let got = actlen(ctrl_sts);
    let mut out = vec![0u8; got];
    mem.read(buf, &mut out);
    out
}

fn interrupt_out(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    devaddr: u8,
    ep: u8,
    data: &[u8],
) {
    let qh_addr = alloc.alloc(0x20, 0x10);
    let buf = alloc.alloc(data.len() as u32, 0x10);
    let td = alloc.alloc(0x20, 0x10);

    mem.write(buf, data);
    write_td(
        mem,
        td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(PID_OUT, devaddr, ep, true, data.len()),
        buf,
    );
    write_qh(mem, qh_addr, LINK_PTR_T, td);
    install_frame_list(mem, fl_base, qh_addr);

    ctrl.tick_1ms(mem);
    assert_eq!(mem.read_u32(td + 4) & TD_CTRL_ACTIVE, 0);
}

fn setup_set_address(addr: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05,
        w_value: addr,
        w_index: 0,
        w_length: 0,
    }
}

fn setup_set_configuration(cfg: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09,
        w_value: cfg,
        w_index: 0,
        w_length: 0,
    }
}

fn setup_hub_set_port_feature(port: u16, feature: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x23,
        b_request: 0x03,
        w_value: feature,
        w_index: port,
        w_length: 0,
    }
}

fn setup_hub_clear_port_feature(port: u16, feature: u16) -> SetupPacket {
    SetupPacket {
        bm_request_type: 0x23,
        b_request: 0x01,
        w_value: feature,
        w_index: port,
        w_length: 0,
    }
}

fn power_reset_and_clear_hub_port(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    hub_addr: u8,
    port: u16,
) {
    // SET_FEATURE(PORT_POWER).
    control_no_data(
        ctrl,
        mem,
        alloc,
        fl_base,
        hub_addr,
        setup_hub_set_port_feature(port, 8),
    );
    // SET_FEATURE(PORT_RESET).
    control_no_data(
        ctrl,
        mem,
        alloc,
        fl_base,
        hub_addr,
        setup_hub_set_port_feature(port, 4),
    );
    // Advance time until reset completes.
    for _ in 0..50 {
        ctrl.tick_1ms(mem);
    }
    // Clear relevant change bits.
    for feature in [20u16, 16u16, 17u16] {
        control_no_data(
            ctrl,
            mem,
            alloc,
            fl_base,
            hub_addr,
            setup_hub_clear_port_feature(port, feature),
        );
    }
}

#[test]
fn uhci_external_hub_enumerates_downstream_hid() {
    let mut ctrl = UhciController::new();

    // Root port0: external USB hub.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // A keyboard behind downstream port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .unwrap();

    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable root port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RUN | regs::USBCMD_MAXP) as u32);

    // Enumerate and configure the hub itself at address 0 -> 1.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(1),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_set_configuration(1),
    );

    // Power + reset downstream port 1 so the keyboard becomes routable.
    power_reset_and_clear_hub_port(&mut ctrl, &mut mem, &mut alloc, fl_base, 1, 1);

    // Enumerate and configure the downstream keyboard at address 5.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(5),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        5,
        setup_set_configuration(1),
    );

    keyboard.key_event(0x04, true); // 'a'
    let report = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 5, 1, 8);
    assert_eq!(report.len(), 8);
    assert_eq!(report[2], 0x04);
    assert!(ctrl.irq_level());
}

#[test]
fn uhci_external_hub_enumerates_multiple_downstream_hid_devices() {
    let mut ctrl = UhciController::new();

    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    let keyboard1 = UsbHidKeyboardHandle::new();
    let keyboard2 = UsbHidKeyboardHandle::new();
    let keyboard3 = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard1.clone()))
        .unwrap();
    ctrl.hub_mut()
        .attach_at_path(&[0, 2], Box::new(keyboard2.clone()))
        .unwrap();
    ctrl.hub_mut()
        .attach_at_path(&[0, 3], Box::new(keyboard3.clone()))
        .unwrap();

    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable root port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RUN | regs::USBCMD_MAXP) as u32);

    // Enumerate and configure hub at address 0 -> 1.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(1),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_set_configuration(1),
    );

    // Enumerate each downstream keyboard on ports 1..3 at addresses 5..7.
    for (port, addr) in [(1u16, 5u16), (2u16, 6u16), (3u16, 7u16)] {
        power_reset_and_clear_hub_port(&mut ctrl, &mut mem, &mut alloc, fl_base, 1, port);
        control_no_data(
            &mut ctrl,
            &mut mem,
            &mut alloc,
            fl_base,
            0,
            setup_set_address(addr),
        );
        control_no_data(
            &mut ctrl,
            &mut mem,
            &mut alloc,
            fl_base,
            addr as u8,
            setup_set_configuration(1),
        );
    }

    keyboard1.key_event(0x04, true); // 'a'
    keyboard2.key_event(0x05, true); // 'b'
    keyboard3.key_event(0x06, true); // 'c'

    let report1 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 5, 1, 8);
    let report2 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 6, 1, 8);
    let report3 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 7, 1, 8);

    assert_eq!(report1[2], 0x04);
    assert_eq!(report2[2], 0x05);
    assert_eq!(report3[2], 0x06);
}

#[test]
fn uhci_external_hub_enumerates_device_behind_nested_hubs() {
    let mut ctrl = UhciController::new();

    // Root port0: hub1 -> hub2 -> keyboard (all full-speed).
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));
    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(UsbHubDevice::new()))
        .unwrap();
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut()
        .attach_at_path(&[0, 1, 1], Box::new(keyboard.clone()))
        .unwrap();

    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable root port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RUN | regs::USBCMD_MAXP) as u32);

    // Enumerate and configure hub1 at address 0 -> 1.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(1),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_set_configuration(1),
    );

    // Enable hub1 downstream port1 so hub2 becomes routable at address 0.
    power_reset_and_clear_hub_port(&mut ctrl, &mut mem, &mut alloc, fl_base, 1, 1);

    // Enumerate hub2 at address 0 -> 2 and configure it.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(2),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        2,
        setup_set_configuration(1),
    );

    // Enable hub2 downstream port1 so the keyboard becomes routable at address 0.
    power_reset_and_clear_hub_port(&mut ctrl, &mut mem, &mut alloc, fl_base, 2, 1);

    // Enumerate the downstream keyboard at address 0 -> 5.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(5),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        5,
        setup_set_configuration(1),
    );

    keyboard.key_event(0x04, true); // 'a'
    let report = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 5, 1, 8);
    assert_eq!(report[2], 0x04);
}

#[test]
fn uhci_external_hub_enumerates_multiple_passthrough_hid_devices() {
    let mut ctrl = UhciController::new();

    // Root port0: external USB hub.
    ctrl.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

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

    let dev1 = UsbHidPassthroughHandle::new(
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
    );
    let dev2 = UsbHidPassthroughHandle::new(
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
    );
    let dev3 = UsbHidPassthroughHandle::new(
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
    );

    ctrl.hub_mut()
        .attach_at_path(&[0, 1], Box::new(dev1.clone()))
        .unwrap();
    ctrl.hub_mut()
        .attach_at_path(&[0, 2], Box::new(dev2.clone()))
        .unwrap();
    ctrl.hub_mut()
        .attach_at_path(&[0, 3], Box::new(dev3.clone()))
        .unwrap();

    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FRBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable root port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RUN | regs::USBCMD_MAXP) as u32);

    // Enumerate and configure the hub itself at address 0 -> 1.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(1),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_set_configuration(1),
    );

    // Power + reset port1 and validate the hub interrupt endpoint reports a change.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(1, 8), // PORT_POWER
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(1, 4), // PORT_RESET
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }
    let bitmap = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 1, 1, 1);
    assert_eq!(bitmap.len(), 1);
    assert_ne!(
        bitmap[0] & 0x02,
        0,
        "expected port1 change bit in hub bitmap"
    );

    // Clear change bits for port1.
    for feature in [20u16, 16u16, 17u16] {
        control_no_data(
            &mut ctrl,
            &mut mem,
            &mut alloc,
            fl_base,
            1,
            setup_hub_clear_port_feature(1, feature),
        );
    }

    // Enumerate device1 at address 5.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(5),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        5,
        setup_set_configuration(1),
    );
    assert!(dev1.configured());

    // Power+reset port2, enumerate device2 at address 6.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(2, 8),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(2, 4),
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }
    for feature in [20u16, 16u16, 17u16] {
        control_no_data(
            &mut ctrl,
            &mut mem,
            &mut alloc,
            fl_base,
            1,
            setup_hub_clear_port_feature(2, feature),
        );
    }
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(6),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        6,
        setup_set_configuration(1),
    );
    assert!(dev2.configured());

    // Power+reset port3, enumerate device3 at address 7.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(3, 8),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        setup_hub_set_port_feature(3, 4),
    );
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }
    for feature in [20u16, 16u16, 17u16] {
        control_no_data(
            &mut ctrl,
            &mut mem,
            &mut alloc,
            fl_base,
            1,
            setup_hub_clear_port_feature(3, feature),
        );
    }
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        setup_set_address(7),
    );
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        7,
        setup_set_configuration(1),
    );
    assert!(dev3.configured());

    // Functional proof: each device has independent interrupt IN and OUT endpoints.
    let report1 = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
    let report2 = [0x11u8, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18];
    let report3 = [0x21u8, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28];

    dev1.push_input_report(0, &report1);
    dev2.push_input_report(0, &report2);
    dev3.push_input_report(0, &report3);

    let got1 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 5, 1, 8);
    assert_eq!(got1, report1);
    let got2 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 6, 1, 8);
    assert_eq!(got2, report2);
    let got3 = interrupt_in(&mut ctrl, &mut mem, &mut alloc, fl_base, 7, 1, 8);
    assert_eq!(got3, report3);

    let out1 = [0xaa, 0xbb, 0xcc];
    let out2 = [0x10, 0x20];
    let out3 = [0xde, 0xad, 0xbe, 0xef];

    interrupt_out(&mut ctrl, &mut mem, &mut alloc, fl_base, 5, 1, &out1);
    interrupt_out(&mut ctrl, &mut mem, &mut alloc, fl_base, 6, 1, &out2);
    interrupt_out(&mut ctrl, &mut mem, &mut alloc, fl_base, 7, 1, &out3);

    assert_eq!(
        dev1.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out1.to_vec(),
        })
    );
    assert_eq!(
        dev2.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out2.to_vec(),
        })
    );
    assert_eq!(
        dev3.pop_output_report(),
        Some(UsbHidPassthroughOutputReport {
            report_type: 2,
            report_id: 0,
            data: out3.to_vec(),
        })
    );

    assert!(ctrl.irq_level());
}
