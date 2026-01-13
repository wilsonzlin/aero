use std::boxed::Box;

use aero_usb::ehci::regs;
use aero_usb::ehci::EhciController;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

mod util;

use util::{Alloc, TestMemory};

const LP_TERMINATE: u32 = 1 << 0;
const LP_QH: u32 = 0b01 << 1;

const QTD_STATUS_ACTIVE: u32 = 1 << 7;
const QTD_TOKEN_IOC: u32 = 1 << 15;
const QTD_TOKEN_PID_SHIFT: u32 = 8;
const QTD_TOKEN_BYTES_SHIFT: u32 = 16;

const PID_IN: u32 = 1;

#[derive(Debug)]
struct DummyInterruptIn {
    report: Option<Vec<u8>>,
}

impl DummyInterruptIn {
    fn new(report: Vec<u8>) -> Self {
        Self {
            report: Some(report),
        }
    }
}

impl UsbDeviceModel for DummyInterruptIn {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        self.report.take()
    }

    fn handle_interrupt_in(&mut self, ep_addr: u8) -> UsbInResult {
        // Ensure the controller passed an IN endpoint address.
        assert_eq!(ep_addr & 0x80, 0x80);
        match self.poll_interrupt_in(ep_addr) {
            Some(data) => UsbInResult::Data(data),
            None => UsbInResult::Nak,
        }
    }
}

#[test]
fn ehci_periodic_qh_interrupt_in_qtd_completes() {
    let mut mem = TestMemory::new(0x20000);
    let mut alloc = Alloc::new(0x1000);

    let periodic_base = alloc.alloc(4096, 4096);
    let qh_addr = alloc.alloc(64, 32);
    let qtd_addr = alloc.alloc(32, 32);
    let buf_addr = alloc.alloc(64, 4);

    // Periodic frame list: point every frame at the single QH.
    for i in 0..1024u32 {
        mem.write_u32(periodic_base + i * 4, (qh_addr & 0xffff_ffe0) | LP_QH);
    }

    // qTD (IN, 8 bytes, IOC).
    let token = QTD_STATUS_ACTIVE
        | (PID_IN << QTD_TOKEN_PID_SHIFT)
        | QTD_TOKEN_IOC
        | (8 << QTD_TOKEN_BYTES_SHIFT);
    mem.write_u32(qtd_addr + 0x00, LP_TERMINATE); // next qTD
    mem.write_u32(qtd_addr + 0x04, LP_TERMINATE); // alt next qTD
    mem.write_u32(qtd_addr + 0x08, token);
    mem.write_u32(qtd_addr + 0x0c, buf_addr); // buffer 0
    mem.write_u32(qtd_addr + 0x10, 0);
    mem.write_u32(qtd_addr + 0x14, 0);
    mem.write_u32(qtd_addr + 0x18, 0);
    mem.write_u32(qtd_addr + 0x1c, 0);

    // QH: device address 0, endpoint 1, max packet 8, SMASK=uframe 0, next qTD points at qtd.
    let ep_char = (0u32) | (1u32 << 8) | (8u32 << 16);
    let ep_caps = 0x01u32; // SMASK bit0
    mem.write_u32(qh_addr + 0x00, LP_TERMINATE); // horiz link
    mem.write_u32(qh_addr + 0x04, ep_char);
    mem.write_u32(qh_addr + 0x08, ep_caps);
    mem.write_u32(qh_addr + 0x0c, 0); // current qTD
    mem.write_u32(qh_addr + 0x10, qtd_addr); // next qTD
    mem.write_u32(qh_addr + 0x14, LP_TERMINATE); // alt next qTD

    let expected = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];

    let mut ehci = EhciController::new();
    ehci.hub_mut()
        .attach(0, Box::new(DummyInterruptIn::new(expected.clone())));

    // Enable the port by performing a 50ms reset, mirroring the OS driver sequence.
    ehci.mmio_write(regs::reg_portsc(0), 4, regs::PORTSC_PP | regs::PORTSC_PR);
    for _ in 0..50 {
        ehci.tick_1ms(&mut mem);
    }

    ehci.mmio_write(regs::REG_PERIODICLISTBASE, 4, periodic_base);
    ehci.mmio_write(regs::REG_FRINDEX, 4, 0);
    ehci.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    ehci.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    ehci.tick_1ms(&mut mem);

    let mut got = vec![0u8; 8];
    mem.read(buf_addr, &mut got);
    assert_eq!(got, expected);

    let token_after = mem.read_u32(qtd_addr + 0x08);
    assert_eq!(token_after & QTD_STATUS_ACTIVE, 0);
    assert_eq!((token_after >> QTD_TOKEN_BYTES_SHIFT) & 0x7fff, 0);

    // QH should have advanced to the qTD's next pointer (terminate).
    let qh_next_after = mem.read_u32(qh_addr + 0x10);
    assert_ne!(qh_next_after & LP_TERMINATE, 0);

    let usbsts = ehci.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(usbsts & regs::USBSTS_USBINT, 0);
}
