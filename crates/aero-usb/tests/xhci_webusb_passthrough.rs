//! xHCI + WebUSB passthrough integration test.
//!
//! `UsbWebUsbPassthroughDevice` / `UsbPassthroughDevice` model asynchronous host I/O by queueing a
//! `UsbHostAction` and returning `NAK` until the host pushes a matching `UsbHostCompletion`.
//!
//! This test drives that device model through a minimal **xHCI-style** flow using the canonical
//! xHCI TRB/ring helpers in `aero_usb::xhci`:
//! - command ring setup (Enable Slot → Address Device → Configure Endpoint),
//! - an EP0 control-IN transfer (GET_DESCRIPTOR) built from Setup/Data/Status stage TRBs,
//! - retries while the host completion is pending (without duplicating actions),
//! - completion handling + guest-memory DMA of the returned data.
//!
//! The crate does not yet have a full xHCI MMIO controller model; this is a logical transfer-engine
//! harness that consumes TRBs from guest memory and drives the existing `AttachedUsbDevice` control
//! pipe state machine.

use std::collections::VecDeque;

use aero_usb::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbHostCompletionOut,
};
use aero_usb::xhci::ring::{RingCursor, RingPoll};
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::{SetupPacket, UsbWebUsbPassthroughDevice};

mod util;

