use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::ehci::regs::*;
use aero_usb::ehci::EhciController;
use aero_usb::hid::UsbHidKeyboard;
use aero_usb::MemoryBus;

use std::boxed::Box;

struct NoMem;

impl MemoryBus for NoMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        // EHCI schedule DMA is not exercised by this snapshot test.
        panic!("unexpected guest memory read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected guest memory write");
    }
}

#[test]
fn ehci_controller_snapshot_roundtrips_registers_and_ports() {
    let mut ctrl = EhciController::new_with_port_count(2);

    // Attach a keyboard to port 0.
    ctrl.hub_mut().attach(0, Box::new(UsbHidKeyboard::new()));

    // Claim ports for EHCI (clears PORTSC.PORT_OWNER), then enable the port.
    ctrl.mmio_write(REG_CONFIGFLAG, 4, CONFIGFLAG_CF);
    let port0 = ctrl.mmio_read(reg_portsc(0), 4);
    ctrl.mmio_write(reg_portsc(0), 4, port0 | PORTSC_PED);

    // Latch an interrupt so we can verify derived irq_level restores.
    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT);
    ctrl.set_usbsts_bits(USBSTS_USBINT);
    assert!(ctrl.irq_level());

    let snapshot = ctrl.save_state();

    let mut restored = EhciController::new_with_port_count(2);
    restored.load_state(&snapshot).unwrap();

    assert!(restored.irq_level());
    assert_eq!(
        restored.mmio_read(REG_CONFIGFLAG, 4) & CONFIGFLAG_CF,
        CONFIGFLAG_CF
    );

    let port0_after = restored.mmio_read(reg_portsc(0), 4);
    assert_ne!(
        port0_after & PORTSC_PO,
        PORTSC_PO,
        "port should be owned by EHCI"
    );
    assert_ne!(
        port0_after & PORTSC_CCS,
        0,
        "port should report device connected"
    );
    assert_ne!(port0_after & PORTSC_PED, 0, "port should be enabled");

    assert!(
        restored.hub().port_device(0).is_some(),
        "attached device should be restored"
    );

    // Device should be reachable by address 0 when enabled and owned by EHCI.
    assert!(
        restored.hub_mut().device_mut_for_address(0).is_some(),
        "restored device should be reachable"
    );

    // Ensure the snapshot path did not touch guest memory.
    let mut mem = NoMem;
    restored.tick_1ms(&mut mem);
}
