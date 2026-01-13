//! xHCI + WebUSB passthrough integration smoke test.
//!
//! The production USB stack currently exposes a UHCI controller model, but our async passthrough
//! devices (`UsbWebUsbPassthroughDevice` / `UsbPassthroughDevice`) are intentionally host-controller
//! agnostic: they surface *pending* I/O as `NAK` and complete later when the host pushes a matching
//! `UsbHostCompletion`.
//!
//! This test encodes the expected xHCI transfer-engine behaviour against that async device model:
//! a control transfer should issue exactly one `UsbHostAction`, then remain pending until a
//! completion is injected, at which point the transfer completes and the IN data is DMA'd into the
//! guest buffer.
//!
//! Note: This is a *logical* xHCI transfer-engine harness (Enable Slot / Address Device / Configure
//! Endpoint + transfer progression). It intentionally does not model the full xHCI register block
//! or TRB ring mechanics; the contract under test is the interaction between a "retry until ready"
//! host controller and the async passthrough device model.

use std::collections::VecDeque;

use aero_usb::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use aero_usb::hub::RootHub;
use aero_usb::passthrough::{
    SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionIn,
    UsbHostCompletionOut,
};
use aero_usb::{SetupPacket, UsbDeviceModel, UsbWebUsbPassthroughDevice};

mod util;

use util::{Alloc, TestMemory};

#[derive(Clone, Copy, Debug)]
struct Slot {
    root_port: usize,
    address: u8,
    configured: bool,
}

#[derive(Clone, Debug)]
struct ControlInTransfer {
    slot_id: u8,
    setup: SetupPacket,
    buf_ptr: u64,
    len: usize,
    stage: ControlInStage,
    actual_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlInStage {
    Setup,
    Data,
    Status,
}

#[derive(Clone, Debug)]
enum PendingTransfer {
    ControlIn(ControlInTransfer),
    BulkIn {
        slot_id: u8,
        ep: u8,
        buf_ptr: u64,
        len: usize,
    },
    BulkOut {
        slot_id: u8,
        ep: u8,
        buf_ptr: u64,
        len: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletedTransfer {
    slot_id: u8,
    bytes_transferred: usize,
}

/// Minimal xHCI-ish controller harness:
/// - root hub ports
/// - slot enabling
/// - Address Device (SET_ADDRESS) command
/// - endpoint configuration gate (for bulk)
/// - transfer progression with "retry on NAK" semantics
struct XhciHarness {
    hub: RootHub,
    slot: Option<Slot>,
    pending: VecDeque<PendingTransfer>,
    completed: VecDeque<CompletedTransfer>,
}

impl XhciHarness {
    fn new() -> Self {
        Self {
            hub: RootHub::new(),
            slot: None,
            pending: VecDeque::new(),
            completed: VecDeque::new(),
        }
    }

    fn attach_root_port(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        self.hub.attach(port, model);
        // The UHCI root hub model disables the port on attach until the guest performs a reset +
        // enable sequence. For this harness, force the port enabled so the device is reachable.
        self.hub.force_enable_for_tests(port);
    }

    fn enable_slot(&mut self, port: usize) -> u8 {
        assert!(self.slot.is_none(), "harness supports a single slot");
        let slot_id = 1u8;
        self.slot = Some(Slot {
            root_port: port,
            address: 0,
            configured: false,
        });
        slot_id
    }

    fn address_device(&mut self, slot_id: u8) {
        let slot = self
            .slot
            .as_mut()
            .expect("slot must be enabled before Address Device");
        assert_eq!(slot_id, 1, "harness supports a single slot id=1");

        // xHCI "Address Device" conceptually performs SET_ADDRESS over EP0 and updates its device
        // context. We drive the existing AttachedUsbDevice control-pipe state machine directly.
        let set_address = SetupPacket {
            bm_request_type: 0x00, // HostToDevice | Standard | Device
            b_request: 0x05,       // SET_ADDRESS
            w_value: slot_id as u16,
            w_index: 0,
            w_length: 0,
        };

        let dev = self
            .hub
            .device_mut_for_address(0)
            .expect("device should be reachable at address 0");

        assert_eq!(dev.handle_setup(set_address), UsbOutResult::Ack);
        // STATUS stage: IN ZLP.
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));

        slot.address = slot_id;
    }

    fn configure_endpoints(&mut self, slot_id: u8) {
        let slot = self
            .slot
            .as_mut()
            .expect("slot must be enabled before Configure Endpoint");
        assert_eq!(slot_id, 1, "harness supports a single slot id=1");
        slot.configured = true;
    }

    fn submit_control_in(&mut self, slot_id: u8, setup: SetupPacket, buf_ptr: u64, len: usize) {
        self.pending.push_back(PendingTransfer::ControlIn(ControlInTransfer {
            slot_id,
            setup,
            buf_ptr,
            len,
            stage: ControlInStage::Setup,
            actual_len: 0,
        }));
    }