use util::{Alloc, TestMemory};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionKind {
    ControlIn,
    BulkOut,
    BulkIn,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Completion {
    kind: CompletionKind,
    bytes_transferred: usize,
}

#[derive(Debug)]
struct Ep0PendingControlIn {
    buf_ptr: u64,
    len: usize,
    next_cursor: RingCursor,
}

#[derive(Debug)]
struct BulkOutPending {
    ep: u8,
    data: Vec<u8>,
    next_cursor: RingCursor,
}

#[derive(Debug)]
struct BulkInPending {
    ep: u8,
    buf_ptr: u64,
    len: usize,
    next_cursor: RingCursor,
}

/// A minimal xHCI-style command + transfer engine harness.
///
/// This intentionally focuses on:
/// - consuming TRBs from guest memory via `RingCursor`,
/// - driving the existing `AttachedUsbDevice` transaction helpers, and
/// - verifying correct interaction with the async WebUSB passthrough model.
struct XhciHarness {
    dev: AttachedUsbDevice,
    slot_id: Option<u8>,
    endpoints_configured: bool,
    ep0_pending: Option<Ep0PendingControlIn>,
    bulk_out_pending: Option<BulkOutPending>,
    bulk_in_pending: Option<BulkInPending>,
    completions: VecDeque<Completion>,
}

impl XhciHarness {
    fn new(dev: AttachedUsbDevice) -> Self {
        Self {
            dev,
            slot_id: None,
            endpoints_configured: false,
            ep0_pending: None,
            bulk_out_pending: None,
            bulk_in_pending: None,
            completions: VecDeque::new(),
        }
    }

    fn pop_completion(&mut self) -> Option<Completion> {
        self.completions.pop_front()
    }

    fn process_command_ring(&mut self, mem: &mut TestMemory, cursor: &mut RingCursor) {
        loop {
            let item = match cursor.poll(mem, 64) {
                RingPoll::Ready(item) => item,
                RingPoll::NotReady => break,
                RingPoll::Err(err) => panic!("command ring error: {err:?}"),
            };

            match item.trb.trb_type() {
                TrbType::EnableSlotCommand => {
                    assert!(
                        self.slot_id.is_none(),
                        "harness supports a single slot; Enable Slot issued twice"
                    );
                    self.slot_id = Some(1);
                }
                TrbType::AddressDeviceCommand => {
                    let slot_id = item.trb.slot_id();
                    assert_eq!(
                        Some(slot_id),
                        self.slot_id,
                        "Address Device should reference the enabled slot"
                    );

                    // xHCI Address Device performs SET_ADDRESS on EP0.
                    let set_address = SetupPacket {
                        bm_request_type: 0x00, // HostToDevice | Standard | Device
                        b_request: 0x05,       // SET_ADDRESS
                        w_value: slot_id as u16,
                        w_index: 0,
                        w_length: 0,
                    };
                    assert_eq!(self.dev.handle_setup(set_address), UsbOutResult::Ack);
                    assert_eq!(self.dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
                    assert_eq!(self.dev.address(), slot_id);
                }
                TrbType::ConfigureEndpointCommand => {
                    let slot_id = item.trb.slot_id();
                    assert_eq!(
                        Some(slot_id),
                        self.slot_id,
                        "Configure Endpoint should reference the enabled slot"
                    );
                    self.endpoints_configured = true;
                }
                TrbType::NoOpCommand => {}
                other => panic!("unexpected command TRB type: {other:?}"),
            }
        }
    }

    fn tick_ep0_control_in(&mut self, mem: &mut TestMemory, cursor: &mut RingCursor) {
        if self.ep0_pending.is_none() {
            // xHCI does not advance the transfer ring dequeue pointer until the TD completes.
            // Read the TRBs using a temporary cursor and only commit cursor advancement once the
            // transfer has finished.
            let mut probe = *cursor;

            let setup_trb = match probe.poll(mem, 64) {
                RingPoll::Ready(item) => item.trb,
                RingPoll::NotReady => return,
                RingPoll::Err(err) => panic!("ep0 ring error: {err:?}"),
            };
            assert_eq!(
                setup_trb.trb_type(),
                TrbType::SetupStage,
                "expected Setup Stage TRB"
            );
            let setup = SetupPacket::from_bytes(setup_trb.parameter.to_le_bytes());

            // Ensure enumeration ran first (Address Device).
            assert!(
                self.dev.address() != 0,
                "device must be addressed before xHCI transfers are issued"
            );

            assert_eq!(self.dev.handle_setup(setup), UsbOutResult::Ack);

            let data_trb = match probe.poll(mem, 64) {
                RingPoll::Ready(item) => item.trb,
                RingPoll::NotReady => panic!("missing Data Stage TRB"),
                RingPoll::Err(err) => panic!("ep0 ring error: {err:?}"),
            };
            assert_eq!(
                data_trb.trb_type(),
                TrbType::DataStage,
                "expected Data Stage TRB"
            );
            let status_trb = match probe.poll(mem, 64) {
                RingPoll::Ready(item) => item.trb,
                RingPoll::NotReady => panic!("missing Status Stage TRB"),
                RingPoll::Err(err) => panic!("ep0 ring error: {err:?}"),
            };
            assert_eq!(
                status_trb.trb_type(),
                TrbType::StatusStage,
                "expected Status Stage TRB"
            );

            self.ep0_pending = Some(Ep0PendingControlIn {
                buf_ptr: data_trb.parameter,
                len: data_trb.status as usize,
                next_cursor: probe,
            });
        }

        let pending = self.ep0_pending.as_ref().expect("pending must exist");
        match self.dev.handle_in(0, pending.len) {
            UsbInResult::Nak => {}
            UsbInResult::Data(data) => {
                mem.write(pending.buf_ptr as u32, &data);
                // STATUS stage: for control-IN, the status stage is an OUT ZLP.
                assert_eq!(self.dev.handle_out(0, &[]), UsbOutResult::Ack);
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::ControlIn,
                    bytes_transferred: data.len(),
                });
                self.ep0_pending = None;
            }
            UsbInResult::Stall | UsbInResult::Timeout => {
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::ControlIn,
                    bytes_transferred: 0,
                });
                self.ep0_pending = None;
            }
        }
    }

    fn tick_bulk_out(&mut self, mem: &mut TestMemory, cursor: &mut RingCursor, ep: u8) {
        assert!(
            self.endpoints_configured,
            "bulk transfers require Configure Endpoint first"
        );

        if self.bulk_out_pending.is_none() {
            // Like the EP0 helper above, keep the dequeue pointer stable until the TD completes.
            let mut probe = *cursor;
            let trb = match probe.poll(mem, 64) {
                RingPoll::Ready(item) => item.trb,
                RingPoll::NotReady => return,
                RingPoll::Err(err) => panic!("bulk out ring error: {err:?}"),
            };
            assert_eq!(trb.trb_type(), TrbType::Normal, "expected Normal TRB");

            let len = trb.status as usize;
            let mut data = vec![0u8; len];
            mem.read(trb.parameter as u32, &mut data);
            self.bulk_out_pending = Some(BulkOutPending {
                ep,
                data,
                next_cursor: probe,
            });
        }

        let pending = self
            .bulk_out_pending
            .as_ref()
            .expect("pending bulk out must exist");
        match self.dev.handle_out(pending.ep, &pending.data) {
            UsbOutResult::Nak => {}
            UsbOutResult::Ack => {
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::BulkOut,
                    bytes_transferred: pending.data.len(),
                });
                self.bulk_out_pending = None;
            }
            UsbOutResult::Stall | UsbOutResult::Timeout => {
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::BulkOut,
                    bytes_transferred: 0,
                });
                self.bulk_out_pending = None;
            }
        }
    }

    fn tick_bulk_in(&mut self, mem: &mut TestMemory, cursor: &mut RingCursor, ep: u8) {
        assert!(
            self.endpoints_configured,
            "bulk transfers require Configure Endpoint first"
        );

        if self.bulk_in_pending.is_none() {
            let mut probe = *cursor;
            let trb = match probe.poll(mem, 64) {
                RingPoll::Ready(item) => item.trb,
                RingPoll::NotReady => return,
                RingPoll::Err(err) => panic!("bulk in ring error: {err:?}"),
            };
            assert_eq!(trb.trb_type(), TrbType::Normal, "expected Normal TRB");
            self.bulk_in_pending = Some(BulkInPending {
                ep,
                buf_ptr: trb.parameter,
                len: trb.status as usize,
                next_cursor: probe,
            });
        }

        let pending = self
            .bulk_in_pending
            .as_ref()
            .expect("pending bulk in must exist");
        match self.dev.handle_in(pending.ep, pending.len) {
            UsbInResult::Nak => {}
            UsbInResult::Data(data) => {
                mem.write(pending.buf_ptr as u32, &data);
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::BulkIn,
                    bytes_transferred: data.len(),
                });
                self.bulk_in_pending = None;
            }
            UsbInResult::Stall | UsbInResult::Timeout => {
                *cursor = pending.next_cursor;
                self.completions.push_back(Completion {
                    kind: CompletionKind::BulkIn,
                    bytes_transferred: 0,
                });
                self.bulk_in_pending = None;
            }
        }
    }
}

