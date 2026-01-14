use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use aero_usb::ehci::{regs, EhciController};
use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

mod util;

use util::TestMemory;

const MEM_SIZE: usize = 0x20_000;

// Guest RAM layout (all schedule addresses are 32-byte aligned).
const ASYNC_QH: u32 = 0x1000;
const QTD_SETUP: u32 = 0x2000;
const QTD_DATA: u32 = 0x2020;
const QTD_STATUS: u32 = 0x2040;
const QTD_BULK_OUT: u32 = 0x2100;
const QTD_BULK_IN: u32 = 0x2120;

const BUF_SETUP: u32 = 0x4000;
const BUF_DATA: u32 = 0x5000;
const BUF_INT: u32 = 0x6000;

// ---------------------------------------------------------------------------
// Minimal EHCI schedule structure helpers (QH/qTD)
// ---------------------------------------------------------------------------

const LINK_TERMINATE: u32 = 1 << 0;
const LINK_TYPE_QH: u32 = 0b01 << 1;
const LINK_TYPE_SITD: u32 = 0b10 << 1;
const LINK_TYPE_FSTN: u32 = 0b11 << 1;
const LINK_ADDR_MASK: u32 = 0xffff_ffe0;

const QTD_TOKEN_ACTIVE: u32 = 1 << 7;
const QTD_TOKEN_IOC: u32 = 1 << 15;
const QTD_TOKEN_PID_SHIFT: u32 = 8;
const QTD_TOKEN_PID_OUT: u32 = 0b00 << QTD_TOKEN_PID_SHIFT;
const QTD_TOKEN_PID_IN: u32 = 0b01 << QTD_TOKEN_PID_SHIFT;
const QTD_TOKEN_PID_SETUP: u32 = 0b10 << QTD_TOKEN_PID_SHIFT;
const QTD_TOKEN_TOTAL_BYTES_SHIFT: u32 = 16;

const QH_EPCHAR_ENDPT_SHIFT: u32 = 8;
const QH_EPCHAR_SPEED_SHIFT: u32 = 12;
const QH_EPCHAR_MAX_PACKET_SHIFT: u32 = 16;

const QH_SPEED_HIGH: u32 = 2;

fn qh_link_ptr_qh(addr: u32) -> u32 {
    (addr & LINK_ADDR_MASK) | LINK_TYPE_QH
}

fn qtd_token(pid: u32, total_bytes: usize, active: bool, ioc: bool) -> u32 {
    let mut v = pid | ((total_bytes as u32) << QTD_TOKEN_TOTAL_BYTES_SHIFT);
    if active {
        v |= QTD_TOKEN_ACTIVE;
    }
    if ioc {
        v |= QTD_TOKEN_IOC;
    }
    v
}

fn qh_epchar(dev_addr: u8, ep: u8, max_packet: u16) -> u32 {
    (dev_addr as u32)
        | ((ep as u32) << QH_EPCHAR_ENDPT_SHIFT)
        | (QH_SPEED_HIGH << QH_EPCHAR_SPEED_SHIFT)
        | ((max_packet as u32) << QH_EPCHAR_MAX_PACKET_SHIFT)
}

fn write_qtd(mem: &mut TestMemory, addr: u32, next: u32, token: u32, buf0: u32) {
    mem.write_u32(addr + 0x00, next);
    mem.write_u32(addr + 0x04, LINK_TERMINATE); // alt-next terminate
    mem.write_u32(addr + 0x08, token);
    // Buffer pointers (5 dwords).
    mem.write_u32(addr + 0x0c, buf0);
    for i in 1..5u32 {
        mem.write_u32(addr + 0x0c + i * 4, 0);
    }
}

fn write_qh(mem: &mut TestMemory, addr: u32, horiz: u32, ep_char: u32, first_qtd: u32) {
    mem.write_u32(addr + 0x00, horiz);
    mem.write_u32(addr + 0x04, ep_char);
    mem.write_u32(addr + 0x08, 0); // ep caps
    mem.write_u32(addr + 0x0c, 0); // cur qTD (none loaded)
    mem.write_u32(addr + 0x10, first_qtd);
    mem.write_u32(addr + 0x14, LINK_TERMINATE); // alt-next terminate
    mem.write_u32(addr + 0x18, 0); // overlay token
                                   // overlay buffer pointers (5 dwords)
    for i in 0..5u32 {
        mem.write_u32(addr + 0x1c + i * 4, 0);
    }
}

// ---------------------------------------------------------------------------
// Root hub helpers
// ---------------------------------------------------------------------------

fn write_portsc_w1c(c: &mut EhciController, port: usize, w1c: u32) {
    // Clear change bits with the usual read-modify-write so we don't power-off the port.
    let cur = c.mmio_read(regs::reg_portsc(port), 4);
    let preserve = (cur & regs::PORTSC_PED) | regs::PORTSC_PP;
    c.mmio_write(regs::reg_portsc(port), 4, preserve | w1c);
}

