use aero_usb::device::AttachedUsbDevice;
use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_CSC, PORTSC_FPR, PORTSC_HSP, PORTSC_LS_MASK, PORTSC_PED,
    PORTSC_PEDC, PORTSC_PO, PORTSC_PP, PORTSC_PR, PORTSC_SUSP, REG_CONFIGFLAG, REG_USBSTS,
    USBSTS_PCD,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::hid::mouse::UsbHidMouseHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::usb2_port::Usb2PortMux;
use aero_usb::{
    ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult, UsbSpeed,
    UsbWebUsbPassthroughDevice,
};

use std::cell::RefCell;
use std::rc::Rc;

mod util;

use util::TestMemory;

// Hub-class port features.
const HUB_PORT_FEATURE_RESET: u16 = 4;
const HUB_PORT_FEATURE_POWER: u16 = 8;

struct SpeedOnlyDevice {
    speed: UsbSpeed,
}

impl UsbDeviceModel for SpeedOnlyDevice {
    fn speed(&self) -> UsbSpeed {
        self.speed
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn control_no_data(ehci: &mut EhciController, addr: u8, setup: SetupPacket) {
    let mut dev = ehci
        .hub_mut()
        .device_mut_for_address(addr)
        .unwrap_or_else(|| panic!("expected USB device at address {addr}"));
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

fn control_no_data_dev(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

#[test]
fn ehci_configflag_routes_ports_and_owner_writes_trigger_pcd() {
    let mut ehci = EhciController::new_with_port_count(2);

    // Attach a full-speed device.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut().attach(0, Box::new(keyboard));

    // By default ports are owned by a (non-existent) companion controller.
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);

    // Clear connect status change so PCD can be cleared deterministically.
    if portsc0 & PORTSC_CSC != 0 {
        // W1C: write 1 to clear, preserving other bits.
        ehci.mmio_write(reg_portsc(0), 4, portsc0 | PORTSC_CSC);
    }

    // Clear any lingering PCD (write-1-to-clear).
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // Attempt to enable the port while PORT_OWNER=1: enable/reset should be ignored.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PED);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_PED, 0);

    // Device should not be reachable while PORT_OWNER=1.
    assert!(ehci.hub_mut().device_mut_for_address(0).is_none());

    // CONFIGFLAG 0->1 should route ports to EHCI ownership, clearing PORT_OWNER and asserting PCD.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc0 & PORTSC_PO, 0);

    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_ne!(st & USBSTS_PCD, 0);

    // Enable the port now that EHCI owns it.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PED);

    // The device should now be reachable by the EHCI controller (enabled + owned).
    assert!(ehci.hub_mut().device_mut_for_address(0).is_some());

    // Clear PCD again to test direct PORT_OWNER writes. Since enabling the port set PEDC, clear it.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    if portsc & PORTSC_PEDC != 0 {
        ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PEDC);
    }
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // PORT_OWNER is only writable when the port is disabled. Disable it first (preserving power).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc & !PORTSC_PED);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_PED, 0);

    // Clear the resulting PEDC so PCD can be isolated to the owner change.
    if portsc & PORTSC_PEDC != 0 {
        ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PEDC);
    }
    ehci.mmio_write(REG_USBSTS, 4, USBSTS_PCD);
    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_eq!(st & USBSTS_PCD, 0);

    // Now set PORT_OWNER=1 and ensure PCD latches.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_PO);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);

    let st = ehci.mmio_read(REG_USBSTS, 4);
    assert_ne!(st & USBSTS_PCD, 0);

    // With PORT_OWNER=1 and no companion present, the device is unreachable.
    assert!(ehci.hub_mut().device_mut_for_address(0).is_none());
}

