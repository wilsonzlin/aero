use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::ehci::regs::{
    reg_portsc, PORTSC_PP, PORTSC_PR, REG_ASYNCLISTADDR, REG_USBCMD, REG_USBINTR, REG_USBSTS,
    USBINTR_USBERRINT,
    USBINTR_USBINT, USBSTS_USBERRINT, USBSTS_USBINT, USBCMD_ASE, USBCMD_RS,
};
use aero_usb::ehci::EhciController;
use aero_usb::{
    ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbSpeed,
};

mod util;

use util::{Alloc, TestMemory};

const LINK_TERMINATE: u32 = 1 << 0;
const LINK_TYPE_QH: u32 = 0b01 << 1;

const QH_HORIZ: u32 = 0x00;
const QH_EPCHAR: u32 = 0x04;
const QH_CUR_QTD: u32 = 0x0c;
const QH_NEXT_QTD: u32 = 0x10;
const QH_ALT_NEXT_QTD: u32 = 0x14;
const QH_TOKEN: u32 = 0x18;
const QH_BUF0: u32 = 0x1c;

const QTD_NEXT: u32 = 0x00;
const QTD_ALT_NEXT: u32 = 0x04;
const QTD_TOKEN: u32 = 0x08;
const QTD_BUF0: u32 = 0x0c;

const QTD_STS_ACTIVE: u32 = 1 << 7;
const QTD_STS_HALT: u32 = 1 << 6;
const QTD_STS_XACTERR: u32 = 1 << 3;
const QTD_IOC: u32 = 1 << 15;
const QTD_TOTAL_BYTES_SHIFT: u32 = 16;

const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

fn qh_ep_char(dev_addr: u8, endpoint: u8, max_packet: u16) -> u32 {
    let speed_high: u32 = 2;
    (dev_addr as u32)
        | ((endpoint as u32) << 8)
        | (speed_high << 12)
        | ((max_packet as u32) << 16)
}

fn qtd_token(pid: u32, total_bytes: u16, active: bool, ioc: bool) -> u32 {
    let mut tok = ((pid & 0x3) << 8) | ((total_bytes as u32) << QTD_TOTAL_BYTES_SHIFT);
    if active {
        tok |= QTD_STS_ACTIVE;
    }
    if ioc {
        tok |= QTD_IOC;
    }
    tok
}

fn write_qtd(mem: &mut TestMemory, addr: u32, next: u32, token: u32, buf0: u32) {
    mem.write_u32(addr + QTD_NEXT, next);
    mem.write_u32(addr + QTD_ALT_NEXT, LINK_TERMINATE);
    mem.write_u32(addr + QTD_TOKEN, token);
    mem.write_u32(addr + QTD_BUF0, buf0);
    // Unused buffer pointers.
    mem.write_u32(addr + QTD_BUF0 + 4, 0);
    mem.write_u32(addr + QTD_BUF0 + 8, 0);
    mem.write_u32(addr + QTD_BUF0 + 12, 0);
    mem.write_u32(addr + QTD_BUF0 + 16, 0);
}

fn write_qh(mem: &mut TestMemory, qh_addr: u32, ep_char: u32, first_qtd: u32) {
    mem.write_u32(qh_addr + QH_HORIZ, qh_addr | LINK_TYPE_QH);
    mem.write_u32(qh_addr + QH_EPCHAR, ep_char);
    mem.write_u32(qh_addr + 0x08, 0); // Endpoint capabilities (unused)
    mem.write_u32(qh_addr + QH_CUR_QTD, 0);
    mem.write_u32(qh_addr + QH_NEXT_QTD, first_qtd);
    mem.write_u32(qh_addr + QH_ALT_NEXT_QTD, LINK_TERMINATE);
    mem.write_u32(qh_addr + QH_TOKEN, 0);
    for i in 0..5 {
        mem.write_u32(qh_addr + QH_BUF0 + i * 4, 0);
    }
}

fn setup_packet_bytes(setup: SetupPacket) -> [u8; 8] {
    [
        setup.bm_request_type,
        setup.b_request,
        setup.w_value.to_le_bytes()[0],
        setup.w_value.to_le_bytes()[1],
        setup.w_index.to_le_bytes()[0],
        setup.w_index.to_le_bytes()[1],
        setup.w_length.to_le_bytes()[0],
        setup.w_length.to_le_bytes()[1],
    ]
}

#[derive(Default, Debug)]
struct DummyState {
    configured: bool,
    bulk_reads: usize,
}

#[derive(Clone, Debug)]
struct DummyHsDevice(Rc<RefCell<DummyState>>);

impl DummyHsDevice {
    fn new(state: Rc<RefCell<DummyState>>) -> Self {
        Self(state)
    }
}

