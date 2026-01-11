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
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use std::cell::RefCell;
use std::mem;
use std::rc::Rc;

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

    /// Clears host-side WebUSB bookkeeping without changing guest-visible USB state.
    ///
    /// WebUSB host actions are backed by JS Promises that cannot be resumed after a VM snapshot
    /// restore. Host/wasm glue should call this after restore so the next UHCI TD retry re-emits
    /// fresh host actions instead of deadlocking on a completion that will never arrive.
    pub fn reset_host_state_for_restore(&mut self) {
        self.passthrough.reset();
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

/// Adapter for sharing a [`UsbWebUsbPassthroughDevice`] between host glue (e.g. WASM bindings)
/// and an emulated USB bus.
///
/// This mirrors the `Rc<RefCell<_>>` pattern used by the WebHID passthrough device wrapper so the
/// host can keep a handle for draining actions / pushing completions while the UHCI bus owns a
/// `Box<dyn UsbDevice>`.
#[derive(Clone)]
pub struct SharedUsbWebUsbPassthroughDevice(pub Rc<RefCell<UsbWebUsbPassthroughDevice>>);

impl UsbDevice for SharedUsbWebUsbPassthroughDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn speed(&self) -> UsbSpeed {
        self.0.borrow().speed()
    }

    fn tick_1ms(&mut self) {
        self.0.borrow_mut().tick_1ms();
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn address(&self) -> u8 {
        self.0.borrow().address()
    }

    fn handle_setup(&mut self, setup: UsbSetupPacket) {
        self.0.borrow_mut().handle_setup(setup);
    }

    fn handle_out(&mut self, ep: u8, data: &[u8]) -> UsbHandshake {
        self.0.borrow_mut().handle_out(ep, data)
    }

    fn handle_in(&mut self, ep: u8, buf: &mut [u8]) -> UsbHandshake {
        self.0.borrow_mut().handle_in(ep, buf)
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
        //
        // Note: Only match Standard/Device recipient requests (`bmRequestType & 0x7f == 0`), so
        // vendor/class requests that happen to use `bRequest == 0x05` still get passed through.
        if (setup.request_type & 0x7f) == 0x00 && setup.request == USB_REQUEST_SET_ADDRESS {
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
                        ControlResponse::Stall => {
                            state.stalled = true;
                            return UsbHandshake::Stall;
                        }
                        ControlResponse::Timeout => return UsbHandshake::Timeout,
                        ControlResponse::Data(_) => return UsbHandshake::Stall,
                    }
                }
                // ACK covers the entire packet; we may ignore any bytes beyond wLength but must not
                // report a short write to the UHCI layer.
                UsbHandshake::Ack { bytes: data.len() }
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
                    ControlResponse::Stall => {
                        state.stalled = true;
                        UsbHandshake::Stall
                    }
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
                    ControlResponse::Stall => {
                        state.stalled = true;
                        UsbHandshake::Stall
                    }
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
                    ControlResponse::Stall => {
                        state.stalled = true;
                        UsbHandshake::Stall
                    }
                    ControlResponse::Timeout => UsbHandshake::Timeout,
                    ControlResponse::Data(_) => UsbHandshake::Stall,
                }
            }
            _ => UsbHandshake::Nak,
        }
    }
}

