use aero_machine::{Machine, MachineConfig};
use aero_usb::ehci::regs;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::hub::UsbHubDevice;
use aero_usb::UsbHubAttachError;

#[test]
fn usb_ehci_attach_root_is_noop_when_ehci_disabled() {
    let mut machine = Machine::new(MachineConfig::default()).expect("machine constructs");
    assert!(machine.ehci().is_none());

    // Should be a no-op.
    machine
        .usb_ehci_attach_root(0, Box::new(UsbHidKeyboardHandle::new()))
        .expect("attach is a no-op when EHCI is disabled");
}

#[test]
fn usb_ehci_attach_root_attaches_and_detaches_devices() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_ehci: true,
        ..Default::default()
    };
    let mut machine = Machine::new(cfg).expect("machine constructs");
    assert!(machine.ehci().is_some());

    machine
        .usb_ehci_attach_root(0, Box::new(UsbHidKeyboardHandle::new()))
        .expect("attach succeeds");

    let ehci = machine.ehci().expect("ehci still present");
    assert!(
        ehci.borrow().controller().hub().port_device(0).is_some(),
        "device is visible at root port 0"
    );

    machine.usb_ehci_detach_root(0).expect("detach succeeds");

    let ehci = machine.ehci().expect("ehci still present");
    assert!(
        ehci.borrow().controller().hub().port_device(0).is_none(),
        "device is detached from root port 0"
    );
}

#[test]
fn usb_ehci_attach_root_rejects_invalid_port() {
    let cfg = MachineConfig {
        enable_pc_platform: true,
        enable_ehci: true,
        ..Default::default()
    };
    let mut machine = Machine::new(cfg).expect("machine constructs");
    assert!(machine.ehci().is_some());

    let err = machine
        .usb_ehci_attach_root(6, Box::new(UsbHidKeyboardHandle::new()))
        .expect_err("root port 6 should be out of range (0..=5)");
    assert_eq!(err, UsbHubAttachError::InvalidPort);
}

#[test]
fn machine_reset_preserves_host_attached_ehci_devices() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep this test minimal/deterministic.
        enable_uhci: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        enable_virtio_net: false,
        enable_e1000: false,
        enable_vga: false,
        enable_aerogpu: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    };

    let mut machine = Machine::new(cfg).expect("machine constructs");

    // Attach a hub to root port 0 so we can observe topology persistence across reset.
    machine
        .usb_ehci_attach_root(0, Box::new(UsbHubDevice::with_port_count(2)))
        .expect("attach hub");

    // Claim ports for EHCI (CONFIGFLAG=1) so we can observe the reset path restoring the default
    // companion-controller routing decision (CONFIGFLAG=0, PORTSC.PO=1).
    {
        let ehci = machine.ehci().expect("ehci present");
        let mut ehci = ehci.borrow_mut();
        let ctrl = ehci.controller_mut();
        ctrl.mmio_write(regs::REG_CONFIGFLAG, 4, regs::CONFIGFLAG_CF);
        let portsc0 = ctrl.hub().read_portsc(0);
        assert_eq!(portsc0 & regs::PORTSC_PO, 0, "expected EHCI to own port 0");
    }

    machine.reset();

    let ehci = machine.ehci().expect("ehci present after reset");
    let ehci = ehci.borrow();
    let ctrl = ehci.controller();

    assert!(
        ctrl.hub().port_device(0).is_some(),
        "expected host-attached device to remain attached after reset"
    );

    let portsc0 = ctrl.hub().read_portsc(0);
    assert_ne!(
        portsc0 & regs::PORTSC_CCS,
        0,
        "expected port 0 to still report a connected device after reset"
    );
    assert_ne!(
        portsc0 & regs::PORTSC_PO,
        0,
        "expected reset to route ports back to the companion controller (PORTSC.PO=1)"
    );
}
