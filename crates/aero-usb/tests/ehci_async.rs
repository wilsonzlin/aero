use std::cell::RefCell;
use std::rc::Rc;

use aero_usb::ehci::regs::{
    reg_portsc, PORTSC_PP, PORTSC_PR, REG_ASYNCLISTADDR, REG_USBCMD, REG_USBINTR, REG_USBSTS,
    USBCMD_ASE, USBCMD_RS, USBINTR_USBERRINT, USBINTR_USBINT, USBSTS_USBERRINT, USBSTS_USBINT,
};
use aero_usb::ehci::EhciController;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbSpeed};

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
const QTD_STS_BUFERR: u32 = 1 << 5;
const QTD_STS_XACTERR: u32 = 1 << 3;
const QTD_IOC: u32 = 1 << 15;
const QTD_TOTAL_BYTES_SHIFT: u32 = 16;

const PID_IN: u32 = 1;
const PID_OUT: u32 = 0;
const PID_SETUP: u32 = 2;

fn qh_ep_char(dev_addr: u8, endpoint: u8, max_packet: u16) -> u32 {
    let speed_high: u32 = 2;
    (dev_addr as u32) | ((endpoint as u32) << 8) | (speed_high << 12) | ((max_packet as u32) << 16)
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

fn write_qtd(mem: &mut TestMemory, addr: u32, next: u32, alt_next: u32, token: u32, buf0: u32) {
    mem.write_u32(addr + QTD_NEXT, next);
    mem.write_u32(addr + QTD_ALT_NEXT, alt_next);
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

#[derive(Clone, Debug)]
struct ShortControlInDevice;

impl UsbDeviceModel for ShortControlInDevice {
    fn speed(&self) -> UsbSpeed {
        UsbSpeed::High
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        // Return a fixed 18-byte "device descriptor" payload for GET_DESCRIPTOR(Device).
        if setup.b_request == 0x06 && setup.descriptor_type() == 0x01 {
            return ControlResponse::Data(vec![
                0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01,
                0x01, 0x02, 0x03, 0x01,
            ]);
        }
        ControlResponse::Stall
    }
}

#[derive(Default, Debug)]
struct ChunkedInState {
    calls: usize,
}

#[derive(Clone, Debug)]
struct ChunkedInDevice(Rc<RefCell<ChunkedInState>>);

impl ChunkedInDevice {
    fn new(state: Rc<RefCell<ChunkedInState>>) -> Self {
        Self(state)
    }
}

impl UsbDeviceModel for ChunkedInDevice {
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

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        if ep != 0x81 {
            return UsbInResult::Stall;
        }
        let mut st = self.0.borrow_mut();
        st.calls += 1;
        match st.calls {
            1 => UsbInResult::Data(vec![1u8, 2, 3, 4][..max_len.min(4)].to_vec()),
            2 => UsbInResult::Nak,
            3 => UsbInResult::Data(vec![5u8, 6, 7, 8][..max_len.min(4)].to_vec()),
            _ => UsbInResult::Nak,
        }
    }
}

#[derive(Clone, Debug)]
struct FillInDevice {
    pattern: u8,
}

impl UsbDeviceModel for FillInDevice {
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

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        if ep != 0x81 {
            return UsbInResult::Stall;
        }
        UsbInResult::Data(vec![self.pattern; max_len])
    }
}