impl IoSnapshot for UsbWebUsbPassthroughDevice {
    const DEVICE_ID: [u8; 4] = *b"WUSB";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_PENDING_ADDRESS: u16 = 2;
        const TAG_CONTROL: u16 = 3;
        const TAG_PASSTHROUGH: u16 = 4;
        const TAG_PASSTHROUGH_FULL: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_ADDRESS, self.address);
        if let Some(addr) = self.pending_address {
            w.field_u8(TAG_PENDING_ADDRESS, addr);
        }

        w.field_bytes(TAG_PASSTHROUGH, self.passthrough.save_state());
        w.field_bytes(TAG_PASSTHROUGH_FULL, self.passthrough.snapshot_save());

        if let Some(control) = &self.control {
            let setup = control.setup;
            let mut enc = Encoder::new()
                .bool(control.stalled)
                .u8(setup.request_type)
                .u8(setup.request)
                .u16(setup.value)
                .u16(setup.index)
                .u16(setup.length);

            match &control.stage {
                ControlStage::InData { data, offset } => {
                    enc = enc.u8(0).vec_u8(data).u32(*offset as u32);
                }
                ControlStage::InDataPending => {
                    enc = enc.u8(1);
                }
                ControlStage::OutData { expected, received } => {
                    enc = enc.u8(2).u32(*expected as u32).vec_u8(received);
                }
                ControlStage::StatusIn => {
                    enc = enc.u8(3);
                }
                ControlStage::StatusInPending { data } => {
                    enc = enc.u8(4).bool(data.is_some());
                    if let Some(buf) = data {
                        enc = enc.vec_u8(buf);
                    }
                }
                ControlStage::StatusOut => {
                    enc = enc.u8(5);
                }
                ControlStage::StatusOutPending => {
                    enc = enc.u8(6);
                }
            }

            w.field_bytes(TAG_CONTROL, enc.finish());
        }

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ADDRESS: u16 = 1;
        const TAG_PENDING_ADDRESS: u16 = 2;
        const TAG_CONTROL: u16 = 3;
        const TAG_PASSTHROUGH: u16 = 4;
        const TAG_PASSTHROUGH_FULL: u16 = 5;

        const MAX_CONTROL_DATA_BYTES: usize = 128 * 1024;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Start from a clean slate (also clears the underlying host-action queues).
        self.reset();

        self.address = r.u8(TAG_ADDRESS)?.unwrap_or(0);
        self.pending_address = r.u8(TAG_PENDING_ADDRESS)?;
        if self.address > 127 {
            return Err(SnapshotError::InvalidFieldEncoding("invalid usb address"));
        }
        if self.pending_address.is_some_and(|v| v > 127) {
            return Err(SnapshotError::InvalidFieldEncoding(
                "invalid pending usb address",
            ));
        }

        if let Some(buf) = r.bytes(TAG_PASSTHROUGH_FULL) {
            self.passthrough.snapshot_load(buf)?;
        } else if let Some(buf) = r.bytes(TAG_PASSTHROUGH) {
            self.passthrough.load_state(buf)?;
        } else {
            self.passthrough.reset();
        }

        if let Some(buf) = r.bytes(TAG_CONTROL) {
            fn dec_vec_u8_limited(d: &mut Decoder<'_>, max: usize) -> SnapshotResult<Vec<u8>> {
                let len = d.u32()? as usize;
                if len > max {
                    return Err(SnapshotError::InvalidFieldEncoding("wusb buffer too large"));
                }
                Ok(d.bytes(len)?.to_vec())
            }

            let mut d = Decoder::new(buf);

            let stalled = d.bool()?;
            let setup = UsbSetupPacket {
                request_type: d.u8()?,
                request: d.u8()?,
                value: d.u16()?,
                index: d.u16()?,
                length: d.u16()?,
            };

            let stage = match d.u8()? {
                0 => {
                    let data = dec_vec_u8_limited(&mut d, MAX_CONTROL_DATA_BYTES)?;
                    let offset = d.u32()? as usize;
                    if offset > data.len() {
                        return Err(SnapshotError::InvalidFieldEncoding("wusb indata offset"));
                    }
                    ControlStage::InData { data, offset }
                }
                1 => ControlStage::InDataPending,
                2 => {
                    let expected = d.u32()? as usize;
                    if expected > MAX_CONTROL_DATA_BYTES {
                        return Err(SnapshotError::InvalidFieldEncoding("wusb outdata len"));
                    }
                    let received = dec_vec_u8_limited(&mut d, MAX_CONTROL_DATA_BYTES)?;
                    if received.len() > expected {
                        return Err(SnapshotError::InvalidFieldEncoding("wusb outdata len"));
                    }
                    ControlStage::OutData { expected, received }
                }
                3 => ControlStage::StatusIn,
                4 => {
                    let has_data = d.bool()?;
                    let data = if has_data {
                        Some(dec_vec_u8_limited(&mut d, MAX_CONTROL_DATA_BYTES)?)
                    } else {
                        None
                    };
                    ControlStage::StatusInPending { data }
                }
                5 => ControlStage::StatusOut,
                6 => ControlStage::StatusOutPending,
                _ => return Err(SnapshotError::InvalidFieldEncoding("wusb stage")),
            };

            d.finish()?;

            self.control = Some(ControlState {
                setup,
                stage,
                stalled,
            });
        }

        Ok(())
    }
}