#[test]
fn ehci_portsc_reports_device_speed_via_hsp_and_line_status() {
    let mut ehci = EhciController::new_with_port_count(3);

    // Port 0: high-speed device.
    let hs = UsbWebUsbPassthroughDevice::new_with_speed(UsbSpeed::High);
    ehci.hub_mut().attach(0, Box::new(hs));

    // Port 1: full-speed device.
    ehci.hub_mut()
        .attach(1, Box::new(UsbHidKeyboardHandle::new()));

    // Port 2: low-speed device.
    ehci.hub_mut().attach(
        2,
        Box::new(SpeedOnlyDevice {
            speed: UsbSpeed::Low,
        }),
    );

    // By default ports are companion-owned. PORTSC.HSP must be clear even if a high-speed device is
    // physically attached.
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);
    assert_eq!(portsc0 & PORTSC_HSP, 0);
    assert_eq!(
        portsc0 & PORTSC_LS_MASK,
        0,
        "high-speed should clear LS bits"
    );

    // Full-speed device: HSP clear, LS encodes J-state (D+ high).
    let portsc1 = ehci.mmio_read(reg_portsc(1), 4);
    assert_eq!(portsc1 & PORTSC_HSP, 0);
    assert_eq!(portsc1 & PORTSC_LS_MASK, 0b10 << 10);

    // Low-speed device: HSP clear, LS encodes K-state (D- high).
    let portsc2 = ehci.mmio_read(reg_portsc(2), 4);
    assert_eq!(portsc2 & PORTSC_HSP, 0);
    assert_eq!(portsc2 & PORTSC_LS_MASK, 0b01 << 10);

    // Route ports to EHCI ownership and ensure the high-speed bit becomes visible.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc0 & PORTSC_PO, 0);
    assert_ne!(portsc0 & PORTSC_HSP, 0);
    assert_eq!(
        portsc0 & PORTSC_LS_MASK,
        0,
        "high-speed should clear LS bits"
    );

    // While companion-owned, HSP must remain clear.
    ehci.mmio_write(reg_portsc(0), 4, portsc0 | PORTSC_PO);
    let portsc0 = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc0 & PORTSC_PO, 0);
    assert_eq!(portsc0 & PORTSC_HSP, 0);
}

#[test]
fn ehci_portsc_line_status_flips_to_k_state_during_resume_for_full_speed() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(
        0,
        Box::new(SpeedOnlyDevice {
            speed: UsbSpeed::Full,
        }),
    );

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_HSP, 0);
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b10 << 10,
        "expected J-state when idle"
    );

    // Suspend the port, then force resume and verify line status flips to K state.
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0);
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b10 << 10,
        "expected J-state while suspended"
    );

    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_FPR);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_FPR, 0);
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b10 << 10,
        "expected J-state after resume"
    );
}

#[test]
fn ehci_portsc_reports_low_speed_line_status() {
    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach(
        0,
        Box::new(SpeedOnlyDevice {
            speed: UsbSpeed::Low,
        }),
    );

    // Claim ports for EHCI (clears PORT_OWNER).
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);

    // Reset the port to enable it.
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & PORTSC_HSP, 0, "low-speed device should clear HSP");
    assert_eq!(
        (portsc & PORTSC_LS_MASK) >> 10,
        0b01,
        "low-speed device should report idle K-state via LS"
    );
}

#[test]
fn ehci_keyboard_remote_wakeup_enters_resume_state() {
    let mut ehci = EhciController::new_with_port_count(1);
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Suspend the port.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a keypress while suspended. This should request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wakeup"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
}