impl UsbDeviceModel for DummyHsDevice {
    fn speed(&self) -> UsbSpeed {
        UsbSpeed::High
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        // We only need SET_CONFIGURATION for this test; everything else can ACK.
        if setup.b_request == 0x09 {
            self.0.borrow_mut().configured = true;
        }
        ControlResponse::Ack
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        if ep == 0x81 {
            let mut st = self.0.borrow_mut();
            if st.bulk_reads == 0 {
                st.bulk_reads += 1;
                let data = vec![1u8, 2, 3, 4];
                return UsbInResult::Data(data[..data.len().min(max_len)].to_vec());
            }
            return UsbInResult::Nak;
        }
        UsbInResult::Stall
    }
}

#[derive(Clone, Debug)]
struct NakInDevice;

impl UsbDeviceModel for NakInDevice {
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

    fn handle_in_transfer(&mut self, _ep: u8, _max_len: usize) -> UsbInResult {
        UsbInResult::Nak
    }
}

#[test]
fn ehci_async_executes_control_and_bulk_transfers() {
    let mut mem = TestMemory::new(0x20000);
    let mut ctrl = EhciController::new();
    let state = Rc::new(RefCell::new(DummyState::default()));
    ctrl.hub_mut().attach(0, Box::new(DummyHsDevice::new(state.clone())));

    // Reset + enable the port (EHCI models a deterministic 50ms reset).
    // Preserve PORTSC.PP while asserting reset; the EHCI model treats PP as software-controlled
    // (HCSPARAMS.PPC=1).
    ctrl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT | USBINTR_USBERRINT);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_ASE);
    let mut alloc = Alloc::new(0x1000);

    let qh_addr = alloc.alloc(0x40, 0x20);
    let qtd0 = alloc.alloc(0x20, 0x20);
    let qtd1 = alloc.alloc(0x20, 0x20);
    let setup_buf = alloc.alloc(0x1000, 0x1000);
    let status_buf = alloc.alloc(0x1000, 0x1000);

    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, qh_addr);

    // ---------------------------------------------------------------------
    // SET_ADDRESS (IOC=1 on status stage) should set USBSTS.USBINT.
    // ---------------------------------------------------------------------
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    mem.write(setup_buf, &setup_packet_bytes(setup));

    write_qtd(
        &mut mem,
        qtd0,
        qtd1, // next
        qtd_token(PID_SETUP, 8, true, false),
        setup_buf,
    );
    write_qtd(
        &mut mem,
        qtd1,
        LINK_TERMINATE,
        qtd_token(PID_IN, 0, true, true), // IOC=1
        status_buf,
    );
    write_qh(
        &mut mem,
        qh_addr,
        qh_ep_char(0, 0, 64),
        qtd0, // first qTD
    );

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
        let tok0 = mem.read_u32(qtd0 + QTD_TOKEN);
        let tok1 = mem.read_u32(qtd1 + QTD_TOKEN);
        if (tok0 & QTD_STS_ACTIVE) == 0 && (tok1 & QTD_STS_ACTIVE) == 0 {
            break;
        }
    }

    let tok0 = mem.read_u32(qtd0 + QTD_TOKEN);
    let tok1 = mem.read_u32(qtd1 + QTD_TOKEN);
    assert_eq!(tok0 & QTD_STS_ACTIVE, 0, "SETUP qTD should complete");
    assert_eq!(tok1 & QTD_STS_ACTIVE, 0, "STATUS qTD should complete");
    assert_eq!((tok0 >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff, 0);
    assert_eq!((tok1 >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff, 0);

    assert_ne!(
        ctrl.mmio_read(REG_USBSTS, 4) & USBSTS_USBINT,
        0,
        "IOC qTD should raise USBINT"
    );
    assert!(
        ctrl.irq_level(),
        "USBINT should propagate to irq_level when enabled"
    );

    assert!(
        ctrl.hub_mut().device_mut_for_address(1).is_some(),
        "SET_ADDRESS should apply after STATUS stage"
    );

    // ---------------------------------------------------------------------
    // SET_CONFIGURATION (IOC=0) should *not* set USBSTS.USBINT.
    // ---------------------------------------------------------------------
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09, // SET_CONFIGURATION
        w_value: 1,
        w_index: 0,
        w_length: 0,
    };
    mem.write(setup_buf, &setup_packet_bytes(setup));

    write_qtd(
        &mut mem,
        qtd0,
        qtd1,
        qtd_token(PID_SETUP, 8, true, false),
        setup_buf,
    );
    write_qtd(
        &mut mem,
        qtd1,
        LINK_TERMINATE,
        qtd_token(PID_IN, 0, true, false), // IOC=0
        status_buf,
    );
    write_qh(&mut mem, qh_addr, qh_ep_char(1, 0, 64), qtd0);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
        let tok0 = mem.read_u32(qtd0 + QTD_TOKEN);
        let tok1 = mem.read_u32(qtd1 + QTD_TOKEN);
        if (tok0 & QTD_STS_ACTIVE) == 0 && (tok1 & QTD_STS_ACTIVE) == 0 {
            break;
        }
    }

    assert_eq!(
        ctrl.mmio_read(REG_USBSTS, 4) & USBSTS_USBINT,
        0,
        "qTD completion without IOC should not raise USBINT"
    );
    assert!(
        state.borrow().configured,
        "SET_CONFIGURATION should reach the device model (at STATUS stage)"
    );

    // ---------------------------------------------------------------------
    // Bulk/interrupt IN transfer (IOC=1) should write data and raise USBINT.
    // ---------------------------------------------------------------------
    let bulk_qtd = alloc.alloc(0x20, 0x20);
    let bulk_buf = alloc.alloc(0x1000, 0x1000);
    // Clear destination buffer.
    mem.write(bulk_buf, &[0u8; 8]);

    write_qtd(
        &mut mem,
        bulk_qtd,
        LINK_TERMINATE,
        qtd_token(PID_IN, 4, true, true),
        bulk_buf,
    );
    write_qh(&mut mem, qh_addr, qh_ep_char(1, 1, 64), bulk_qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
        let tok = mem.read_u32(bulk_qtd + QTD_TOKEN);
        if (tok & QTD_STS_ACTIVE) == 0 {
            break;
        }
    }

    let tok = mem.read_u32(bulk_qtd + QTD_TOKEN);
    assert_eq!(tok & QTD_STS_ACTIVE, 0, "bulk IN qTD should complete");
    assert_eq!((tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff, 0);

    let mut got = [0u8; 4];
    mem.read(bulk_buf, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    assert_ne!(ctrl.mmio_read(REG_USBSTS, 4) & USBSTS_USBINT, 0);
}

#[test]
fn ehci_async_missing_device_halts_qtd_and_sets_usberrint() {
    let mut mem = TestMemory::new(0x20000);
    let mut ctrl = EhciController::new();
    // Attach a device but do not enumerate it to the target address (default address remains 0).
    ctrl.hub_mut().attach(0, Box::new(DummyHsDevice::new(Rc::new(RefCell::new(
        DummyState::default(),
    )))));

    ctrl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT | USBINTR_USBERRINT);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_ASE);

    let mut alloc = Alloc::new(0x1000);
    let qh_addr = alloc.alloc(0x40, 0x20);
    let qtd = alloc.alloc(0x20, 0x20);
    let buf = alloc.alloc(0x1000, 0x1000);

    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, qh_addr);

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        qtd_token(PID_IN, 0, true, true),
        buf,
    );
    // Target a non-existent device address.
    write_qh(&mut mem, qh_addr, qh_ep_char(5, 0, 64), qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    ctrl.tick_1ms(&mut mem);

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_eq!(tok & QTD_STS_ACTIVE, 0, "missing device should clear Active");
    assert_ne!(tok & QTD_STS_HALT, 0, "missing device should set Halt");
    assert_ne!(tok & QTD_STS_XACTERR, 0, "missing device should set XactErr");

    let sts = ctrl.mmio_read(REG_USBSTS, 4);
    assert_ne!(sts & USBSTS_USBERRINT, 0, "USBERRINT should be asserted");
    assert_ne!(sts & USBSTS_USBINT, 0, "IOC should still assert USBINT");
    assert!(ctrl.irq_level(), "IRQ should be raised when enabled");
}

