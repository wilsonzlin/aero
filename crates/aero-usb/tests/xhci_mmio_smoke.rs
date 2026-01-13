use aero_usb::xhci::regs;
use aero_usb::xhci::XhciController;
use aero_usb::MemoryBus;

mod util;

use util::TestMemory;

#[derive(Default)]
struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

fn op_base(ctrl: &mut XhciController, mem: &mut dyn MemoryBus) -> u64 {
    let cap0 = ctrl.mmio_read(mem, regs::cap::CAPLENGTH as u64, 4);
    (cap0 & 0xff) as u64
}

#[test]
fn xhci_caplength_hciversion_plausible() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;

    let cap0 = ctrl.mmio_read(&mut mem, regs::cap::CAPLENGTH as u64, 4);
    let caplength = (cap0 & 0xff) as u8;
    let hciversion = (cap0 >> 16) as u16;

    assert_eq!(caplength, regs::CAPLENGTH_VALUE);
    assert_eq!(hciversion, regs::HCIVERSION_VALUE);

    let dboff = ctrl.mmio_read(&mut mem, regs::cap::DBOFF as u64, 4);
    assert_ne!(dboff, 0, "DBOFF should be non-zero");
    assert_eq!(dboff & 0x3, 0, "DBOFF must be dword-aligned");

    let rtsoff = ctrl.mmio_read(&mut mem, regs::cap::RTSOFF as u64, 4);
    assert_ne!(rtsoff, 0, "RTSOFF should be non-zero");
    assert_eq!(rtsoff & 0x1f, 0, "RTSOFF must be 32-byte-aligned");
}

#[test]
fn xhci_run_stop_toggles_halted_bit() {
    let mut ctrl = XhciController::new();
    // Starting the controller triggers a small DMA read from CRCR (used by wrappers to validate
    // PCI Bus Master Enable gating). Use a backing memory bus so the test focuses on RUN/STOP
    // semantics rather than DMA behaviour.
    let mut mem = TestMemory::new(0x1000);

    let op = op_base(&mut ctrl, &mut mem);
    let usbcmd = op + regs::op::USBCMD as u64;
    let usbsts = op + regs::op::USBSTS as u64;

    assert_ne!(
        ctrl.mmio_read(&mut mem, usbsts, 4) & regs::op::USBSTS_HCH,
        0,
        "controller should start halted"
    );

    ctrl.mmio_write(&mut mem, usbcmd, 4, regs::op::USBCMD_RUN_STOP);
    assert_eq!(
        ctrl.mmio_read(&mut mem, usbsts, 4) & regs::op::USBSTS_HCH,
        0,
        "RUN/STOP should clear HCHalted"
    );

    ctrl.mmio_write(&mut mem, usbcmd, 4, 0);
    assert_ne!(
        ctrl.mmio_read(&mut mem, usbsts, 4) & regs::op::USBSTS_HCH,
        0,
        "clearing RUN/STOP should set HCHalted"
    );
}

#[test]
fn xhci_reset_clears_operational_state() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMemory::new(0x2000);

    let op = op_base(&mut ctrl, &mut mem);
    let usbcmd = op + regs::op::USBCMD as u64;
    let usbsts = op + regs::op::USBSTS as u64;
    let crcr = op + regs::op::CRCR as u64;
    let dcbaap = op + regs::op::DCBAAP as u64;
    let config = op + regs::op::CONFIG as u64;

    let rtsoff = ctrl.mmio_read(&mut mem, regs::cap::RTSOFF as u64, 4) as u64;
    let mfindex = rtsoff + regs::runtime::MFINDEX as u64;

    // Dirty some state.
    ctrl.mmio_write(&mut mem, crcr, 4, 0x100);
    ctrl.mmio_write(&mut mem, crcr + 4, 4, 0);
    ctrl.mmio_write(&mut mem, dcbaap, 4, 0x200);
    ctrl.mmio_write(&mut mem, dcbaap + 4, 4, 0);
    ctrl.mmio_write(&mut mem, config, 4, 5);

    ctrl.mmio_write(&mut mem, usbcmd, 4, regs::op::USBCMD_RUN_STOP);
    ctrl.tick_1ms_no_dma();
    assert_ne!(
        ctrl.mmio_read(&mut mem, mfindex, 4) & regs::runtime::MFINDEX_MASK,
        0,
        "MFINDEX should advance while running"
    );

    // Host controller reset should clear state and return to halted.
    ctrl.mmio_write(&mut mem, usbcmd, 4, regs::op::USBCMD_HCRST);

    assert_ne!(
        ctrl.mmio_read(&mut mem, usbsts, 4) & regs::op::USBSTS_HCH,
        0,
        "reset should leave controller halted"
    );
    assert_eq!(
        ctrl.mmio_read(&mut mem, usbcmd, 4) & (regs::op::USBCMD_RUN_STOP | regs::op::USBCMD_HCRST),
        0,
        "reset should clear RUN/STOP and self-clear HCRST"
    );

    assert_eq!(ctrl.mmio_read(&mut mem, crcr, 4), 0);
    assert_eq!(ctrl.mmio_read(&mut mem, crcr + 4, 4), 0);
    assert_eq!(ctrl.mmio_read(&mut mem, dcbaap, 4), 0);
    assert_eq!(ctrl.mmio_read(&mut mem, dcbaap + 4, 4), 0);
    assert_eq!(ctrl.mmio_read(&mut mem, config, 4), 0);
    assert_eq!(ctrl.mmio_read(&mut mem, mfindex, 4), 0);
}