fn reset_port(c: &mut EhciController, mem: &mut TestMemory, port: usize) {
    // Claim ownership for EHCI (clears PORTSC.PORT_OWNER on all ports).
    c.mmio_write(regs::REG_CONFIGFLAG, 4, regs::CONFIGFLAG_CF);

    // Trigger port reset and wait ~50ms.
    c.mmio_write(regs::reg_portsc(port), 4, regs::PORTSC_PP | regs::PORTSC_PR);
    for _ in 0..50 {
        c.tick_1ms(mem);
    }

    // Clear latched port-change bits so later tests start from a clean state.
    write_portsc_w1c(c, port, regs::PORTSC_CSC | regs::PORTSC_PEDC);
    c.mmio_write(regs::REG_USBSTS, 4, regs::USBSTS_PCD); // W1C
}

// ---------------------------------------------------------------------------
// Test USB devices
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct BulkEndpointDevice {
    in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
    out_received: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl BulkEndpointDevice {
    fn new(
        in_queue: Rc<RefCell<VecDeque<Vec<u8>>>>,
        out_received: Rc<RefCell<Vec<Vec<u8>>>>,
    ) -> Self {
        Self {
            in_queue,
            out_received,
        }
    }
}

impl UsbDeviceModel for BulkEndpointDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, max_len: usize) -> UsbInResult {
        if ep_addr != 0x81 {
            return UsbInResult::Stall;
        }

        let Some(mut data) = self.in_queue.borrow_mut().pop_front() else {
            return UsbInResult::Nak;
        };
        if data.len() > max_len {
            data.truncate(max_len);
        }
        UsbInResult::Data(data)
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        if ep_addr != 0x01 {
            return UsbOutResult::Stall;
        }
        self.out_received.borrow_mut().push(data.to_vec());
        UsbOutResult::Ack
    }
}

#[derive(Clone, Debug)]
struct CountingOutDevice {
    bytes_written: Rc<Cell<usize>>,
}

impl CountingOutDevice {
    fn new(bytes_written: Rc<Cell<usize>>) -> Self {
        Self { bytes_written }
    }
}

impl UsbDeviceModel for CountingOutDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, data: &[u8]) -> UsbOutResult {
        if ep_addr != 0x01 {
            return UsbOutResult::Stall;
        }
        self.bytes_written
            .set(self.bytes_written.get().saturating_add(data.len()));
        UsbOutResult::Ack
    }
}

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

// ---------------------------------------------------------------------------
// Existing basic controller/MMIO tests (reg stability, doorbells, etc)
// ---------------------------------------------------------------------------

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
    assert_eq!(
        c.mmio_read(regs::REG_HCCPARAMS, 4),
        c.mmio_read(regs::REG_HCCPARAMS, 4)
    );
}

#[test]
fn ehci_port_reset_timer_self_clears() {
    let mut c = EhciController::new();
    c.hub_mut().attach(0, Box::new(TestDevice));

    // Start port reset (and keep power enabled). This also clears PORT_OWNER because the port is
    // disabled and we are not setting the PORTSC.PO bit.
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
fn ehci_schedule_status_bits_track_usbcmd() {
    let mut c = EhciController::new();
    let mut mem = TestMemory::new(0x1000);

    // Schedules should not report running while the controller is halted.
    let st0 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st0 & (regs::USBSTS_ASS | regs::USBSTS_PSS), 0);

    // Run without enabling schedules.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS);
    let st1 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st1 & (regs::USBSTS_ASS | regs::USBSTS_PSS), 0);

    // Enable the async schedule.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);
    c.tick_1ms(&mut mem);
    let st2 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st2 & regs::USBSTS_ASS, 0);
    assert_eq!(st2 & regs::USBSTS_PSS, 0);

    // Enable periodic schedule as well.
    c.mmio_write(
        regs::REG_USBCMD,
        4,
        regs::USBCMD_RS | regs::USBCMD_ASE | regs::USBCMD_PSE,
    );
    c.tick_1ms(&mut mem);
    let st3 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st3 & regs::USBSTS_ASS, 0);
    assert_ne!(st3 & regs::USBSTS_PSS, 0);

    // Stop the controller: schedule status bits should clear.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_ASE | regs::USBCMD_PSE);
    c.tick_1ms(&mut mem);
    let st4 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st4 & regs::USBSTS_HCHALTED, 0);
    assert_eq!(st4 & (regs::USBSTS_ASS | regs::USBSTS_PSS), 0);
}

#[test]
fn ehci_usbsts_schedule_status_bits_are_derived_not_stored() {
    let mut c = EhciController::new();

    // Force the schedule status bits into the backing register state. Real hardware exposes these
    // bits as read-only derived state, so reads should ignore the stored values.
    c.set_usbsts_bits(regs::USBSTS_PSS | regs::USBSTS_ASS);

    // While halted, schedules are inactive regardless of what is stored in USBSTS.
    let st0 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st0 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);

    // Running without schedule enables should still read as inactive.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS);
    let st1 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st1 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);

    // Enabling periodic only should set only PSS.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);
    let st2 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st2 & regs::USBSTS_PSS, 0);
    assert_eq!(st2 & regs::USBSTS_ASS, 0);

    // Enabling both schedules should set both bits.
    c.mmio_write(
        regs::REG_USBCMD,
        4,
        regs::USBCMD_RS | regs::USBCMD_PSE | regs::USBCMD_ASE,
    );
    let st3 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st3 & regs::USBSTS_PSS, 0);
    assert_ne!(st3 & regs::USBSTS_ASS, 0);

    // Stopping the controller clears both status bits.
    c.mmio_write(regs::REG_USBCMD, 4, 0);
    let st4 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st4 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);
}