#[test]
fn xhci_control_in_get_descriptor_queues_action_then_completes_and_dmas_data() {
    let dev = UsbWebUsbPassthroughDevice::new();
    let attached = AttachedUsbDevice::new(Box::new(dev.clone()));
    let mut xhci = XhciHarness::new(attached);

    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x1000);

    // Command ring: Enable Slot → Address Device → Configure Endpoint.
    let cmd_ring = alloc.alloc((TRB_LEN * 3) as u32, 0x10) as u64;

    let mut enable_slot = Trb::default();
    enable_slot.set_cycle(true);
    enable_slot.set_trb_type(TrbType::EnableSlotCommand);
    enable_slot.write_to(&mut mem, cmd_ring);

    let mut address_device = Trb::default();
    address_device.set_cycle(true);
    address_device.set_trb_type(TrbType::AddressDeviceCommand);
    address_device.set_slot_id(1);
    address_device.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    let mut configure_ep = Trb::default();
    configure_ep.set_cycle(true);
    configure_ep.set_trb_type(TrbType::ConfigureEndpointCommand);
    configure_ep.set_slot_id(1);
    configure_ep.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    let mut cmd_cursor = RingCursor::new(cmd_ring, true);
    xhci.process_command_ring(&mut mem, &mut cmd_cursor);

    assert_eq!(xhci.slot_id, Some(1));
    assert!(xhci.endpoints_configured);
    assert_eq!(xhci.dev.address(), 1);

    // EP0 control-IN GET_DESCRIPTOR via Setup/Data/Status stage TRBs.
    let setup = SetupPacket {
        bm_request_type: 0x80, // DeviceToHost | Standard | Device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // DEVICE descriptor, index 0
        w_index: 0,
        w_length: 18,
    };

    let ep0_ring = alloc.alloc((TRB_LEN * 3) as u32, 0x10) as u64;
    let dma_buf = alloc.alloc(64, 0x10) as u64;

    let mut setup_trb = Trb::new(u64::from_le_bytes(setup_packet_bytes(setup)), 0, 0);
    setup_trb.set_cycle(true);
    setup_trb.set_trb_type(TrbType::SetupStage);
    setup_trb.write_to(&mut mem, ep0_ring);

    let mut data_trb = Trb::new(dma_buf, setup.w_length as u32, Trb::CONTROL_DIR);
    data_trb.set_cycle(true);
    data_trb.set_trb_type(TrbType::DataStage);
    data_trb.write_to(&mut mem, ep0_ring + TRB_LEN as u64);

    let mut status_trb = Trb::default();
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    status_trb.write_to(&mut mem, ep0_ring + 2 * TRB_LEN as u64);

    let mut ep0_cursor = RingCursor::new(ep0_ring, true);

    // Tick #1: SETUP is executed, DATA stage NAKs (pending), and the passthrough model queues a
    // single host action.
    xhci.tick_ep0_control_in(&mut mem, &mut ep0_cursor);
    assert_eq!(
        ep0_cursor.dequeue_ptr(),
        ep0_ring,
        "xHCI should not advance the dequeue pointer while a TD is NAK/pending"
    );

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1, "expected exactly one queued host action");
    let action = actions.pop().unwrap();

    let (id, got_setup) = match action {
        UsbHostAction::ControlIn { id, setup } => (id, setup),
        other => panic!("unexpected action: {other:?}"),
    };

    assert_eq!(
        got_setup,
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    );

    assert!(
        xhci.pop_completion().is_none(),
        "transfer should still be pending without a host completion"
    );

    // Tick again without a completion: the DATA stage should keep NAKing and must not emit a
    // duplicate host action.
    xhci.tick_ep0_control_in(&mut mem, &mut ep0_cursor);
    assert_eq!(ep0_cursor.dequeue_ptr(), ep0_ring);
    assert!(
        dev.drain_actions().is_empty(),
        "in-flight control transfers must not queue duplicate host actions"
    );
    assert!(
        xhci.pop_completion().is_none(),
        "transfer should still be pending without a host completion"
    );

    // Inject completion with deterministic payload.
    let payload: Vec<u8> = (0u8..18u8).collect();
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    // Tick #2: DATA stage completes and DMA writes into guest memory; STATUS stage completes.
    xhci.tick_ep0_control_in(&mut mem, &mut ep0_cursor);
    assert_eq!(
        ep0_cursor.dequeue_ptr(),
        ep0_ring + (3 * TRB_LEN) as u64,
        "completion should advance the dequeue pointer past the control TD"
    );

    assert_eq!(
        xhci.pop_completion(),
        Some(Completion {
            kind: CompletionKind::ControlIn,
            bytes_transferred: payload.len(),
        })
    );

    let mut got = vec![0u8; payload.len()];
    mem.read(dma_buf as u32, &mut got);
    assert_eq!(got, payload);

    assert!(
        dev.drain_actions().is_empty(),
        "no duplicate host actions should be emitted across retries"
    );
}