impl IoSnapshot for SharedUsbWebUsbPassthroughDevice {
    const DEVICE_ID: [u8; 4] = <UsbWebUsbPassthroughDevice as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion =
        <UsbWebUsbPassthroughDevice as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.0.borrow().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        self.0.borrow_mut().load_state(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::passthrough::{UsbHostCompletionIn, UsbHostCompletionOut};

    #[test]
    fn vendor_brequest_0x05_is_not_treated_as_set_address() {
        let mut dev = UsbWebUsbPassthroughDevice::new();

        // Vendor-specific Host->Device request that happens to use bRequest=0x05.
        // This must be forwarded to the host (not virtualized as SET_ADDRESS).
        let setup = UsbSetupPacket {
            request_type: 0x40,
            request: USB_REQUEST_SET_ADDRESS,
            value: 0x1234,
            index: 0,
            length: 0,
        };

        dev.handle_setup(setup);
        let actions = dev.drain_actions();
        assert_eq!(actions.len(), 1);
        let (id, host_setup, host_data) = match &actions[0] {
            UsbHostAction::ControlOut { id, setup, data } => (*id, *setup, data.as_slice()),
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(host_setup.bm_request_type, 0x40);
        assert_eq!(host_setup.b_request, USB_REQUEST_SET_ADDRESS);
        assert_eq!(host_setup.w_value, 0x1234);
        assert_eq!(host_setup.w_index, 0);
        assert_eq!(host_setup.w_length, 0);
        assert!(host_data.is_empty());

        // Poll status stage while pending.
        let mut buf = [0u8; 0];
        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Nak);

        // Complete the host transfer and ensure it does not apply a new USB address.
        dev.push_completion(UsbHostCompletion::ControlOut {
            id,
            result: UsbHostCompletionOut::Success { bytes_written: 0 },
        });
        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Ack { bytes: 0 });
        assert_eq!(dev.address(), 0);
    }

    #[test]
    fn new_setup_cancels_inflight_control_and_drops_queued_action() {
        let mut dev = UsbWebUsbPassthroughDevice::new();

        let setup1 = UsbSetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0100,
            index: 0,
            length: 4,
        };
        let setup2 = UsbSetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0200,
            index: 0,
            length: 4,
        };

        dev.handle_setup(setup1);
        let summary = dev.pending_summary();
        assert_eq!(summary.queued_actions, 1);
        let id1 = summary
            .inflight_control
            .expect("expected inflight control id");

        // Issuing a new SETUP must cancel the previous in-flight control transfer and drop its
        // queued host action (if it hasn't been drained yet).
        dev.handle_setup(setup2);
        let summary = dev.pending_summary();
        assert_eq!(summary.queued_actions, 1);
        let id2 = summary
            .inflight_control
            .expect("expected inflight control id");
        assert_ne!(id1, id2);

        let actions = dev.drain_actions();
        assert_eq!(actions.len(), 1, "stale action should be dropped");
        match &actions[0] {
            UsbHostAction::ControlIn { id, setup } => {
                assert_eq!(*id, id2);
                assert_eq!(
                    *setup,
                    HostSetupPacket {
                        bm_request_type: setup2.request_type,
                        b_request: setup2.request,
                        w_value: setup2.value,
                        w_index: setup2.index,
                        w_length: setup2.length,
                    }
                );
            }
            other => panic!("unexpected action: {other:?}"),
        }

        // Stale completion for the canceled id must be ignored.
        dev.push_completion(UsbHostCompletion::ControlIn {
            id: id1,
            result: UsbHostCompletionIn::Success {
                data: vec![1, 2, 3, 4],
            },
        });
        assert_eq!(dev.pending_summary().queued_completions, 0);

        // Completion for the active request should deliver data and complete the control transfer.
        dev.push_completion(UsbHostCompletion::ControlIn {
            id: id2,
            result: UsbHostCompletionIn::Success {
                data: vec![9, 8, 7, 6],
            },
        });

        let mut buf = [0u8; 4];
        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Ack { bytes: 4 });
        assert_eq!(buf, [9, 8, 7, 6]);

        // Status stage for control-IN is an OUT ZLP.
        assert_eq!(dev.handle_out(0, &[]), UsbHandshake::Ack { bytes: 0 });
        assert!(dev.drain_actions().is_empty());
    }

    #[test]
    fn control_in_stall_completion_halts_until_next_setup() {
        let mut dev = UsbWebUsbPassthroughDevice::new();

        let setup = UsbSetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0100,
            index: 0,
            length: 4,
        };

        dev.handle_setup(setup);
        let actions = dev.drain_actions();
        assert_eq!(actions.len(), 1);
        let id = match actions[0] {
            UsbHostAction::ControlIn { id, .. } => id,
            ref other => panic!("unexpected action: {other:?}"),
        };

        let mut buf = [0u8; 4];
        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Nak);

        dev.push_completion(UsbHostCompletion::ControlIn {
            id,
            result: UsbHostCompletionIn::Stall,
        });

        assert_eq!(dev.handle_in(0, &mut buf), UsbHandshake::Stall);
        // A stalled control endpoint should keep stalling until the next SETUP resets the pipe.
        assert_eq!(dev.handle_out(0, &[]), UsbHandshake::Stall);

        dev.handle_setup(setup);
        assert_eq!(dev.drain_actions().len(), 1);
    }
}