#[test]
fn ehci_hcreset_resets_operational_regs_but_preserves_port_connection() {
    let mut c = EhciController::new_with_port_count(1);
    c.hub_mut().attach(0, Box::new(TestDevice));

    let mut mem = TestMemory::new(0x1000);

    // Clear connect status change (CSC) so USBSTS.PCD can be cleared deterministically.
    write_portsc_w1c(&mut c, 0, regs::PORTSC_CSC);
    c.mmio_write(regs::REG_USBSTS, 4, regs::USBSTS_PCD); // W1C
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_PCD, 0);

    // Program some operational state.
    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, 0x1000);
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, 0x2000);
    c.mmio_write(regs::REG_FRINDEX, 4, 0x1234);
    c.mmio_write(
        regs::REG_USBINTR,
        4,
        regs::USBINTR_USBINT | regs::USBINTR_USBERRINT | regs::USBINTR_PCD,
    );
    c.mmio_write(regs::REG_CONFIGFLAG, 4, regs::CONFIGFLAG_CF);
    c.mmio_write(
        regs::REG_USBCMD,
        4,
        regs::USBCMD_RS | regs::USBCMD_ASE | regs::USBCMD_PSE,
    );
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_HCHALTED, 0);

    // Host Controller Reset should clear controller-local operational registers but not implicitly
    // detach devices from the root hub.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_HCRESET);
    c.tick_1ms(&mut mem);

    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4), 0);
    assert_eq!(c.mmio_read(regs::REG_USBINTR, 4), 0);
    assert_eq!(c.mmio_read(regs::REG_FRINDEX, 4), 0);
    assert_eq!(c.mmio_read(regs::REG_PERIODICLISTBASE, 4), 0);
    assert_eq!(c.mmio_read(regs::REG_ASYNCLISTADDR, 4), 0);
    assert_eq!(c.mmio_read(regs::REG_CONFIGFLAG, 4), 0);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(
        sts & regs::USBSTS_W1C_MASK,
        0,
        "reset should clear all W1C status bits"
    );

    let portsc = c.mmio_read(regs::reg_portsc(0), 4);
    assert_ne!(
        portsc & regs::PORTSC_CCS,
        0,
        "device should remain connected"
    );
}

#[test]
fn ehci_usbsts_schedule_status_bits_track_usbcmd() {
    let mut c = EhciController::new();

    // Initial state: halted, schedules inactive.
    let st0 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st0 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);

    // Enabling schedules while halted should not raise the status bits.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_PSE | regs::USBCMD_ASE);
    let st1 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st1 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);

    // When running, schedule enable bits should be reflected in USBSTS.
    c.mmio_write(
        regs::REG_USBCMD,
        4,
        regs::USBCMD_RS | regs::USBCMD_PSE | regs::USBCMD_ASE,
    );
    let st2 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st2 & regs::USBSTS_PSS, 0);
    assert_ne!(st2 & regs::USBSTS_ASS, 0);

    // Clearing one enable bit should clear the corresponding status bit.
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);
    let st3 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(st3 & regs::USBSTS_PSS, 0);
    assert_eq!(st3 & regs::USBSTS_ASS, 0);

    // Stopping the controller clears both status bits.
    c.mmio_write(regs::REG_USBCMD, 4, 0);
    let st4 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(st4 & (regs::USBSTS_PSS | regs::USBSTS_ASS), 0);
}

#[test]
fn ehci_frame_list_rollover_sets_flr() {
    let mut c = EhciController::new();
    let mut mem = TestMemory::new(0x1000);

    // Enable the FLR interrupt cause so we can observe irq_level transitions.
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_FLR);

    // Put FRINDEX just before rollover and start the controller.
    c.mmio_write(regs::REG_FRINDEX, 4, regs::FRINDEX_MASK - 7);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_FLR, 0);
    assert!(c.irq_level());

    // W1C should clear FLR and drop the IRQ line.
    c.mmio_write(regs::REG_USBSTS, 4, regs::USBSTS_FLR);
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_FLR, 0);
    assert!(!c.irq_level());
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

// ---------------------------------------------------------------------------
// Schedule + port behavior tests (EHCI-025)
// ---------------------------------------------------------------------------