#[test]
fn xhci_bulk_in_out_normal_trb_queues_actions_and_consumes_completions() {
    let dev = UsbWebUsbPassthroughDevice::new();
    let attached = AttachedUsbDevice::new(Box::new(dev.clone()));
    let mut xhci = XhciHarness::new(attached);

    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    // Enumeration command ring.
    let cmd_ring = alloc.alloc((TRB_LEN * 3) as u32, 0x10) as u64;

    let mut enable_slot = Trb::default();
    enable_slot.set_cycle(true);
    enable_slot.set_trb_type(TrbType::EnableSlotCommand);
    enable_slot.write_to(&mut mem, cmd_ring);

    let mut address_device = Trb::default();
    address_device.set_cycle(true);
    address_device.set_trb_type(TrbType::AddressDeviceCommand);
    address_device.set_slot_id(1);
    address_device.write_to(&mut mem, cmd_ring + TRB_LEN as u64);

    let mut configure_ep = Trb::default();
    configure_ep.set_cycle(true);
    configure_ep.set_trb_type(TrbType::ConfigureEndpointCommand);
    configure_ep.set_slot_id(1);
    configure_ep.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);

    let mut cmd_cursor = RingCursor::new(cmd_ring, true);
    xhci.process_command_ring(&mut mem, &mut cmd_cursor);

    // Bulk OUT Normal TRB (endpoint 1 OUT).
    let out_payload = [0xAAu8, 0xBB, 0xCC, 0xDD];
    let out_buf = alloc.alloc(out_payload.len() as u32, 0x10) as u64;
    mem.write(out_buf as u32, &out_payload);

    let bulk_out_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let mut bulk_out_trb = Trb::new(out_buf, out_payload.len() as u32, 0);
    bulk_out_trb.set_cycle(true);
    bulk_out_trb.set_trb_type(TrbType::Normal);
    bulk_out_trb.write_to(&mut mem, bulk_out_ring);

    let mut bulk_out_cursor = RingCursor::new(bulk_out_ring, true);

    xhci.tick_bulk_out(&mut mem, &mut bulk_out_cursor, 1);
    assert_eq!(
        bulk_out_cursor.dequeue_ptr(),
        bulk_out_ring,
        "bulk OUT NAK should leave the TRB pending"
    );

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (out_id, out_endpoint, out_data) = match actions.pop().unwrap() {
        UsbHostAction::BulkOut { id, endpoint, data } => (id, endpoint, data),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(out_endpoint, 0x01);
    assert_eq!(out_data, out_payload);

    // Retry without completion should not emit a duplicate host action.
    xhci.tick_bulk_out(&mut mem, &mut bulk_out_cursor, 1);
    assert_eq!(bulk_out_cursor.dequeue_ptr(), bulk_out_ring);
    assert!(
        dev.drain_actions().is_empty(),
        "in-flight bulk OUT must not queue duplicate host actions"
    );
    assert!(xhci.pop_completion().is_none());

    dev.push_completion(UsbHostCompletion::BulkOut {
        id: out_id,
        result: UsbHostCompletionOut::Success {
            bytes_written: out_payload.len() as u32,
        },
    });
    xhci.tick_bulk_out(&mut mem, &mut bulk_out_cursor, 1);
    assert_eq!(
        bulk_out_cursor.dequeue_ptr(),
        bulk_out_ring + TRB_LEN as u64,
        "bulk OUT completion should advance the dequeue pointer"
    );
    assert_eq!(
        xhci.pop_completion(),
        Some(Completion {
            kind: CompletionKind::BulkOut,
            bytes_transferred: out_payload.len(),
        })
    );

    // Bulk IN Normal TRB (endpoint 1 IN).
    let in_payload = [1u8, 2, 3, 4, 5];
    let in_buf = alloc.alloc(in_payload.len() as u32, 0x10) as u64;

    let bulk_in_ring = alloc.alloc(TRB_LEN as u32, 0x10) as u64;
    let mut bulk_in_trb = Trb::new(in_buf, in_payload.len() as u32, 0);
    bulk_in_trb.set_cycle(true);
    bulk_in_trb.set_trb_type(TrbType::Normal);
    bulk_in_trb.write_to(&mut mem, bulk_in_ring);

    let mut bulk_in_cursor = RingCursor::new(bulk_in_ring, true);
    xhci.tick_bulk_in(&mut mem, &mut bulk_in_cursor, 1);
    assert_eq!(
        bulk_in_cursor.dequeue_ptr(),
        bulk_in_ring,
        "bulk IN NAK should leave the TRB pending"
    );

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (in_id, in_endpoint, in_len) = match actions.pop().unwrap() {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => (id, endpoint, length),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(in_endpoint, 0x81);
    assert_eq!(in_len as usize, in_payload.len());

    // Retry without completion should not emit a duplicate host action.
    xhci.tick_bulk_in(&mut mem, &mut bulk_in_cursor, 1);
    assert_eq!(bulk_in_cursor.dequeue_ptr(), bulk_in_ring);
    assert!(
        dev.drain_actions().is_empty(),
        "in-flight bulk IN must not queue duplicate host actions"
    );
    assert!(xhci.pop_completion().is_none());

    dev.push_completion(UsbHostCompletion::BulkIn {
        id: in_id,
        result: UsbHostCompletionIn::Success {
            data: in_payload.to_vec(),
        },
    });
    xhci.tick_bulk_in(&mut mem, &mut bulk_in_cursor, 1);
    assert_eq!(
        bulk_in_cursor.dequeue_ptr(),
        bulk_in_ring + TRB_LEN as u64,
        "bulk IN completion should advance the dequeue pointer"
    );
    assert_eq!(
        xhci.pop_completion(),
        Some(Completion {
            kind: CompletionKind::BulkIn,
            bytes_transferred: in_payload.len(),
        })
    );

    let mut got = vec![0u8; in_payload.len()];
    mem.read(in_buf as u32, &mut got);
    assert_eq!(got, in_payload);
}

#[test]
fn xhci_transfer_executor_bulk_in_naks_until_webusb_completion() {
    use aero_usb::xhci::transfer::{
        write_trb, CompletionCode, Trb as XferTrb, TrbType as XferTrbType, XhciTransferExecutor,
    };

    fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> XferTrb {
        let mut trb = XferTrb::new(buf_ptr, len & XferTrb::STATUS_TRANSFER_LEN_MASK, 0);
        trb.set_cycle(cycle);
        trb.set_trb_type(XferTrbType::Normal);
        if ioc {
            trb.control |= XferTrb::CONTROL_IOC_BIT;
        }
        trb
    }

    const RING_BASE: u64 = 0x1000;
    const DATA_BUF: u64 = 0x2000;

    let dev = UsbWebUsbPassthroughDevice::new();
    let mut exec = XhciTransferExecutor::new(Box::new(dev.clone()));
    exec.add_endpoint(0x81, RING_BASE);

    let mut mem = TestMemory::new(0x10000);
    write_trb(
        &mut mem,
        RING_BASE,
        make_normal_trb(DATA_BUF, 8, true, true),
    );

    // Tick #1: TD is pending (NAK) and queues a single host action.
    exec.tick_1ms(&mut mem);
    assert!(exec.take_events().is_empty());
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE,
        "dequeue pointer must not advance while TD is pending"
    );

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (id, endpoint, length) = match actions.pop().unwrap() {
        UsbHostAction::BulkIn {
            id,
            endpoint,
            length,
        } => (id, endpoint, length),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(endpoint, 0x81);
    assert_eq!(length, 8);

    // Tick #2 without completion: still pending and no duplicate action.
    exec.tick_1ms(&mut mem);
    assert!(dev.drain_actions().is_empty());
    assert!(exec.take_events().is_empty());
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE
    );

    // Provide completion.
    let payload = [0u8, 1, 2, 3, 4, 5, 6, 7];
    dev.push_completion(UsbHostCompletion::BulkIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.to_vec(),
        },
    });

    exec.tick_1ms(&mut mem);
    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x81);
    assert_eq!(events[0].completion_code, CompletionCode::Success);
    assert_eq!(events[0].residual, 0);
    assert_eq!(
        exec.endpoint_state(0x81).unwrap().ring.dequeue_ptr,
        RING_BASE + 16,
        "completion should advance past the Normal TRB"
    );

    let mut got = [0u8; 8];
    mem.read(DATA_BUF as u32, &mut got);
    assert_eq!(got, payload);
}

