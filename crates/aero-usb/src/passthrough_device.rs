//! UHCI-visible WebUSB passthrough USB device.
//!
//! This adapts the request-level [`crate::passthrough::UsbPassthroughDevice`] API to the
//! transaction-level [`crate::usb::UsbDevice`] interface used by the UHCI controller.
//!
//! The key impedance mismatch is that WebUSB operations are asynchronous (Promise-based), while
//! UHCI consumes Transfer Descriptors synchronously. We represent an in-flight host action as a
//! TD-level NAK: the relevant DATA/STATUS stage TD remains active and will be retried by the UHCI
//! schedule until a completion is pushed.

use crate::passthrough::{
    ControlResponse, PendingSummary, SetupPacket as HostSetupPacket, UsbHostAction,
    UsbHostCompletion, UsbInResult, UsbOutResult, UsbPassthroughDevice,
};
use crate::usb::{SetupPacket as UsbSetupPacket, UsbDevice, UsbHandshake, UsbSpeed};
use std::mem;

const USB_REQUEST_SET_ADDRESS: u8 = 0x05;

#[derive(Debug, Clone)]
enum ControlStage {
    InData { data: Vec<u8>, offset: usize },
    InDataPending,
    OutData { expected: usize, received: Vec<u8> },
    StatusIn,
    StatusInPending { data: Option<Vec<u8>> },
    StatusOut,
    StatusOutPending,
}

#[derive(Debug, Clone)]
struct ControlState {
    setup: UsbSetupPacket,
    stage: ControlStage,
    /// When `true`, the control pipe is halted and all subsequent transactions stall.
    stalled: bool,
}

/// USB device model that bridges guest UHCI transactions to [`UsbPassthroughDevice`] host actions.
///
/// This is the Rust-side implementation of the "async WebUSB host actions ↔ synchronous UHCI TDs"
/// contract:
/// - SETUP transactions always ACK (UHCI uses [`UsbBus`](crate::usb::UsbBus) which unconditionally
///   ACKs SETUP TDs).
/// - When a host action is in-flight, the DATA/STATUS TDs return NAK so the TD remains active and is
///   retried on later frames.
/// - `SET_ADDRESS` is virtualized: it is not forwarded as a host action, and the guest-visible
///   address only updates after the STATUS stage completes.
#[derive(Debug, Default)]
pub struct UsbWebUsbPassthroughDevice {
    address: u8,
    pending_address: Option<u8>,
    control: Option<ControlState>,
    passthrough: UsbPassthroughDevice,
}

impl UsbWebUsbPassthroughDevice {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current guest-visible USB address.
    pub fn address(&self) -> u8 {
        self.address
    }

    /// Reset the guest-visible device state and the underlying host-action queue.
    ///
    /// This is an inherent helper (in addition to the [`UsbDevice`] trait method) so that
    /// host/wasm glue can reset the passthrough device without needing a `dyn UsbDevice`.
    pub fn reset(&mut self) {
        <Self as UsbDevice>::reset(self);
    }

    pub fn pop_action(&mut self) -> Option<UsbHostAction> {
        self.passthrough.pop_action()
    }

    pub fn drain_actions(&mut self) -> Vec<UsbHostAction> {
        self.passthrough.drain_actions()
    }

    pub fn push_completion(&mut self, completion: UsbHostCompletion) {
        self.passthrough.push_completion(completion);
    }

    pub fn pending_summary(&self) -> PendingSummary {
        self.passthrough.pending_summary()
    }

    fn to_passthrough_setup(setup: UsbSetupPacket) -> HostSetupPacket {
        HostSetupPacket {
            bm_request_type: setup.request_type,
            b_request: setup.request,
            w_value: setup.value,
            w_index: setup.index,
            w_length: setup.length,
        }
    }
}

