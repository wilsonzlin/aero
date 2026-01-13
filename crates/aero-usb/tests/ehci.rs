use aero_usb::ehci::{regs, EhciController};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};

use crate::util::TestMemory;

mod util;

struct TestDevice;

impl UsbDeviceModel for TestDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

#[test]
fn ehci_capability_registers_are_stable() {
    let c = EhciController::new();

    let cap0 = c.mmio_read(regs::REG_CAPLENGTH_HCIVERSION, 4);
    assert_eq!(cap0 & 0xff, regs::CAPLENGTH as u32);
    assert_eq!((cap0 >> 16) as u16, regs::HCIVERSION);

    // HCSPARAMS should at least encode the number of ports.
    let hcsparams = c.mmio_read(regs::REG_HCSPARAMS, 4);
    assert_eq!((hcsparams & 0x0f) as usize, c.hub().num_ports());

    // Reads should be stable across calls.
    assert_eq!(cap0, c.mmio_read(regs::REG_CAPLENGTH_HCIVERSION, 4));
    assert_eq!(hcsparams, c.mmio_read(regs::REG_HCSPARAMS, 4));
    assert_eq!(c.mmio_read(regs::REG_HCCPARAMS, 4), c.mmio_read(regs::REG_HCCPARAMS, 4));
}

#[test]
fn ehci_port_reset_timer_self_clears() {
    let mut c = EhciController::new();
    c.hub_mut().attach(0, Box::new(TestDevice));

    // Start port reset (and keep power enabled).
    c.mmio_write(regs::reg_portsc(0), 4, regs::PORTSC_PP | regs::PORTSC_PR);
    assert_ne!(c.mmio_read(regs::reg_portsc(0), 4) & regs::PORTSC_PR, 0);

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        c.tick_1ms(&mut mem);
    }

    let portsc = c.mmio_read(regs::reg_portsc(0), 4);
    assert_eq!(portsc & regs::PORTSC_PR, 0);
}

#[test]
fn ehci_hchalted_tracks_usbcmd_run_stop() {
    let mut c = EhciController::new();

    let st0 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st0 & regs::USBSTS_HCHALTED, 0);

    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS);
    let st1 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st1 & regs::USBSTS_HCHALTED, 0);

    c.mmio_write(regs::REG_USBCMD, 4, 0);
    let st2 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st2 & regs::USBSTS_HCHALTED, 0);
}

#[test]
fn ehci_async_advance_doorbell_sets_iaa_and_clears_iaad() {
    let mut c = EhciController::new();
    let mut mem = TestMemory::new(0x1000);

    // Start the controller with a minimal async schedule.
    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, 0x20);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_IAA);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    // Ring the Async Advance Doorbell (IAAD).
    c.mmio_write(
        regs::REG_USBCMD,
        4,
        regs::USBCMD_RS | regs::USBCMD_ASE | regs::USBCMD_IAAD,
    );

    // One tick should be enough to service the doorbell deterministically.
    c.tick_1ms(&mut mem);

    let cmd = c.mmio_read(regs::REG_USBCMD, 4);
    assert_eq!(cmd & regs::USBCMD_IAAD, 0);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_IAA, 0);

    assert!(c.irq_level());

    // W1C clear should drop both USBSTS.IAA and the IRQ line.
    c.mmio_write(regs::REG_USBSTS, 4, regs::USBSTS_IAA);
    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts & regs::USBSTS_IAA, 0);
    assert!(!c.irq_level());
}