#[test]
fn ehci_root_port_reset_enables_port_and_latches_changes() {
    let mut c = EhciController::new();
    let keyboard = UsbHidKeyboardHandle::new();
    c.hub_mut().attach(0, Box::new(keyboard.clone()));

    // Claim the port for EHCI and then reset it.
    c.mmio_write(regs::REG_CONFIGFLAG, 4, regs::CONFIGFLAG_CF);

    let st0 = c.mmio_read(regs::reg_portsc(0), 4);
    assert_ne!(st0 & regs::PORTSC_CCS, 0);
    assert_ne!(st0 & regs::PORTSC_CSC, 0);

    c.mmio_write(regs::reg_portsc(0), 4, regs::PORTSC_PP | regs::PORTSC_PR);
    let st1 = c.mmio_read(regs::reg_portsc(0), 4);
    assert_ne!(st1 & regs::PORTSC_PR, 0);

    let mut mem = TestMemory::new(0x1000);
    for _ in 0..50 {
        c.tick_1ms(&mut mem);
    }

    let st2 = c.mmio_read(regs::reg_portsc(0), 4);
    assert_eq!(st2 & regs::PORTSC_PR, 0);
    assert_ne!(st2 & regs::PORTSC_PED, 0);
    assert_ne!(st2 & regs::PORTSC_PEDC, 0);

    // W1C clear should drop change bits without powering off / disabling the port.
    write_portsc_w1c(&mut c, 0, regs::PORTSC_CSC | regs::PORTSC_PEDC);
    let st3 = c.mmio_read(regs::reg_portsc(0), 4);
    assert_eq!(st3 & (regs::PORTSC_CSC | regs::PORTSC_PEDC), 0);
    assert_ne!(st3 & regs::PORTSC_PP, 0);
    assert_ne!(st3 & regs::PORTSC_PED, 0);
}

#[test]
fn ehci_async_control_get_descriptor_device_completes_and_sets_usbint() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();
    let keyboard = UsbHidKeyboardHandle::new();
    c.hub_mut().attach(0, Box::new(keyboard.clone()));

    reset_port(&mut c, &mut mem, 0);

    // GET_DESCRIPTOR(Device), wLength=18.
    mem.write(BUF_SETUP, &[0x80, 0x06, 0x00, 0x01, 0x00, 0x00, 18, 0x00]);

    write_qtd(
        &mut mem,
        QTD_SETUP,
        QTD_DATA,
        qtd_token(QTD_TOKEN_PID_SETUP, 8, true, false),
        BUF_SETUP,
    );
    write_qtd(
        &mut mem,
        QTD_DATA,
        QTD_STATUS,
        qtd_token(QTD_TOKEN_PID_IN, 18, true, false),
        BUF_DATA,
    );
    write_qtd(
        &mut mem,
        QTD_STATUS,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, 0, true, true),
        0,
    );

    let ep_char = qh_epchar(0, 0, 64);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_SETUP,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let expected = [
        0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x34, 0x12, 0x01, 0x00, 0x00, 0x01, 0x01,
        0x02, 0x00, 0x01,
    ];
    assert_eq!(
        &mem.data[BUF_DATA as usize..BUF_DATA as usize + expected.len()],
        expected
    );

    for &qtd in &[QTD_SETUP, QTD_DATA, QTD_STATUS] {
        let token = mem.read_u32(qtd + 0x08);
        assert_eq!(token & QTD_TOKEN_ACTIVE, 0);
    }

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_USBINT, 0);
    assert!(c.irq_level());
}

#[test]
fn ehci_async_bulk_in_out_nak_then_completes() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );

    reset_port(&mut c, &mut mem, 0);

    // Async schedule head.
    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    let out_payload = [0x10, 0x20, 0x30];
    mem.write(BUF_DATA, &out_payload);

    let sentinel = [0xa5, 0xa5, 0xa5, 0xa5];
    mem.write(BUF_INT, &sentinel);

    write_qtd(
        &mut mem,
        QTD_BULK_OUT,
        QTD_BULK_IN,
        qtd_token(QTD_TOKEN_PID_OUT, out_payload.len(), true, false),
        BUF_DATA,
    );
    write_qtd(
        &mut mem,
        QTD_BULK_IN,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_IN, 3, true, true),
        BUF_INT,
    );

    // Bulk endpoint 1 at high-speed. Use 512B max packet for realism.
    let ep_char = qh_epchar(0, 1, 512);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_BULK_OUT,
    );

    // First tick: OUT should complete, IN should NAK and remain active.
    c.tick_1ms(&mut mem);

    let t0 = mem.read_u32(QTD_BULK_OUT + 0x08);
    let t1 = mem.read_u32(QTD_BULK_IN + 0x08);
    assert_eq!(t0 & QTD_TOKEN_ACTIVE, 0);
    assert_ne!(t1 & QTD_TOKEN_ACTIVE, 0);

    assert_eq!(out_received.borrow().as_slice(), &[out_payload.to_vec()]);
    assert_eq!(
        &mem.data[BUF_INT as usize..BUF_INT as usize + sentinel.len()],
        sentinel
    );
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);

    // Second tick: provide data, IN completes.
    in_queue.borrow_mut().push_back(vec![1, 2, 3]);
    c.tick_1ms(&mut mem);

    let t1 = mem.read_u32(QTD_BULK_IN + 0x08);
    assert_eq!(t1 & QTD_TOKEN_ACTIVE, 0);
    assert_eq!(&mem.data[BUF_INT as usize..BUF_INT as usize + 3], [1, 2, 3]);
    assert_ne!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);
}