impl UsbDevice for UsbWebUsbPassthroughDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.control = None;
        self.passthrough.reset();
    }

    fn address(&self) -> u8 {
        self.address
    }

    fn handle_setup(&mut self, setup: UsbSetupPacket) {
        // Starting a new SETUP always aborts any in-flight control transfer.
        self.control = None;
        self.passthrough.cancel_control_transfer();
        // `SET_ADDRESS` only takes effect after the STATUS stage; if the control transfer was
        // aborted by a new SETUP, discard the pending address.
        self.pending_address = None;

        // Intercept SET_ADDRESS so it is never forwarded to the host.
        if setup.request == USB_REQUEST_SET_ADDRESS {
            if setup.request_type != 0x00
                || setup.index != 0
                || setup.length != 0
                || setup.value > 127
            {
                self.control = Some(ControlState {
                    setup,
                    stage: ControlStage::StatusIn,
                    stalled: true,
                });
                return;
            }

            self.pending_address = Some((setup.value & 0x7F) as u8);
            self.control = Some(ControlState {
                setup,
                stage: ControlStage::StatusIn,
                stalled: false,
            });
            return;
        }

        let stage = if setup.request_type & 0x80 != 0 {
            let resp = self
                .passthrough
                .handle_control_request(Self::to_passthrough_setup(setup), None);
            match resp {
                ControlResponse::Data(mut data) => {
                    let requested = setup.length as usize;
                    if data.len() > requested {
                        data.truncate(requested);
                    }
                    if requested == 0 {
                        ControlStage::StatusOut
                    } else {
                        ControlStage::InData { data, offset: 0 }
                    }
                }
                ControlResponse::Ack => {
                    if setup.length == 0 {
                        ControlStage::StatusOut
                    } else {
                        ControlStage::InData {
                            data: Vec::new(),
                            offset: 0,
                        }
                    }
                }
                ControlResponse::Nak => {
                    if setup.length == 0 {
                        ControlStage::StatusOutPending
                    } else {
                        ControlStage::InDataPending
                    }
                }
                ControlResponse::Stall | ControlResponse::Timeout => {
                    self.control = Some(ControlState {
                        setup,
                        stage: ControlStage::StatusOut,
                        stalled: true,
                    });
                    return;
                }
            }
        } else if setup.length == 0 {
            match self
                .passthrough
                .handle_control_request(Self::to_passthrough_setup(setup), None)
            {
                ControlResponse::Ack => ControlStage::StatusIn,
                ControlResponse::Nak => ControlStage::StatusInPending { data: None },
                ControlResponse::Stall | ControlResponse::Timeout | ControlResponse::Data(_) => {
                    self.control = Some(ControlState {
                        setup,
                        stage: ControlStage::StatusIn,
                        stalled: true,
                    });
                    return;
                }
            }
        } else {
            ControlStage::OutData {
                expected: setup.length as usize,
                received: Vec::with_capacity(setup.length as usize),
            }
        };

        self.control = Some(ControlState {
            setup,
            stage,
            stalled: false,
        });
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        if ep != 0 {
            let endpoint = ep & 0x0F;
            return match self.passthrough.handle_out_transfer(endpoint, data) {
                UsbOutResult::Ack => UsbHandshake::Ack { bytes: data.len() },
                UsbOutResult::Nak => UsbHandshake::Nak,
                UsbOutResult::Stall => UsbHandshake::Stall,
                UsbOutResult::Timeout => UsbHandshake::Timeout,
            };
        }

        let Some(state) = self.control.as_mut() else {
            return UsbHandshake::Nak;
        };
        if state.stalled {
            return UsbHandshake::Stall;
        }

        match &mut state.stage {
            ControlStage::OutData { expected, received } => {
                let remaining = expected.saturating_sub(received.len());
                let chunk_len = remaining.min(data.len());
                received.extend_from_slice(&data[..chunk_len]);
                if received.len() >= *expected {
                    let setup = state.setup;
                    match self.passthrough.handle_control_request(
                        Self::to_passthrough_setup(setup),
                        Some(received.as_slice()),
                    ) {
                        ControlResponse::Ack => state.stage = ControlStage::StatusIn,
                        ControlResponse::Nak => {
                            // The DATA stage carries the OUT payload; once buffered we ACK the final
                            // OUT packet and backpressure using NAK on the STATUS stage.
                            state.stage = ControlStage::StatusInPending {
                                data: Some(mem::take(received)),
                            };
                        }
                        ControlResponse::Stall => return UsbHandshake::Stall,
                        ControlResponse::Timeout => return UsbHandshake::Timeout,
                        ControlResponse::Data(_) => return UsbHandshake::Stall,
                    }
                }
                UsbHandshake::Ack { bytes: chunk_len }
            }
            ControlStage::StatusOut => {
                if !data.is_empty() {
                    return UsbHandshake::Stall;
                }
                self.control = None;
                UsbHandshake::Ack { bytes: 0 }
            }
            ControlStage::StatusOutPending => {
                if !data.is_empty() {
                    return UsbHandshake::Stall;
                }
                let setup = state.setup;
                match self
                    .passthrough
                    .handle_control_request(Self::to_passthrough_setup(setup), None)
                {
                    ControlResponse::Nak => UsbHandshake::Nak,
                    ControlResponse::Stall => UsbHandshake::Stall,
                    ControlResponse::Timeout => UsbHandshake::Timeout,
                    // Whether the model reports `Ack` or `Data([])`, treat this as the completion
                    // point for the whole control transfer (status stage).
                    ControlResponse::Ack | ControlResponse::Data(_) => {
                        self.control = None;
                        UsbHandshake::Ack { bytes: 0 }
                    }
                }
            }
            _ => UsbHandshake::Nak,
        }
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        if ep != 0 {
            let endpoint = 0x80 | (ep & 0x0F);
            return match self.passthrough.handle_in_transfer(endpoint, buf.len()) {
                UsbInResult::Data(data) => {
                    let len = buf.len().min(data.len());
                    buf[..len].copy_from_slice(&data[..len]);
                    UsbHandshake::Ack { bytes: len }
                }
                UsbInResult::Nak => UsbHandshake::Nak,
                UsbInResult::Stall => UsbHandshake::Stall,
                UsbInResult::Timeout => UsbHandshake::Timeout,
            };
        }

        let Some(state) = self.control.as_mut() else {
            return UsbHandshake::Nak;
        };
        if state.stalled {
            return UsbHandshake::Stall;
        }

        match &mut state.stage {
            ControlStage::InData { data, offset } => {
                let remaining = data.len().saturating_sub(*offset);
                let chunk_len = remaining.min(buf.len());
                buf[..chunk_len].copy_from_slice(&data[*offset..*offset + chunk_len]);
                *offset += chunk_len;
                if *offset >= data.len() {
                    state.stage = ControlStage::StatusOut;
                }
                UsbHandshake::Ack { bytes: chunk_len }
            }
            ControlStage::InDataPending => {
                let setup = state.setup;
                match self
                    .passthrough
                    .handle_control_request(Self::to_passthrough_setup(setup), None)
                {
                    ControlResponse::Nak => UsbHandshake::Nak,
                    ControlResponse::Stall => UsbHandshake::Stall,
                    ControlResponse::Timeout => UsbHandshake::Timeout,
                    ControlResponse::Ack => {
                        state.stage = ControlStage::StatusOut;
                        UsbHandshake::Ack { bytes: 0 }
                    }
                    ControlResponse::Data(mut data) => {
                        let requested = setup.length as usize;
                        if data.len() > requested {
                            data.truncate(requested);
                        }
                        // A zero-length DATA stage (ZLP) is a valid completion when wLength > 0.
                        if requested == 0 || data.is_empty() {
                            state.stage = ControlStage::StatusOut;
                            return UsbHandshake::Ack { bytes: 0 };
                        }

                        let chunk_len = data.len().min(buf.len());
                        buf[..chunk_len].copy_from_slice(&data[..chunk_len]);
                        if chunk_len >= data.len() {
                            state.stage = ControlStage::StatusOut;
                        } else {
                            state.stage = ControlStage::InData {
                                data,
                                offset: chunk_len,
                            };
                        }
                        UsbHandshake::Ack { bytes: chunk_len }
                    }
                }
            }
            ControlStage::StatusIn => {
                if !buf.is_empty() {
                    return UsbHandshake::Stall;
                }
                if let Some(addr) = self.pending_address.take() {
                    self.address = addr;
                }
                self.control = None;
                UsbHandshake::Ack { bytes: 0 }
            }
            ControlStage::StatusInPending { data } => {
                if !buf.is_empty() {
                    return UsbHandshake::Stall;
                }
                let setup = state.setup;
                match self
                    .passthrough
                    .handle_control_request(Self::to_passthrough_setup(setup), data.as_deref())
                {
                    ControlResponse::Nak => UsbHandshake::Nak,
                    ControlResponse::Ack => {
                        if let Some(addr) = self.pending_address.take() {
                            self.address = addr;
                        }
                        self.control = None;
                        UsbHandshake::Ack { bytes: 0 }
                    }
                    ControlResponse::Stall => UsbHandshake::Stall,
                    ControlResponse::Timeout => UsbHandshake::Timeout,
                    ControlResponse::Data(_) => UsbHandshake::Stall,
                }
            }
            _ => UsbHandshake::Nak,
        }
    }
}
