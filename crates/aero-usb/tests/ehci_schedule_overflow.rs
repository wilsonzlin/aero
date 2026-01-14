use std::collections::BTreeMap;

use aero_usb::ehci::{regs, EhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel, UsbSpeed};

/// Sparse `MemoryBus` that panics if the controller ever tries to DMA to low memory.
///
/// This catches 32-bit address wraparound (e.g. `0xffff_ffe0 + 0x20 -> 0x0000_0000`) in EHCI
/// schedule pointer arithmetic.
#[derive(Default)]
struct GuardedMem {
    bytes: BTreeMap<u64, u8>,
}

impl GuardedMem {
    fn write_u32(&mut self, paddr: u64, value: u32) {
        let bytes = value.to_le_bytes();
        for (i, b) in bytes.iter().enumerate() {
            self.bytes.insert(paddr + i as u64, *b);
        }
    }
}

impl MemoryBus for GuardedMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if paddr < 0x1000 {
            panic!("unexpected DMA read at low address {paddr:#x}");
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = *self.bytes.get(&(paddr + i as u64)).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if paddr < 0x1000 {
            panic!("unexpected DMA write at low address {paddr:#x}");
        }
        for (i, b) in buf.iter().enumerate() {
            self.bytes.insert(paddr + i as u64, *b);
        }
    }
}

#[derive(Clone, Debug)]
struct DummyHsDevice;

impl UsbDeviceModel for DummyHsDevice {
    fn speed(&self) -> UsbSpeed {
        UsbSpeed::High
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }
}

#[test]
fn ehci_async_qh_overlay_addr_overflow_sets_hse_and_halts_without_wrapping() {
    let mut ctrl = EhciController::new();
    ctrl.hub_mut().attach(0, Box::new(DummyHsDevice));

    // Claim + enable port 0 for EHCI (clears PORT_OWNER as part of the full-portsc write).
    ctrl.mmio_write(regs::reg_portsc(0), 4, regs::PORTSC_PP | regs::PORTSC_PED);

    // Point ASYNCLISTADDR at the end of the 32-bit address space so that QH overlay accesses like
    // `qh_addr + QH_BUF0 + 4` would overflow u32.
    const QH_ADDR: u32 = 0xffff_ffe0;
    const QTD_ADDR: u32 = 0x2000;

    let mut mem = GuardedMem::default();

    // QH endpoint characteristics: dev=0, ep=0, high-speed, max packet=64.
    let ep_char = (2u32 << 12) | (64u32 << 16);
    mem.write_u32(u64::from(QH_ADDR) + 0x04, ep_char);
    mem.write_u32(u64::from(QH_ADDR) + 0x0c, 0); // CUR_QTD
    mem.write_u32(u64::from(QH_ADDR) + 0x10, QTD_ADDR); // NEXT_QTD

    // Minimal qTD (token inactive). The overflow happens while copying qTD buffer pointers into the
    // QH overlay, before any transfer execution.
    mem.write_u32(u64::from(QTD_ADDR), 1); // NEXT=terminate
    mem.write_u32(u64::from(QTD_ADDR) + 0x04, 1); // ALT_NEXT=terminate
    mem.write_u32(u64::from(QTD_ADDR) + 0x08, 0); // TOKEN (inactive)
                                                  // Buffer pointer 0..4 (values don't matter for the overflow case).
    mem.write_u32(u64::from(QTD_ADDR) + 0x0c, 0x3000);
    mem.write_u32(u64::from(QTD_ADDR) + 0x10, 0x4000);
    mem.write_u32(u64::from(QTD_ADDR) + 0x14, 0x5000);
    mem.write_u32(u64::from(QTD_ADDR) + 0x18, 0x6000);
    mem.write_u32(u64::from(QTD_ADDR) + 0x1c, 0x7000);

    ctrl.mmio_write(regs::REG_ASYNCLISTADDR, 4, QH_ADDR);
    ctrl.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    // Must not DMA to low memory (would panic) and should surface a schedule fault.
    ctrl.tick_1ms(&mut mem);

    let sts = ctrl.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0, "expected Host System Error");
    assert_ne!(
        sts & regs::USBSTS_USBERRINT,
        0,
        "expected USB Error Interrupt"
    );

    // A schedule fault halts the controller by clearing USBCMD.RS.
    let cmd = ctrl.mmio_read(regs::REG_USBCMD, 4);
    assert_eq!(cmd & regs::USBCMD_RS, 0, "expected controller to be halted");
}