#[test]
fn ehci_async_in_dma_crosses_page_boundary() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );

    reset_port(&mut c, &mut mem, 0);

    // Start 16 bytes before a 4KiB boundary so the transfer spans two pages.
    let buf0 = BUF_DATA + 0xff0;
    let buf1 = BUF_INT;

    let payload: Vec<u8> = (0..32u8).collect();
    in_queue.borrow_mut().push_back(payload.clone());

    let sentinel = [0xa5u8; 32];
    mem.write(buf0, &sentinel[..16]);
    mem.write(buf1, &sentinel[16..]);

    write_qtd(
        &mut mem,
        QTD_BULK_IN,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_IN, payload.len(), true, true),
        buf0,
    );
    // Second page pointer.
    mem.write_u32(QTD_BULK_IN + 0x10, buf1);

    let ep_char = qh_epchar(0, 1, 512);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_BULK_IN,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    assert_eq!(&mem.data[buf0 as usize..buf0 as usize + 16], &payload[..16]);
    assert_eq!(&mem.data[buf1 as usize..buf1 as usize + 16], &payload[16..]);

    let token = mem.read_u32(QTD_BULK_IN + 0x08);
    assert_eq!(token & QTD_TOKEN_ACTIVE, 0);
    assert_eq!((token >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff, 0);

    assert_ne!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);
}

#[test]
fn ehci_async_out_reads_guest_memory_across_page_boundary() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );

    reset_port(&mut c, &mut mem, 0);

    let buf0 = BUF_DATA + 0xff0;
    let buf1 = BUF_INT;

    let payload: Vec<u8> = (0..32u8).map(|v| v ^ 0x5a).collect();
    mem.write(buf0, &payload[..16]);
    mem.write(buf1, &payload[16..]);

    write_qtd(
        &mut mem,
        QTD_BULK_OUT,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, payload.len(), true, true),
        buf0,
    );
    mem.write_u32(QTD_BULK_OUT + 0x10, buf1);

    let ep_char = qh_epchar(0, 1, 512);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_BULK_OUT,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().as_slice(), &[payload.clone()]);

    let token = mem.read_u32(QTD_BULK_OUT + 0x08);
    assert_eq!(token & QTD_TOKEN_ACTIVE, 0);
    assert_eq!((token >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff, 0);

    assert_ne!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);
}

#[test]
fn ehci_async_out_allows_full_5_page_qtd() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );

    reset_port(&mut c, &mut mem, 0);

    let base: u32 = 0x8000;
    let len: usize = 5 * 4096;
    let payload: Vec<u8> = (0..len).map(|i| (i & 0xff) as u8).collect();
    mem.write(base, &payload);

    write_qtd(
        &mut mem,
        QTD_BULK_OUT,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, len, true, true),
        base,
    );
    // Provide all five page pointers so the transfer can span the full 5-page qTD capacity.
    mem.write_u32(QTD_BULK_OUT + 0x10, base + 0x1000);
    mem.write_u32(QTD_BULK_OUT + 0x14, base + 0x2000);
    mem.write_u32(QTD_BULK_OUT + 0x18, base + 0x3000);
    mem.write_u32(QTD_BULK_OUT + 0x1c, base + 0x4000);

    let ep_char = qh_epchar(0, 1, 512);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_BULK_OUT,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let token = mem.read_u32(QTD_BULK_OUT + 0x08);
    assert_eq!(token & QTD_TOKEN_ACTIVE, 0);
    assert_eq!((token >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff, 0);
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBERRINT, 0);
    assert_ne!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);

    let mut received = Vec::with_capacity(len);
    for chunk in out_received.borrow().iter() {
        received.extend_from_slice(chunk);
    }
    assert_eq!(received, payload);
}

#[test]
fn ehci_async_in_allows_full_5_page_qtd() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );

    reset_port(&mut c, &mut mem, 0);

    let base: u32 = 0x8000;
    let len: usize = 5 * 4096;
    let payload: Vec<u8> = (0..len).map(|i| (i as u8).wrapping_mul(3)).collect();
    for chunk in payload.chunks(512) {
        in_queue.borrow_mut().push_back(chunk.to_vec());
    }

    let sentinel = 0xa5u8;
    mem.write(base, &vec![sentinel; len]);

    write_qtd(
        &mut mem,
        QTD_BULK_IN,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_IN, len, true, true),
        base,
    );
    mem.write_u32(QTD_BULK_IN + 0x10, base + 0x1000);
    mem.write_u32(QTD_BULK_IN + 0x14, base + 0x2000);
    mem.write_u32(QTD_BULK_IN + 0x18, base + 0x3000);
    mem.write_u32(QTD_BULK_IN + 0x1c, base + 0x4000);

    let ep_char = qh_epchar(0, 1, 512);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_BULK_IN,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    assert!(out_received.borrow().is_empty());

    let token = mem.read_u32(QTD_BULK_IN + 0x08);
    assert_eq!(token & QTD_TOKEN_ACTIVE, 0);
    assert_eq!((token >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff, 0);
    assert_eq!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBERRINT, 0);
    assert_ne!(c.mmio_read(regs::REG_USBSTS, 4) & regs::USBSTS_USBINT, 0);

    assert_eq!(&mem.data[base as usize..base as usize + len], payload);
}