    fn submit_bulk_out(&mut self, slot_id: u8, ep: u8, buf_ptr: u64, len: usize) {
        self.pending
            .push_back(PendingTransfer::BulkOut { slot_id, ep, buf_ptr, len });
    }

    fn submit_bulk_in(&mut self, slot_id: u8, ep: u8, buf_ptr: u64, len: usize) {
        self.pending
            .push_back(PendingTransfer::BulkIn { slot_id, ep, buf_ptr, len });
    }

    fn pop_completed(&mut self) -> Option<CompletedTransfer> {
        self.completed.pop_front()
    }

    fn device_mut_for_slot(&mut self, slot_id: u8) -> &mut AttachedUsbDevice {
        let slot = self.slot.expect("slot not enabled");
        assert_eq!(slot_id, 1, "harness supports a single slot id=1");
        self.hub
            .device_mut_for_address(slot.address)
            .expect("device should be reachable at addressed slot")
    }

    fn tick(&mut self, mem: &mut TestMemory) {
        // Tick device timers to mirror how real controllers drive time-based state machines.
        self.hub.tick_1ms();

        let Some(mut transfer) = self.pending.pop_front() else {
            return;
        };

        match &mut transfer {
            PendingTransfer::ControlIn(t) => {
                let dev = self.device_mut_for_slot(t.slot_id);
                match t.stage {
                    ControlInStage::Setup => {
                        match dev.handle_setup(t.setup) {
                            UsbOutResult::Ack => {
                                t.stage = ControlInStage::Data;
                            }
                            UsbOutResult::Nak => unreachable!(
                                "AttachedUsbDevice::handle_setup should not return NAK"
                            ),
                            UsbOutResult::Stall | UsbOutResult::Timeout => {
                                self.completed.push_back(CompletedTransfer {
                                    slot_id: t.slot_id,
                                    bytes_transferred: 0,
                                });
                                return;
                            }
                        }
                    }
                    ControlInStage::Data => {}
                    ControlInStage::Status => {}
                }

                if t.stage == ControlInStage::Data {
                    match dev.handle_in(0, t.len) {
                        UsbInResult::Data(data) => {
                            t.actual_len = data.len();
                            mem.write(t.buf_ptr as u32, &data);
                            t.stage = ControlInStage::Status;
                        }
                        UsbInResult::Nak => {
                            // Retry later.
                            self.pending.push_front(transfer);
                            return;
                        }
                        UsbInResult::Stall | UsbInResult::Timeout => {
                            self.completed.push_back(CompletedTransfer {
                                slot_id: t.slot_id,
                                bytes_transferred: 0,
                            });
                            return;
                        }
                    }
                }

                if t.stage == ControlInStage::Status {
                    match dev.handle_out(0, &[]) {
                        UsbOutResult::Ack => {
                            self.completed.push_back(CompletedTransfer {
                                slot_id: t.slot_id,
                                bytes_transferred: t.actual_len,
                            });
                        }
                        UsbOutResult::Nak => {
                            self.pending.push_front(transfer);
                        }
                        UsbOutResult::Stall | UsbOutResult::Timeout => {
                            self.completed.push_back(CompletedTransfer {
                                slot_id: t.slot_id,
                                bytes_transferred: 0,
                            });
                        }
                    }
                }
            }
            PendingTransfer::BulkOut {
                slot_id,
                ep,
                buf_ptr,
                len,
            } => {
                let slot = self.slot.expect("slot not enabled");
                assert!(
                    slot.configured,
                    "bulk endpoints must be configured before use"
                );
                assert_eq!(*slot_id, 1);

                let mut buf = vec![0u8; *len];
                mem.read(*buf_ptr as u32, &mut buf);

                let dev = self.device_mut_for_slot(*slot_id);
                match dev.handle_out(*ep, &buf) {
                    UsbOutResult::Ack => {
                        self.completed.push_back(CompletedTransfer {
                            slot_id: *slot_id,
                            bytes_transferred: *len,
                        });
                    }
                    UsbOutResult::Nak => self.pending.push_front(transfer),
                    UsbOutResult::Stall | UsbOutResult::Timeout => {
                        self.completed.push_back(CompletedTransfer {
                            slot_id: *slot_id,
                            bytes_transferred: 0,
                        });
                    }
                }
            }
            PendingTransfer::BulkIn {
                slot_id,
                ep,
                buf_ptr,
                len,
            } => {
                let slot = self.slot.expect("slot not enabled");
                assert!(
                    slot.configured,
                    "bulk endpoints must be configured before use"
                );
                assert_eq!(*slot_id, 1);

                let dev = self.device_mut_for_slot(*slot_id);
                match dev.handle_in(*ep, *len) {
                    UsbInResult::Data(data) => {
                        mem.write(*buf_ptr as u32, &data);
                        self.completed.push_back(CompletedTransfer {
                            slot_id: *slot_id,
                            bytes_transferred: data.len(),
                        });
                    }
                    UsbInResult::Nak => self.pending.push_front(transfer),
                    UsbInResult::Stall | UsbInResult::Timeout => {
                        self.completed.push_back(CompletedTransfer {
                            slot_id: *slot_id,
                            bytes_transferred: 0,
                        });
                    }
                }
            }
        }
    }
}

#[test]
fn xhci_control_in_get_descriptor_completes_after_host_completion_and_dmas_data() {
    let mut xhci = XhciHarness::new();
    let dev = UsbWebUsbPassthroughDevice::new();
    xhci.attach_root_port(0, Box::new(dev.clone()));

    let slot_id = xhci.enable_slot(0);
    xhci.address_device(slot_id);
    xhci.configure_endpoints(slot_id);

    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x1000);

