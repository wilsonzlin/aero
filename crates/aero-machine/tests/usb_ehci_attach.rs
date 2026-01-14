use aero_machine::{Machine, MachineConfig};
use aero_usb::hid::UsbHidKeyboardHandle;
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
    let mut cfg = MachineConfig::default();
    cfg.enable_pc_platform = true;
    cfg.enable_ehci = true;

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
    let mut cfg = MachineConfig::default();
    cfg.enable_pc_platform = true;
    cfg.enable_ehci = true;

    let mut machine = Machine::new(cfg).expect("machine constructs");
    assert!(machine.ehci().is_some());

    let err = machine
        .usb_ehci_attach_root(6, Box::new(UsbHidKeyboardHandle::new()))
        .expect_err("root port 6 should be out of range (0..=5)");
    assert_eq!(err, UsbHubAttachError::InvalidPort);
}