// ---------------------------------------------------------------------------
// EHCI-026: schedule robustness (loop/cycle bounds)
// ---------------------------------------------------------------------------

#[test]
fn ehci_async_qh_non_head_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let head_qh: u32 = ASYNC_QH;
    let qh1: u32 = 0x1200;

    // The async schedule is a circular list, so a single head QH pointing to itself is valid.
    // Instead, construct: head -> qh1, qh1 -> qh1 (a non-head self-loop).
    mem.write_u32(head_qh + 0x00, qh_link_ptr_qh(qh1));
    mem.write_u32(qh1 + 0x00, qh_link_ptr_qh(qh1));

    // Terminate qTD chains so QH processing is effectively a no-op.
    mem.write_u32(head_qh + 0x10, LINK_TERMINATE);
    mem.write_u32(qh1 + 0x10, LINK_TERMINATE);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, head_qh);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_async_qtd_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();
    c.hub_mut().attach(0, Box::new(TestDevice));

    reset_port(&mut c, &mut mem, 0);

    // qTD next points to itself (cycle). Token is inactive and error-free so the controller tries
    // to advance the qTD pointer.
    write_qtd(
        &mut mem,
        QTD_SETUP,
        QTD_SETUP,
        qtd_token(QTD_TOKEN_PID_OUT, 0, false, false),
        0,
    );

    let ep_char = qh_epchar(0, 0, 64);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        QTD_SETUP,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_qh_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let fl_base: u32 = 0x7000;
    let qh: u32 = 0x1800;

    // Frame list entry 0 points at a QH with a self-referential horizontal link.
    mem.write_u32(fl_base, qh_link_ptr_qh(qh));
    mem.write_u32(qh + 0x00, qh_link_ptr_qh(qh));
    mem.write_u32(qh + 0x10, LINK_TERMINATE);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_itd_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let fl_base: u32 = 0x7000;
    let itd: u32 = 0x1800;

    // Frame list entry 0 points at an iTD (type=0). The iTD's forward pointer (dword0) points back
    // to itself, which would loop forever without periodic traversal cycle detection.
    mem.write_u32(fl_base, itd & LINK_ADDR_MASK);
    mem.write_u32(itd + 0x00, itd & LINK_ADDR_MASK);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_qtd_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );
    reset_port(&mut c, &mut mem, 0);

    let fl_base: u32 = 0x7000;
    let qh: u32 = 0x1800;
    let qtd: u32 = 0x2000;

    // Frame list entry 0 points at a QH. The QH's qTD next pointer points at a qTD whose next
    // pointer loops back to itself.
    mem.write_u32(fl_base, qh_link_ptr_qh(qh));
    write_qtd(
        &mut mem,
        qtd,
        qtd,
        qtd_token(QTD_TOKEN_PID_OUT, 0, true, false),
        0,
    );
    let ep_char = qh_epchar(0, 1, 64);
    write_qh(&mut mem, qh, LINK_TERMINATE, ep_char, qtd);

    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_sitd_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let fl_base: u32 = 0x7000;
    let sitd: u32 = 0x1800;

    // Frame list entry 0 points at a siTD (type=2). The siTD's next link pointer (dword0) points
    // back to itself.
    mem.write_u32(fl_base, (sitd & LINK_ADDR_MASK) | LINK_TYPE_SITD);
    mem.write_u32(sitd + 0x00, (sitd & LINK_ADDR_MASK) | LINK_TYPE_SITD);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_fstn_self_loop_sets_hse_and_halts() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let fl_base: u32 = 0x7000;
    let fstn: u32 = 0x1800;

    // Frame list entry 0 points at an FSTN (type=3). The FSTN's Normal Path Link Pointer (dword0)
    // points back to itself.
    mem.write_u32(fl_base, (fstn & LINK_ADDR_MASK) | LINK_TYPE_FSTN);
    mem.write_u32(fstn + 0x00, (fstn & LINK_ADDR_MASK) | LINK_TYPE_FSTN);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_itd_forward_link_to_qh_executes_qh() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );
    reset_port(&mut c, &mut mem, 0);

    let fl_base: u32 = 0x7000;
    let itd: u32 = 0x1800;
    let qh: u32 = 0x1a00;
    let qtd: u32 = 0x2000;

    let payload = [0x10, 0x20, 0x30];
    mem.write(BUF_DATA, &payload);

    // Frame list entry 0 points at an iTD whose Next Link Pointer points at a QH.
    mem.write_u32(fl_base, itd & LINK_ADDR_MASK);
    mem.write_u32(itd + 0x00, qh_link_ptr_qh(qh));

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, payload.len(), true, true),
        BUF_DATA,
    );
    let ep_char = qh_epchar(0, 1, 64);
    write_qh(&mut mem, qh, LINK_TERMINATE, ep_char, qtd);

    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().len(), 1);
    assert_eq!(out_received.borrow()[0], payload);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_USBINT, 0);
    assert!(c.irq_level());
}

