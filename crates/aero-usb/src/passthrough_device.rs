//! UHCI-visible WebUSB passthrough USB device model.
//!
//! This adapts the asynchronous, host-driven [`crate::passthrough::UsbPassthroughDevice`] contract
//! (queued host actions + later completions) to the synchronous [`crate::UsbDeviceModel`] interface
//! consumed by [`crate::device::AttachedUsbDevice`] and the UHCI scheduler.
//!
//! While a host action is in-flight, transfers return NAK so the guest's TD remains active and is
//! retried in a later frame.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::{UsbInResult, UsbOutResult};
use crate::passthrough::{
    ControlResponse as HostControlResponse, PendingSummary, SetupPacket as HostSetupPacket,
    UsbHostAction, UsbHostCompletion, UsbInResult as HostUsbInResult,
    UsbOutResult as HostUsbOutResult, UsbPassthroughDevice,
};
use crate::{ControlResponse, SetupPacket, UsbDeviceModel};

/// Shareable handle for a WebUSB passthrough USB device model.
///
/// The UHCI root hub stores devices behind `Box<dyn UsbDeviceModel>`. By cloning this handle
/// before attaching, the host integration layer can continue to drain queued actions and push
/// completions.
#[derive(Clone, Debug)]
pub struct UsbWebUsbPassthroughDevice(Rc<RefCell<UsbPassthroughDevice>>);

impl UsbWebUsbPassthroughDevice {
    pub fn new() -> Self {
        Self(Rc::new(RefCell::new(UsbPassthroughDevice::new())))
    }

    pub fn pop_action(&self) -> Option<UsbHostAction> {
        self.0.borrow_mut().pop_action()
    }

    pub fn drain_actions(&self) -> Vec<UsbHostAction> {
        self.0.borrow_mut().drain_actions()
    }

    pub fn push_completion(&self, completion: UsbHostCompletion) {
        self.0.borrow_mut().push_completion(completion);
    }

    pub fn pending_summary(&self) -> PendingSummary {
        self.0.borrow().pending_summary()
    }

    pub fn reset(&self) {
        self.0.borrow_mut().reset();
    }

    /// Clears host-side WebUSB bookkeeping without changing guest-visible USB state.
    ///
    /// WebUSB host actions are backed by JS Promises and cannot be resumed after restoring a VM
    /// snapshot. Dropping queued actions, completions, and in-flight maps ensures the guest's UHCI
    /// TD retries will re-emit host actions instead of deadlocking on a completion that will never
    /// arrive.
    pub fn reset_host_state_for_restore(&self) {
        self.0.borrow_mut().reset();
    }

    fn to_host_setup(setup: SetupPacket) -> HostSetupPacket {
        HostSetupPacket {
            bm_request_type: setup.bm_request_type,
            b_request: setup.b_request,
            w_value: setup.w_value,
            w_index: setup.w_index,
            w_length: setup.w_length,
        }
    }

    fn map_control_response(resp: HostControlResponse) -> ControlResponse {
        match resp {
            HostControlResponse::Data(data) => ControlResponse::Data(data),
            HostControlResponse::Ack => ControlResponse::Ack,
            HostControlResponse::Nak => ControlResponse::Nak,
            HostControlResponse::Stall => ControlResponse::Stall,
            HostControlResponse::Timeout => ControlResponse::Timeout,
        }
    }

    fn map_in_result(resp: HostUsbInResult, max_len: usize) -> UsbInResult {
        match resp {
            HostUsbInResult::Data(mut data) => {
                if data.len() > max_len {
                    data.truncate(max_len);
                }
                UsbInResult::Data(data)
            }
            HostUsbInResult::Nak => UsbInResult::Nak,
            HostUsbInResult::Stall => UsbInResult::Stall,
            HostUsbInResult::Timeout => UsbInResult::Timeout,
        }
    }

    fn map_out_result(resp: HostUsbOutResult) -> UsbOutResult {
        match resp {
            HostUsbOutResult::Ack => UsbOutResult::Ack,
            HostUsbOutResult::Nak => UsbOutResult::Nak,
            HostUsbOutResult::Stall => UsbOutResult::Stall,
            HostUsbOutResult::Timeout => UsbOutResult::Timeout,
        }
    }
}

impl Default for UsbWebUsbPassthroughDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbWebUsbPassthroughDevice {
    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn cancel_control_transfer(&mut self) {
        self.0.borrow_mut().cancel_control_transfer();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        let resp = self
            .0
            .borrow_mut()
            .handle_control_request(Self::to_host_setup(setup), data_stage);
        Self::map_control_response(resp)
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        let resp = self.0.borrow_mut().handle_in_transfer(ep, max_len);
        Self::map_in_result(resp, max_len)
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        let resp = self.0.borrow_mut().handle_out_transfer(ep, data);
        Self::map_out_result(resp)
    }
}

impl IoSnapshot for UsbWebUsbPassthroughDevice {
    const DEVICE_ID: [u8; 4] = *b"WUSB";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        // Backwards compatibility:
        // - v1.0 used TAG_PASSTHROUGH_V1_0 (=1) and stored only `UsbPassthroughDevice::save_state`.
        // - v1.1 stores both the minimal guest-visible state and a deterministic snapshot of the
        //   full host-action queues so WASM bridges can roundtrip snapshots without losing queued
        //   actions (host integrations may still want to clear host state after restore).
        const TAG_PASSTHROUGH_V1_0: u16 = 1;
        const TAG_PASSTHROUGH: u16 = 4;
        const TAG_PASSTHROUGH_FULL: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        let inner = self.0.borrow();
        w.field_bytes(TAG_PASSTHROUGH_V1_0, inner.save_state());
        w.field_bytes(TAG_PASSTHROUGH, inner.save_state());
        w.field_bytes(TAG_PASSTHROUGH_FULL, inner.snapshot_save());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PASSTHROUGH_V1_0: u16 = 1;
        const TAG_PASSTHROUGH: u16 = 4;
        const TAG_PASSTHROUGH_FULL: u16 = 5;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let mut inner = self.0.borrow_mut();
        inner.reset();

        if let Some(buf) = r.bytes(TAG_PASSTHROUGH_FULL) {
            inner.snapshot_load(buf)?;
        } else if let Some(buf) = r.bytes(TAG_PASSTHROUGH).or_else(|| r.bytes(TAG_PASSTHROUGH_V1_0))
        {
            // Minimal snapshot (next_id only).
            inner.load_state(buf)?;
        }

        Ok(())
    }
}