#[test]
fn xhci_transfer_executor_bulk_out_naks_until_webusb_completion() {
    use aero_usb::xhci::transfer::{
        write_trb, CompletionCode, Trb as XferTrb, TrbType as XferTrbType, XhciTransferExecutor,
    };

    fn make_normal_trb(buf_ptr: u64, len: u32, cycle: bool, ioc: bool) -> XferTrb {
        let mut trb = XferTrb::new(buf_ptr, len & XferTrb::STATUS_TRANSFER_LEN_MASK, 0);
        trb.set_cycle(cycle);
        trb.set_trb_type(XferTrbType::Normal);
        if ioc {
            trb.control |= XferTrb::CONTROL_IOC_BIT;
        }
        trb
    }

    const RING_BASE: u64 = 0x3000;
    const DATA_BUF: u64 = 0x4000;

    let dev = UsbWebUsbPassthroughDevice::new();
    let mut exec = XhciTransferExecutor::new(Box::new(dev.clone()));
    exec.add_endpoint(0x01, RING_BASE);

    let mut mem = TestMemory::new(0x10000);
    let payload = [0xAAu8, 0xBB, 0xCC, 0xDD];
    mem.write(DATA_BUF as u32, &payload);
    write_trb(
        &mut mem,
        RING_BASE,
        make_normal_trb(DATA_BUF, payload.len() as u32, true, true),
    );

    // Tick #1: TD is pending (NAK) and queues a single host action.
    exec.tick_1ms(&mut mem);
    assert!(exec.take_events().is_empty());
    assert_eq!(
        exec.endpoint_state(0x01).unwrap().ring.dequeue_ptr,
        RING_BASE,
        "dequeue pointer must not advance while TD is pending"
    );

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (id, endpoint, data) = match actions.pop().unwrap() {
        UsbHostAction::BulkOut { id, endpoint, data } => (id, endpoint, data),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(endpoint, 0x01);
    assert_eq!(data.as_slice(), payload.as_slice());

    // Tick #2 without completion: still pending and no duplicate action.
    exec.tick_1ms(&mut mem);
    assert!(dev.drain_actions().is_empty());
    assert!(exec.take_events().is_empty());
    assert_eq!(
        exec.endpoint_state(0x01).unwrap().ring.dequeue_ptr,
        RING_BASE
    );

    dev.push_completion(UsbHostCompletion::BulkOut {
        id,
        result: UsbHostCompletionOut::Success {
            bytes_written: payload.len() as u32,
        },
    });

    exec.tick_1ms(&mut mem);
    let events = exec.take_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].ep_addr, 0x01);
    assert_eq!(events[0].completion_code, CompletionCode::Success);
    assert_eq!(events[0].residual, 0);
    assert_eq!(
        exec.endpoint_state(0x01).unwrap().ring.dequeue_ptr,
        RING_BASE + 16,
        "completion should advance past the Normal TRB"
    );
}
