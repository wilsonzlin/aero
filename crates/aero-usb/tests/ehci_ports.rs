use aero_usb::ehci::regs::{
    reg_portsc, CONFIGFLAG_CF, PORTSC_CSC, PORTSC_FPR, PORTSC_HSP, PORTSC_LS_MASK, PORTSC_PED,
    PORTSC_PEDC, PORTSC_PO, PORTSC_PP, PORTSC_PR, PORTSC_SUSP, REG_CONFIGFLAG, REG_USBSTS,
    USBSTS_PCD,
};
use aero_usb::ehci::EhciController;
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbSpeed, UsbWebUsbPassthroughDevice};

mod util;

use util::TestMemory;

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
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10, "expected J-state when idle");

    // Suspend the port, then force resume and verify line status flips to K state.
    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_SUSP);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_SUSP, 0);

    ehci.mmio_write(reg_portsc(0), 4, portsc | PORTSC_FPR);
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_ne!(portsc & PORTSC_FPR, 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b01 << 10, "expected K-state while resuming");

    // After the resume timer expires, the port should exit suspend/resume and return to J state.
    for _ in 0..20 {
        ehci.tick_1ms(&mut mem);
    }
    let portsc = ehci.mmio_read(reg_portsc(0), 4);
    assert_eq!(portsc & (PORTSC_SUSP | PORTSC_FPR), 0);
    assert_eq!(portsc & PORTSC_LS_MASK, 0b10 << 10);
}