#[test]
fn ehci_mouse_remote_wakeup_enters_resume_state_from_boot_scroll() {
    let mut ehci = EhciController::new_with_port_count(1);
    let mouse = UsbHidMouseHandle::new();
    ehci.hub_mut().attach(0, Box::new(mouse.clone()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    // Select HID boot protocol so wheel input is not representable; the mouse should still request
    // remote wakeup on scroll.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x21, // HostToDevice | Class | Interface
            b_request: 0x0b,       // SET_PROTOCOL
            w_value: 0,            // Boot
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(mouse.configured(), "expected mouse to be configured");

    // Suspend the port.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a scroll while suspended. In boot protocol this should not enqueue an interrupt
    // report, but it should still request remote wakeup.
    mouse.wheel(1);

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wakeup"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
}

#[test]
fn ehci_keyboard_remote_wakeup_enters_resume_state_through_usb2_port_mux() {
    let mux = Rc::new(RefCell::new(Usb2PortMux::new(1)));

    let mut ehci = EhciController::new_with_port_count(1);
    ehci.hub_mut().attach_usb2_port_mux(0, mux.clone(), 0);

    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal configuration + enable remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Suspend the port.
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a keypress while suspended. This should request remote wakeup.
    keyboard.key_event(0x04, true); // HID usage for KeyA.

    // Tick once to allow the root hub to observe the remote wakeup request.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wakeup"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(1).is_some());
}

#[test]
fn ehci_keyboard_remote_wakeup_enters_resume_state_through_external_hub() {
    let mut ehci = EhciController::new_with_port_count(1);

    // Root port 0: external hub.
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub: address 0 -> address 1, SET_CONFIGURATION(1), then enable hub
    // remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );

    // Keyboard behind hub downstream port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable device remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Suspend the root port (suspends the hub and downstream devices).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a keypress while suspended. This should request remote wakeup from the keyboard which
    // propagates through the hub and causes the root port to enter resume signaling.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wakeup via external hub"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);

    // The device should be reachable again after resume.
    assert!(ehci.hub_mut().device_mut_for_address(2).is_some());
}

#[test]
fn ehci_keyboard_remote_wakeup_does_not_propagate_through_external_hub_without_hub_remote_wakeup() {
    let mut ehci = EhciController::new_with_port_count(1);

    // Root port 0: external hub.
    ehci.hub_mut().attach(0, Box::new(UsbHubDevice::new()));

    // Claim ports for EHCI (clears PORT_OWNER) and reset the port to enable it.
    ehci.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    ehci.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Enumerate/configure the hub: address 0 -> address 1, SET_CONFIGURATION(1).
    //
    // Note: we intentionally do *not* enable DEVICE_REMOTE_WAKEUP on the hub.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Keyboard behind hub downstream port 1.
    let keyboard = UsbHidKeyboardHandle::new();
    ehci.hub_mut()
        .attach_at_path(&[0, 1], Box::new(keyboard.clone()))
        .expect("attach keyboard behind hub port 1");

    // Power and reset the hub port so the keyboard becomes reachable.
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23, // HostToDevice | Class | Other (port)
            b_request: 0x03,       // SET_FEATURE
            w_value: HUB_PORT_FEATURE_POWER,
            w_index: 1,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        1,
        SetupPacket {
            bm_request_type: 0x23,
            b_request: 0x03, // SET_FEATURE
            w_value: HUB_PORT_FEATURE_RESET,
            w_index: 1,
            w_length: 0,
        },
    );
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    // Minimal enumeration/configuration for the keyboard + enable device remote wakeup.
    control_no_data(
        &mut ehci,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 2,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        &mut ehci,
        2,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x03, // SET_FEATURE
            w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    // Suspend the root port (suspends the hub and downstream devices).
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0, "expected port to be suspended");

    // Inject a keypress while suspended. Since the hub does not have DEVICE_REMOTE_WAKEUP enabled,
    // it must not propagate the downstream remote wake request upstream.
    keyboard.key_event(0x04, true); // HID usage for KeyA.
    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected EHCI resume state even though hub remote wake is disabled"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_none(),
        "device should not be reachable while the root port remains suspended"
    );

    // Enable DEVICE_REMOTE_WAKEUP on the hub *after* the downstream device has already requested
    // remote wake. The hub should have drained the wake request even while propagation was
    // disabled, so enabling remote wake later must not "replay" a stale wake event.
    {
        let mut hub_dev = ehci
            .hub_mut()
            .port_device_mut(0)
            .expect("hub device should be attached");
        control_no_data_dev(
            &mut hub_dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 0x0001, // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    for _ in 0..5 {
        ehci.tick_1ms(&mut mem);
    }

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(
        portsc & PORTSC_FPR,
        0,
        "unexpected EHCI resume state from a stale wake request after enabling hub remote wake"
    );
    assert_ne!(portsc & PORTSC_SUSP, 0, "port should remain suspended");
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state");
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_none(),
        "device should remain unreachable while the root port remains suspended"
    );

    // A fresh key event should now propagate remote wakeup through the hub.
    keyboard.key_event(0x05, true); // HID usage for KeyB.
    ehci.tick_1ms(&mut mem);

    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        portsc & PORTSC_FPR,
        0,
        "expected EHCI port to enter resume state after remote wake once hub remote wake is enabled"
    );
    assert_eq!(
        portsc & PORTSC_LS_MASK,
        0b01 << 10,
        "expected K-state while resuming"
    );

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);
    assert!(
        ehci.hub_mut().device_mut_for_address(2).is_some(),
        "device should be reachable after remote wake resumes the port"
    );
}