#[test]
fn ehci_periodic_sitd_forward_link_to_qh_executes_qh() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );
    reset_port(&mut c, &mut mem, 0);

    let fl_base: u32 = 0x7000;
    let sitd: u32 = 0x1800;
    let qh: u32 = 0x1a00;
    let qtd: u32 = 0x2000;

    let payload = [0x10, 0x20, 0x30];
    mem.write(BUF_DATA, &payload);

    // Frame list entry 0 points at a siTD whose Next Link Pointer points at a QH.
    mem.write_u32(fl_base, (sitd & LINK_ADDR_MASK) | LINK_TYPE_SITD);
    mem.write_u32(sitd + 0x00, qh_link_ptr_qh(qh));

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, payload.len(), true, true),
        BUF_DATA,
    );
    let ep_char = qh_epchar(0, 1, 64);
    write_qh(&mut mem, qh, LINK_TERMINATE, ep_char, qtd);

    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().len(), 1);
    assert_eq!(out_received.borrow()[0], payload);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_USBINT, 0);
    assert!(c.irq_level());
}

#[test]
fn ehci_periodic_fstn_forward_link_to_qh_executes_qh() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let in_queue = Rc::new(RefCell::new(VecDeque::new()));
    let out_received = Rc::new(RefCell::new(Vec::new()));
    c.hub_mut().attach(
        0,
        Box::new(BulkEndpointDevice::new(
            in_queue.clone(),
            out_received.clone(),
        )),
    );
    reset_port(&mut c, &mut mem, 0);

    let fl_base: u32 = 0x7000;
    let fstn: u32 = 0x1800;
    let qh: u32 = 0x1a00;
    let qtd: u32 = 0x2000;

    let payload = [0x10, 0x20, 0x30];
    mem.write(BUF_DATA, &payload);

    // Frame list entry 0 points at an FSTN whose Normal Path Link Pointer points at a QH.
    mem.write_u32(fl_base, (fstn & LINK_ADDR_MASK) | LINK_TYPE_FSTN);
    mem.write_u32(fstn + 0x00, qh_link_ptr_qh(qh));

    write_qtd(
        &mut mem,
        qtd,
        LINK_TERMINATE,
        qtd_token(QTD_TOKEN_PID_OUT, payload.len(), true, true),
        BUF_DATA,
    );
    let ep_char = qh_epchar(0, 1, 64);
    write_qh(&mut mem, qh, LINK_TERMINATE, ep_char, qtd);

    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    assert_eq!(out_received.borrow().len(), 1);
    assert_eq!(out_received.borrow()[0], payload);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_USBINT, 0);
    assert!(c.irq_level());
}

#[test]
fn ehci_async_qh_budget_exceeded_sets_hse_and_halts() {
    const QH_COUNT: usize = 16 * 1024;
    let mut mem = TestMemory::new(0x100000);

    let head: u32 = 0x1000;

    // Construct an overlong async QH ring. Each QH is otherwise inert (no qTD chain) so the walker
    // exercises only the traversal budget logic.
    for i in 0..QH_COUNT {
        let qh = head + (i as u32) * 0x20;
        let next = if i + 1 == QH_COUNT { head } else { qh + 0x20 };
        mem.write_u32(qh + 0x00, qh_link_ptr_qh(next));
        mem.write_u32(qh + 0x04, 0); // speed invalid; QH processing is a no-op.
        mem.write_u32(qh + 0x10, LINK_TERMINATE); // no qTDs
    }

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, head);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_async_qtd_budget_exceeded_sets_hse_and_halts() {
    const QTD_COUNT: usize = 16 * 1024;
    let mut mem = TestMemory::new(0x100000);
    let mut c = EhciController::new();
    c.hub_mut().attach(0, Box::new(TestDevice));

    reset_port(&mut c, &mut mem, 0);

    // Build a long chain of already-inactive qTDs. EHCI will quickly advance through these without
    // invoking any device logic, exercising the per-QH qTD step budget.
    let first_qtd: u32 = 0x2000;
    for i in 0..QTD_COUNT {
        let addr = first_qtd + (i as u32) * 0x20;
        let next = if i + 1 == QTD_COUNT {
            LINK_TERMINATE
        } else {
            addr + 0x20
        };
        write_qtd(
            &mut mem,
            addr,
            next,
            qtd_token(QTD_TOKEN_PID_OUT, 0, false, false),
            0,
        );
    }

    let ep_char = qh_epchar(0, 0, 64);
    write_qh(
        &mut mem,
        ASYNC_QH,
        qh_link_ptr_qh(ASYNC_QH),
        ep_char,
        first_qtd,
    );

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_periodic_link_budget_exceeded_sets_hse_and_halts() {
    const LINK_COUNT: usize = 64 * 1024;
    let mut mem = TestMemory::new(0x220000);

    // Keep the frame list separate from the (large) schedule node chain.
    let fl_base: u32 = 0x210000;
    let first: u32 = 0x1000;

    mem.write_u32(fl_base, first & LINK_ADDR_MASK);
    for i in 0..LINK_COUNT {
        let addr = first + (i as u32) * 0x20;
        let next = if i + 1 == LINK_COUNT {
            LINK_TERMINATE
        } else {
            addr + 0x20
        };
        // iTD dword0 is the Next Link Pointer; use iTD type (00).
        mem.write_u32(addr + 0x00, next);
    }

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_HCHALTED, 0);
    assert_eq!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);
}