#[test]
fn ehci_async_executes_control_and_bulk_transfers() {
    let mut mem = TestMemory::new(0x20000);
    let mut ctrl = EhciController::new();
    let state = Rc::new(RefCell::new(DummyState::default()));
    ctrl.hub_mut()
        .attach(0, Box::new(DummyHsDevice::new(state.clone())));

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
        LINK_TERMINATE,
        qtd_token(PID_SETUP, 8, true, false),
        setup_buf,
    );
    write_qtd(
        &mut mem,
        qtd1,
        LINK_TERMINATE,
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
        LINK_TERMINATE,
        qtd_token(PID_SETUP, 8, true, false),
        setup_buf,
    );
    write_qtd(
        &mut mem,
        qtd1,
        LINK_TERMINATE,
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
fn ehci_async_short_packet_uses_alt_next_to_skip_remaining_qtds() {
    let mut mem = TestMemory::new(0x40000);
    let mut ctrl = EhciController::new();
    ctrl.hub_mut().attach(0, Box::new(ShortControlInDevice));

    // Reset + enable port 0 (and keep power enabled).
    ctrl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT | USBINTR_USBERRINT);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_ASE);

    let mut alloc = Alloc::new(0x1000);
    let qh_addr = alloc.alloc(0x40, 0x20);
    let qtd_setup = alloc.alloc(0x20, 0x20);
    let qtd_data = alloc.alloc(0x20, 0x20);
    let qtd_unused = alloc.alloc(0x20, 0x20);
    let qtd_status = alloc.alloc(0x20, 0x20);

    let setup_buf = alloc.alloc(0x1000, 0x1000);
    let data_buf = alloc.alloc(0x1000, 0x1000);
    let unused_buf = alloc.alloc(0x1000, 0x1000);
    let status_buf = alloc.alloc(0x1000, 0x1000);

    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, qh_addr);

    // GET_DESCRIPTOR(Device) with wLength=64. The device returns only 18 bytes, causing a short
    // packet relative to the qTD TotalBytes.
    let setup = SetupPacket {
        bm_request_type: 0x80,
        b_request: 0x06,
        w_value: 0x0100,
        w_index: 0,
        w_length: 64,
    };
    mem.write(setup_buf, &setup_packet_bytes(setup));

    write_qtd(
        &mut mem,
        qtd_setup,
        qtd_data,
        LINK_TERMINATE,
        qtd_token(PID_SETUP, 8, true, false),
        setup_buf,
    );

    // Data qTD expects 64 bytes, but the device will return a single 18-byte packet. Configure
    // AltNext to jump straight to the STATUS stage so we can skip the unused qTD.
    write_qtd(
        &mut mem,
        qtd_data,
        qtd_unused,
        qtd_status,
        qtd_token(PID_IN, 64, true, false),
        data_buf,
    );

    // This qTD should remain untouched because the short packet on qtd_data will take AltNext.
    write_qtd(
        &mut mem,
        qtd_unused,
        LINK_TERMINATE,
        LINK_TERMINATE,
        qtd_token(PID_IN, 64, true, false),
        unused_buf,
    );

    // Status stage for control-IN is an OUT ZLP.
    write_qtd(
        &mut mem,
        qtd_status,
        LINK_TERMINATE,
        LINK_TERMINATE,
        qtd_token(PID_OUT, 0, true, true),
        status_buf,
    );

    write_qh(&mut mem, qh_addr, qh_ep_char(0, 0, 64), qtd_setup);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
        let tok = mem.read_u32(qtd_status + QTD_TOKEN);
        if (tok & QTD_STS_ACTIVE) == 0 {
            break;
        }
    }

    let tok_data = mem.read_u32(qtd_data + QTD_TOKEN);
    assert_eq!(tok_data & QTD_STS_ACTIVE, 0, "data qTD should complete");
    assert_eq!(
        (tok_data >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff,
        46,
        "short packet should preserve remaining bytes (64-18)"
    );

    let tok_unused = mem.read_u32(qtd_unused + QTD_TOKEN);
    assert_ne!(
        tok_unused & QTD_STS_ACTIVE,
        0,
        "AltNext should skip intermediate qTDs without modifying them"
    );

    let tok_status = mem.read_u32(qtd_status + QTD_TOKEN);
    assert_eq!(tok_status & QTD_STS_ACTIVE, 0, "status qTD should complete");
    assert_ne!(
        ctrl.mmio_read(REG_USBSTS, 4) & USBSTS_USBINT,
        0,
        "IOC on STATUS qTD should raise USBINT"
    );

    let mut got = [0u8; 18];
    mem.read(data_buf, &mut got);
    assert_eq!(
        got,
        [
            0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x78, 0x56, 0x00, 0x01,
            0x01, 0x02, 0x03, 0x01
        ]
    );
}