#[test]
fn ehci_async_nak_leaves_qtd_active_and_no_interrupt() {
    let mut mem = TestMemory::new(0x20000);
    let mut ctrl = EhciController::new();
    ctrl.hub_mut().attach(0, Box::new(NakInDevice));

    ctrl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT | USBINTR_USBERRINT);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_ASE);

    let mut alloc = Alloc::new(0x1000);
    let qh_addr = alloc.alloc(0x40, 0x20);
    let qtd = alloc.alloc(0x20, 0x20);
    let buf = alloc.alloc(0x1000, 0x1000);

    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, qh_addr);

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        qtd_token(PID_IN, 4, true, true),
        buf,
    );
    write_qh(&mut mem, qh_addr, qh_ep_char(0, 1, 64), qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    ctrl.tick_1ms(&mut mem);

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_ne!(tok & QTD_STS_ACTIVE, 0, "NAK should leave qTD active");
    assert_eq!(
        (tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff,
        4,
        "NAK should not consume bytes"
    );

    let sts = ctrl.mmio_read(REG_USBSTS, 4);
    assert_eq!(sts & USBSTS_USBINT, 0, "no USBINT without completion");
    assert_eq!(sts & USBSTS_USBERRINT, 0, "no USBERRINT on NAK");
    assert!(!ctrl.irq_level(), "no IRQ without asserted status bits");
}