    let setup = SetupPacket {
        bm_request_type: 0x80, // DeviceToHost | Standard | Device
        b_request: 0x06,       // GET_DESCRIPTOR
        w_value: 0x0100,       // DEVICE descriptor, index 0
        w_index: 0,
        w_length: 18,
    };

    let buf_ptr = alloc.alloc(64, 0x10) as u64;
    xhci.submit_control_in(slot_id, setup, buf_ptr, setup.w_length as usize);

    // Tick #1: SETUP is issued and the DATA stage sees NAK (pending), so the passthrough model
    // should have queued exactly one host action.
    xhci.tick(&mut mem);

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
        xhci.pop_completed().is_none(),
        "transfer should still be pending without a host completion"
    );

    // Provide a deterministic completion payload and re-tick: transfer should complete and DMA data
    // into the guest buffer.
    let payload: Vec<u8> = (0u8..18u8).collect();
    dev.push_completion(UsbHostCompletion::ControlIn {
        id,
        result: UsbHostCompletionIn::Success {
            data: payload.clone(),
        },
    });

    xhci.tick(&mut mem);

    let completed = xhci
        .pop_completed()
        .expect("expected transfer completion after host completion");
    assert_eq!(completed.slot_id, slot_id);
    assert_eq!(completed.bytes_transferred, payload.len());

    let mut got = vec![0u8; payload.len()];
    mem.read(buf_ptr as u32, &mut got);
    assert_eq!(got, payload);

    assert!(
        dev.drain_actions().is_empty(),
        "no duplicate host actions should be emitted across retries"
    );
}

#[test]
fn xhci_bulk_in_out_normal_trb_roundtrip() {
    let mut xhci = XhciHarness::new();
    let dev = UsbWebUsbPassthroughDevice::new();
    xhci.attach_root_port(0, Box::new(dev.clone()));

    let slot_id = xhci.enable_slot(0);
    xhci.address_device(slot_id);
    xhci.configure_endpoints(slot_id);

    let mut mem = TestMemory::new(0x10000);
    let mut alloc = Alloc::new(0x2000);

    // Bulk OUT (endpoint 1 OUT).
    let out_payload = [0xAAu8, 0xBB, 0xCC, 0xDD];
    let out_buf = alloc.alloc(out_payload.len() as u32, 0x10) as u64;
    mem.write(out_buf as u32, &out_payload);
    xhci.submit_bulk_out(slot_id, 1, out_buf, out_payload.len());
    xhci.tick(&mut mem);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (out_id, out_endpoint, out_data) = match actions.pop().unwrap() {
        UsbHostAction::BulkOut { id, endpoint, data } => (id, endpoint, data),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(out_endpoint, 0x01);
    assert_eq!(out_data, out_payload);

    dev.push_completion(UsbHostCompletion::BulkOut {
        id: out_id,
        result: UsbHostCompletionOut::Success {
            bytes_written: out_payload.len() as u32,
        },
    });
    xhci.tick(&mut mem);
    assert_eq!(
        xhci.pop_completed(),
        Some(CompletedTransfer {
            slot_id,
            bytes_transferred: out_payload.len(),
        })
    );

    // Bulk IN (endpoint 1 IN).
    let in_payload = [1u8, 2, 3, 4, 5];
    let in_buf = alloc.alloc(in_payload.len() as u32, 0x10) as u64;
    xhci.submit_bulk_in(slot_id, 1, in_buf, in_payload.len());
    xhci.tick(&mut mem);

    let mut actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    let (in_id, in_endpoint, in_len) = match actions.pop().unwrap() {
        UsbHostAction::BulkIn { id, endpoint, length } => (id, endpoint, length),
        other => panic!("unexpected action: {other:?}"),
    };
    assert_eq!(in_endpoint, 0x81);
    assert_eq!(in_len as usize, in_payload.len());

    dev.push_completion(UsbHostCompletion::BulkIn {
        id: in_id,
        result: UsbHostCompletionIn::Success {
            data: in_payload.to_vec(),
        },
    });
    xhci.tick(&mut mem);
    assert_eq!(
        xhci.pop_completed(),
        Some(CompletedTransfer {
            slot_id,
            bytes_transferred: in_payload.len(),
        })
    );

    let mut got = vec![0u8; in_payload.len()];
    mem.read(in_buf as u32, &mut got);
    assert_eq!(got, in_payload);
}