#[test]
fn ehci_async_in_transfer_spans_five_pages_without_buffer_error() {
    let mut mem = TestMemory::new(0x100000);
    let mut ctrl = EhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(FillInDevice { pattern: 0x5a }));

    ctrl.mmio_write(reg_portsc(0), 4, PORTSC_PP | PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.mmio_write(REG_USBINTR, 4, USBINTR_USBINT | USBINTR_USBERRINT);
    ctrl.mmio_write(REG_USBCMD, 4, USBCMD_RS | USBCMD_ASE);

    let mut alloc = Alloc::new(0x1000);
    let qh_addr = alloc.alloc(0x40, 0x20);
    let qtd = alloc.alloc(0x20, 0x20);

    const TOTAL: u32 = 5 * 4096;
    let buf_base = alloc.alloc(TOTAL, 0x1000);

    ctrl.mmio_write(REG_ASYNCLISTADDR, 4, qh_addr);

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        LINK_TERMINATE,
        qtd_token(PID_IN, TOTAL as u16, true, true),
        buf_base,
    );
    // Populate the remaining buffer page pointers.
    for i in 1..5u32 {
        mem.write_u32(qtd + QTD_BUF0 + i * 4, buf_base + i * 4096);
    }

    write_qh(&mut mem, qh_addr, qh_ep_char(0, 1, 512), qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    for _ in 0..10 {
        ctrl.tick_1ms(&mut mem);
        let tok = mem.read_u32(qtd + QTD_TOKEN);
        if (tok & QTD_STS_ACTIVE) == 0 {
            break;
        }
    }

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_eq!(tok & QTD_STS_ACTIVE, 0, "qTD should complete");
    assert_eq!(
        (tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff,
        0,
        "all bytes should be consumed"
    );
    assert_eq!(
        tok & (QTD_STS_HALT | QTD_STS_BUFERR | QTD_STS_XACTERR),
        0,
        "qTD should not report buffer or transaction errors"
    );
    assert!(
        ((tok >> 12) & 0x7) <= 4,
        "controller must not write reserved CPAGE values"
    );

    let sts = ctrl.mmio_read(REG_USBSTS, 4);
    assert_ne!(sts & USBSTS_USBINT, 0, "IOC should raise USBINT");
    assert_eq!(sts & USBSTS_USBERRINT, 0, "no error interrupt expected");

    let mut got = vec![0u8; TOTAL as usize];
    mem.read(buf_base, &mut got);
    assert!(
        got.iter().all(|&b| b == 0x5a),
        "all DMA-written bytes should match the pattern"
    );
}

#[test]
fn ehci_async_invalid_cpage_halts_qtd_with_buffer_error() {
    let mut mem = TestMemory::new(0x40000);
    let mut ctrl = EhciController::new();
    ctrl.hub_mut()
        .attach(0, Box::new(FillInDevice { pattern: 0xaa }));

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

    // Encode an out-of-range CPAGE (7). A robust EHCI engine should treat this as a buffer error.
    let token = qtd_token(PID_IN, 1, true, true) | (7 << 12);
    write_qtd(&mut mem, qtd, LINK_TERMINATE, LINK_TERMINATE, token, buf);
    write_qh(&mut mem, qh_addr, qh_ep_char(0, 1, 64), qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    ctrl.tick_1ms(&mut mem);

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_eq!(tok & QTD_STS_ACTIVE, 0, "qTD should halt on invalid CPAGE");
    assert_eq!(
        tok & (QTD_STS_HALT | QTD_STS_BUFERR),
        QTD_STS_HALT | QTD_STS_BUFERR,
        "invalid CPAGE should set HALT+BUFERR"
    );

    let sts = ctrl.mmio_read(REG_USBSTS, 4);
    assert_ne!(sts & USBSTS_USBERRINT, 0, "BUFERR should raise USBERRINT");
    assert_ne!(
        sts & USBSTS_USBINT,
        0,
        "IOC should raise USBINT even on error"
    );
    assert!(ctrl.irq_level());
}

#[test]
fn ehci_async_partial_progress_then_nak_updates_total_bytes_and_retries() {
    let mut mem = TestMemory::new(0x40000);
    let mut ctrl = EhciController::new();
    let state = Rc::new(RefCell::new(ChunkedInState::default()));
    ctrl.hub_mut()
        .attach(0, Box::new(ChunkedInDevice::new(state)));

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

    mem.write(buf, &[0u8; 16]);
    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        LINK_TERMINATE,
        qtd_token(PID_IN, 8, true, true),
        buf,
    );
    write_qh(&mut mem, qh_addr, qh_ep_char(0, 1, 4), qtd);

    // Tick once: we should transfer 4 bytes then observe NAK, leaving the qTD active with 4 bytes
    // remaining and no interrupt asserted.
    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    ctrl.tick_1ms(&mut mem);

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_ne!(tok & QTD_STS_ACTIVE, 0, "NAK should leave qTD active");
    assert_eq!(
        (tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff,
        8,
        "qTD token should not be written back while still active"
    );
    let overlay_tok = mem.read_u32(qh_addr + QH_TOKEN);
    assert_ne!(
        overlay_tok & QTD_STS_ACTIVE,
        0,
        "overlay should remain active"
    );
    assert_eq!(
        (overlay_tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff,
        4,
        "partial progress should update remaining bytes in the QH overlay"
    );

    let mut got = [0u8; 4];
    mem.read(buf, &mut got);
    assert_eq!(got, [1, 2, 3, 4]);

    let sts = ctrl.mmio_read(REG_USBSTS, 4);
    assert_eq!(sts & USBSTS_USBINT, 0);
    assert_eq!(sts & USBSTS_USBERRINT, 0);
    assert!(!ctrl.irq_level());

    // Tick again: device returns the remaining 4 bytes and the qTD should complete (IOC=1).
    ctrl.tick_1ms(&mut mem);
    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_eq!(tok & QTD_STS_ACTIVE, 0, "qTD should complete after retry");
    assert_eq!((tok >> QTD_TOTAL_BYTES_SHIFT) & 0x7fff, 0);

    let mut got = [0u8; 8];
    mem.read(buf, &mut got);
    assert_eq!(got, [1, 2, 3, 4, 5, 6, 7, 8]);

    assert_ne!(ctrl.mmio_read(REG_USBSTS, 4) & USBSTS_USBINT, 0);
    assert!(ctrl.irq_level());
}

#[test]
fn ehci_async_missing_device_halts_qtd_and_sets_usberrint() {
    let mut mem = TestMemory::new(0x20000);
    let mut ctrl = EhciController::new();
    // Attach a device but do not enumerate it to the target address (default address remains 0).
    ctrl.hub_mut().attach(
        0,
        Box::new(DummyHsDevice::new(Rc::new(RefCell::new(
            DummyState::default(),
        )))),
    );

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
        LINK_TERMINATE,
        qtd_token(PID_IN, 0, true, true),
        buf,
    );
    // Target a non-existent device address.
    write_qh(&mut mem, qh_addr, qh_ep_char(5, 0, 64), qtd);

    ctrl.mmio_write(REG_USBSTS, 4, USBSTS_USBINT | USBSTS_USBERRINT);
    ctrl.tick_1ms(&mut mem);

    let tok = mem.read_u32(qtd + QTD_TOKEN);
    assert_eq!(
        tok & QTD_STS_ACTIVE,
        0,
        "missing device should clear Active"
    );
    assert_ne!(tok & QTD_STS_HALT, 0, "missing device should set Halt");
    assert_ne!(
        tok & QTD_STS_XACTERR,
        0,
        "missing device should set XactErr"
    );

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