#[test]
fn ehci_async_qtd_packet_budget_yields_without_faulting() {
    let mut mem = TestMemory::new(MEM_SIZE);
    let mut c = EhciController::new();

    let bytes_written = Rc::new(Cell::new(0usize));
    c.hub_mut()
        .attach(0, Box::new(CountingOutDevice::new(bytes_written.clone())));
    reset_port(&mut c, &mut mem, 0);

    // Construct a single active OUT qTD with enough bytes to exceed the per-tick packet budget
    // (4096 packets). Using max_packet=1 keeps the buffer size minimal while still forcing the
    // budget path.
    let total_bytes = 4097usize;
    let qtd = QTD_BULK_OUT;
    mem.write_u32(qtd + 0x00, LINK_TERMINATE); // next terminate
    mem.write_u32(qtd + 0x04, LINK_TERMINATE); // alt-next terminate
    mem.write_u32(
        qtd + 0x08,
        qtd_token(QTD_TOKEN_PID_OUT, total_bytes, true, false),
    );
    mem.write_u32(qtd + 0x0c, BUF_DATA);
    mem.write_u32(qtd + 0x10, BUF_INT); // second page
    mem.write_u32(qtd + 0x14, 0);
    mem.write_u32(qtd + 0x18, 0);
    mem.write_u32(qtd + 0x1c, 0);

    let ep_char = qh_epchar(0, 1, 1);
    write_qh(&mut mem, ASYNC_QH, qh_link_ptr_qh(ASYNC_QH), ep_char, qtd);

    c.mmio_write(regs::REG_ASYNCLISTADDR, 4, ASYNC_QH);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_ASE);

    c.tick_1ms(&mut mem);

    // One tick should process at most 4096 packets and then yield (without faulting/halting).
    assert_eq!(bytes_written.get(), 4096);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts & regs::USBSTS_HSE, 0);
    assert_eq!(sts & regs::USBSTS_HCHALTED, 0);
    assert_ne!(c.mmio_read(regs::REG_USBCMD, 4) & regs::USBCMD_RS, 0);

    // The qTD token in guest memory should not be rewritten on the NAK/budget-yield path; only the
    // QH overlay token is updated.
    let qtd_token_after = mem.read_u32(qtd + 0x08);
    assert_ne!(qtd_token_after & QTD_TOKEN_ACTIVE, 0);
    assert_eq!(
        ((qtd_token_after >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff) as usize,
        total_bytes
    );
    let overlay_token = mem.read_u32(ASYNC_QH + 0x18);
    assert_ne!(overlay_token & QTD_TOKEN_ACTIVE, 0);
    assert_eq!(
        ((overlay_token >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff) as usize,
        1
    );

    // Next tick should complete the final byte and clear Active in the qTD token.
    c.tick_1ms(&mut mem);
    assert_eq!(bytes_written.get(), total_bytes);
    let qtd_token_done = mem.read_u32(qtd + 0x08);
    assert_eq!(qtd_token_done & QTD_TOKEN_ACTIVE, 0);
    assert_eq!(
        ((qtd_token_done >> QTD_TOKEN_TOTAL_BYTES_SHIFT) & 0x7fff) as usize,
        0
    );
}

#[test]
fn ehci_schedule_fault_raises_irq_when_enabled_and_clears_on_w1c() {
    let mut mem = TestMemory::new(MEM_SIZE);

    let fl_base: u32 = 0x7000;
    let qh: u32 = 0x1800;

    // Frame list entry 0 points at a QH with a self-referential horizontal link.
    mem.write_u32(fl_base, qh_link_ptr_qh(qh));
    mem.write_u32(qh + 0x00, qh_link_ptr_qh(qh));
    mem.write_u32(qh + 0x10, LINK_TERMINATE);

    let mut c = EhciController::new();
    c.mmio_write(regs::REG_PERIODICLISTBASE, 4, fl_base);
    c.mmio_write(regs::REG_USBINTR, 4, regs::USBINTR_USBERRINT);
    c.mmio_write(regs::REG_USBCMD, 4, regs::USBCMD_RS | regs::USBCMD_PSE);

    c.tick_1ms(&mut mem);

    let sts = c.mmio_read(regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert_ne!(sts & regs::USBSTS_USBERRINT, 0);
    assert_eq!(sts & regs::USBSTS_USBINT, 0);
    assert!(
        c.irq_level(),
        "expected IRQ to assert when USBERRINT is enabled"
    );

    // USBSTS is write-1-to-clear; acknowledging the error should also drop the IRQ.
    c.mmio_write(
        regs::REG_USBSTS,
        4,
        regs::USBSTS_HSE | regs::USBSTS_USBERRINT,
    );
    let sts2 = c.mmio_read(regs::REG_USBSTS, 4);
    assert_eq!(sts2 & (regs::USBSTS_HSE | regs::USBSTS_USBERRINT), 0);
    assert!(!c.irq_level(), "expected IRQ to deassert after W1C ack");
}
