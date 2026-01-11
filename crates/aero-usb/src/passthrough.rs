//! USB passthrough bridge state machine.
//!
//! This module implements the Rust-side half of the WebUSB passthrough contract described in
//! `docs/webusb-passthrough.md`.
//!
//! The device model queues host actions (`UsbHostAction`) when the guest attempts a USB
//! transfer, and consumes host completions (`UsbHostCompletion`) to finish those transfers.
//! While an action is in-flight, repeated attempts return NAK without emitting duplicate
//! host actions.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupPacket {
    #[serde(rename = "bmRequestType")]
    pub bm_request_type: u8,
    #[serde(rename = "bRequest")]
    pub b_request: u8,
    #[serde(rename = "wValue")]
    pub w_value: u16,
    #[serde(rename = "wIndex")]
    pub w_index: u16,
    #[serde(rename = "wLength")]
    pub w_length: u16,
}

impl SetupPacket {
    fn is_device_to_host(self) -> bool {
        (self.bm_request_type & 0x80) != 0
    }
}

/// Host-side action emitted by a [`UsbPassthroughDevice`].
///
/// This is the canonical wire representation shared with TypeScript:
/// `web/src/usb/usb_passthrough_types.ts` (re-exported from `web/src/usb/webusb_backend.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsbHostAction {
    /// Control transfer, IN direction (device-to-host).
    ControlIn { id: u32, setup: SetupPacket },
    /// Control transfer, OUT direction (host-to-device).
    ControlOut {
        id: u32,
        setup: SetupPacket,
        data: Vec<u8>,
    },
    /// Bulk/interrupt transfer, IN direction.
    BulkIn { id: u32, endpoint: u8, length: u32 },
    /// Bulk/interrupt transfer, OUT direction.
    BulkOut {
        id: u32,
        endpoint: u8,
        data: Vec<u8>,
    },
}

impl UsbHostAction {
    fn id(&self) -> u32 {
        match self {
            UsbHostAction::ControlIn { id, .. } => *id,
            UsbHostAction::ControlOut { id, .. } => *id,
            UsbHostAction::BulkIn { id, .. } => *id,
            UsbHostAction::BulkOut { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum UsbHostCompletionIn {
    Success { data: Vec<u8> },
    Stall,
    Error { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum UsbHostCompletionOut {
    Success {
        #[serde(rename = "bytesWritten")]
        bytes_written: u32,
    },
    Stall,
    Error {
        message: String,
    },
}

/// Host-side completion pushed back into a [`UsbPassthroughDevice`].
///
/// See `UsbHostAction` for the canonical contract notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsbHostCompletion {
    ControlIn {
        id: u32,
        #[serde(flatten)]
        result: UsbHostCompletionIn,
    },
    ControlOut {
        id: u32,
        #[serde(flatten)]
        result: UsbHostCompletionOut,
    },
    BulkIn {
        id: u32,
        #[serde(flatten)]
        result: UsbHostCompletionIn,
    },
    BulkOut {
        id: u32,
        #[serde(flatten)]
        result: UsbHostCompletionOut,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UsbHostResult {
    OkIn { data: Vec<u8> },
    OkOut { bytes_written: usize },
    Stall,
    Error(String),
}

impl UsbHostResult {
    fn from_completion(completion: UsbHostCompletion) -> (u32, Self) {
        match completion {
            UsbHostCompletion::ControlIn { id, result }
            | UsbHostCompletion::BulkIn { id, result } => {
                let mapped = match result {
                    UsbHostCompletionIn::Success { data } => UsbHostResult::OkIn { data },
                    UsbHostCompletionIn::Stall => UsbHostResult::Stall,
                    UsbHostCompletionIn::Error { message } => UsbHostResult::Error(message),
                };
                (id, mapped)
            }
            UsbHostCompletion::ControlOut { id, result }
            | UsbHostCompletion::BulkOut { id, result } => {
                let mapped = match result {
                    UsbHostCompletionOut::Success { bytes_written } => UsbHostResult::OkOut {
                        bytes_written: bytes_written as usize,
                    },
                    UsbHostCompletionOut::Stall => UsbHostResult::Stall,
                    UsbHostCompletionOut::Error { message } => UsbHostResult::Error(message),
                };
                (id, mapped)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlResponse {
    Data(Vec<u8>),
    Ack,
    Nak,
    Stall,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbInResult {
    Data(Vec<u8>),
    Nak,
    Stall,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbOutResult {
    Ack,
    Nak,
    Stall,
    Timeout,
}

#[derive(Debug, Clone)]
struct ControlInflight {
    id: u32,
    setup: SetupPacket,
    data: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct EpInflight {
    id: u32,
    len: usize,
}

#[derive(Debug)]
pub struct UsbPassthroughDevice {
    // `id` is part of the Rust<->TypeScript wire contract and must be representable
    // as a JS number (avoid `u64` here; `serde-wasm-bindgen` would surface it as
    // a `bigint`).
    next_id: u32,
    actions: VecDeque<UsbHostAction>,
    completions: HashMap<u32, UsbHostResult>,
    control_inflight: Option<ControlInflight>,
    ep_inflight: HashMap<u8, EpInflight>,
}

impl UsbPassthroughDevice {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            actions: VecDeque::new(),
            completions: HashMap::new(),
            control_inflight: None,
            ep_inflight: HashMap::new(),
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn is_inflight_id(&self, id: u32) -> bool {
        if self
            .control_inflight
            .as_ref()
            .is_some_and(|ctl| ctl.id == id)
        {
            return true;
        }
        self.ep_inflight.values().any(|ep| ep.id == id)
    }

    pub fn pop_action(&mut self) -> Option<UsbHostAction> {
        self.actions.pop_front()
    }

    pub fn drain_actions(&mut self) -> Vec<UsbHostAction> {
        self.actions.drain(..).collect()
    }

    pub fn push_completion(&mut self, completion: UsbHostCompletion) {
        let (id, result) = UsbHostResult::from_completion(completion);
        if !self.is_inflight_id(id) {
            // Stale completion for an already-finished/canceled transfer.
            return;
        }
        self.completions.insert(id, result);
    }

    pub fn pending_summary(&self) -> PendingSummary {
        PendingSummary {
            queued_actions: self.actions.len(),
            queued_completions: self.completions.len(),
            inflight_control: self.control_inflight.as_ref().map(|c| c.id),
            inflight_endpoints: self.ep_inflight.len(),
        }
    }

    pub fn reset(&mut self) {
        self.actions.clear();
        self.completions.clear();
        self.control_inflight = None;
        self.ep_inflight.clear();
    }

    /// Cancel any in-flight control transfer.
    ///
    /// This matches USB control-pipe semantics: a new SETUP packet may legally abort a previous
    /// control transfer. Host integrations should ignore any eventual completion for the canceled
    /// request.
    pub fn cancel_control_transfer(&mut self) {
        self.cancel_inflight_control();
    }

    fn take_result(&mut self, id: u32) -> Option<UsbHostResult> {
        self.completions.remove(&id)
    }

    fn drop_queued_action(&mut self, id: u32) {
        self.actions.retain(|action| action.id() != id);
    }

    fn cancel_inflight_control(&mut self) {
        if let Some(prev) = self.control_inflight.take() {
            self.completions.remove(&prev.id);
            // If the host has not dequeued the old action yet, drop it so we do not execute a stale
            // control transfer after the guest has already moved on.
            self.drop_queued_action(prev.id);
        }
    }

    fn map_in_result(setup: SetupPacket, result: UsbHostResult) -> ControlResponse {
        match result {
            UsbHostResult::OkIn { mut data } => {
                let requested = setup.w_length as usize;
                if data.len() > requested {
                    data.truncate(requested);
                }
                ControlResponse::Data(data)
            }
            UsbHostResult::Stall => ControlResponse::Stall,
            UsbHostResult::Error(_) | UsbHostResult::OkOut { .. } => ControlResponse::Timeout,
        }
    }

    fn map_out_result(result: UsbHostResult) -> ControlResponse {
        match result {
            UsbHostResult::OkOut { .. } => ControlResponse::Ack,
            UsbHostResult::Stall => ControlResponse::Stall,
            UsbHostResult::Error(_) | UsbHostResult::OkIn { .. } => ControlResponse::Timeout,
        }
    }

    pub fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        let req_dir_in = setup.is_device_to_host();

        let same_as_inflight = self.control_inflight.as_ref().is_some_and(|ctl| {
            if ctl.setup != setup {
                return false;
            }
            match (&ctl.data, data_stage) {
                (None, None) => true,
                (Some(buf), Some(data)) => buf.as_slice() == data,
                _ => false,
            }
        });

        if !same_as_inflight {
            // New SETUP while an older request is pending: abandon it.
            self.cancel_inflight_control();

            let id = self.alloc_id();
            let action = if req_dir_in {
                UsbHostAction::ControlIn { id, setup }
            } else {
                UsbHostAction::ControlOut {
                    id,
                    setup,
                    data: data_stage.unwrap_or_default().to_vec(),
                }
            };
            self.actions.push_back(action);
            self.control_inflight = Some(ControlInflight {
                id,
                setup,
                data: data_stage.map(|d| d.to_vec()),
            });
            return ControlResponse::Nak;
        }

        let Some(inflight) = self.control_inflight.as_ref() else {
            return ControlResponse::Nak;
        };

        let Some(result) = self.take_result(inflight.id) else {
            return ControlResponse::Nak;
        };

        // Request finished (success or failure).
        let inflight = self.control_inflight.take().expect("inflight exists");

        if req_dir_in {
            Self::map_in_result(inflight.setup, result)
        } else {
            Self::map_out_result(result)
        }
    }

    pub fn handle_in_transfer(&mut self, endpoint: u8, max_len: usize) -> UsbInResult {
        debug_assert!(
            (endpoint & 0x80) != 0,
            "handle_in_transfer expects an IN endpoint address (bit7=1), got {endpoint:#04x}"
        );
        debug_assert!(
            (endpoint & 0x70) == 0,
            "handle_in_transfer expects a valid endpoint number (0..=15), got {endpoint:#04x}"
        );
        debug_assert!(
            (endpoint & 0x0f) != 0,
            "handle_in_transfer should not be used for control endpoint 0, got {endpoint:#04x}"
        );
        if let Some(inflight) = self.ep_inflight.get(&endpoint) {
            debug_assert_eq!(
                inflight.len,
                max_len,
                "handle_in_transfer called with max_len={max_len} for endpoint {endpoint:#04x}, but existing in-flight request expects len={}",
                inflight.len
            );
            let inflight_id = inflight.id;
            let inflight_len = inflight.len;
            if let Some(result) = self.take_result(inflight_id) {
                self.ep_inflight.remove(&endpoint);
                return match result {
                    UsbHostResult::OkIn { mut data } => {
                        if data.len() > inflight_len {
                            data.truncate(inflight_len);
                        }
                        UsbInResult::Data(data)
                    }
                    UsbHostResult::Stall => UsbInResult::Stall,
                    UsbHostResult::Error(_) | UsbHostResult::OkOut { .. } => UsbInResult::Timeout,
                };
            }
            return UsbInResult::Nak;
        }

        let id = self.alloc_id();
        self.actions.push_back(UsbHostAction::BulkIn {
            id,
            endpoint,
            length: max_len as u32,
        });
        self.ep_inflight
            .insert(endpoint, EpInflight { id, len: max_len });
        UsbInResult::Nak
    }

    pub fn handle_out_transfer(&mut self, endpoint: u8, data: &[u8]) -> UsbOutResult {
        debug_assert!(
            (endpoint & 0x80) == 0,
            "handle_out_transfer expects an OUT endpoint address (bit7=0), got {endpoint:#04x}"
        );
        debug_assert!(
            (endpoint & 0x70) == 0,
            "handle_out_transfer expects a valid endpoint number (0..=15), got {endpoint:#04x}"
        );
        debug_assert!(
            (endpoint & 0x0f) != 0,
            "handle_out_transfer should not be used for control endpoint 0, got {endpoint:#04x}"
        );
        if let Some(inflight) = self.ep_inflight.get(&endpoint) {
            debug_assert_eq!(
                inflight.len,
                data.len(),
                "handle_out_transfer called with len={} for endpoint {endpoint:#04x}, but existing in-flight request expects len={}",
                data.len(),
                inflight.len
            );
            if let Some(result) = self.take_result(inflight.id) {
                self.ep_inflight.remove(&endpoint);
                return match result {
                    UsbHostResult::OkOut { .. } => UsbOutResult::Ack,
                    UsbHostResult::Stall => UsbOutResult::Stall,
                    UsbHostResult::Error(_) | UsbHostResult::OkIn { .. } => UsbOutResult::Timeout,
                };
            }
            return UsbOutResult::Nak;
        }

        let id = self.alloc_id();
        self.actions.push_back(UsbHostAction::BulkOut {
            id,
            endpoint,
            data: data.to_vec(),
        });
        self.ep_inflight.insert(
            endpoint,
            EpInflight {
                id,
                len: data.len(),
            },
        );
        UsbOutResult::Nak
    }
}

impl Default for UsbPassthroughDevice {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSummary {
    pub queued_actions: usize,
    pub queued_completions: usize,
    pub inflight_control: Option<u32>,
    pub inflight_endpoints: usize,
}

// Backwards-compatible re-export: the UHCI-visible UsbDevice adapter lives in its own module.
pub use crate::passthrough_device::UsbWebUsbPassthroughDevice;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct WireFixture {
        actions: Vec<UsbHostAction>,
        completions: Vec<UsbHostCompletion>,
    }

    const WIRE_FIXTURE_STR: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/fixtures/webusb_passthrough_wire.json"
    ));

    #[test]
    fn wire_fixture_matches_serde_shape() {
        let fixture_value: serde_json::Value = serde_json::from_str(WIRE_FIXTURE_STR).unwrap();
        let fixture: WireFixture = serde_json::from_value(fixture_value.clone()).unwrap();

        assert_eq!(
            serde_json::to_value(&fixture.actions).unwrap(),
            fixture_value["actions"]
        );
        assert_eq!(
            serde_json::to_value(&fixture.completions).unwrap(),
            fixture_value["completions"]
        );
    }

    #[test]
    fn control_in_queues_once_then_returns_data() {
        let mut dev = UsbPassthroughDevice::new();
        let setup = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 4,
        };

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Nak
        );

        let action = dev.pop_action().expect("expected queued action");
        let (id, action_setup) = match action {
            UsbHostAction::ControlIn { id, setup } => (id, setup),
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(action_setup, setup);

        // Poll again without completion: should still NAK and should not enqueue duplicates.
        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Nak
        );
        assert!(dev.pop_action().is_none());

        dev.push_completion(UsbHostCompletion::ControlIn {
            id,
            result: UsbHostCompletionIn::Success {
                data: vec![1, 2, 3, 4, 5],
            },
        });

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Data(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn bulk_in_out_actions_are_not_duplicated_while_inflight() {
        let mut dev = UsbPassthroughDevice::new();

        // Bulk IN.
        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let action = dev.pop_action().expect("bulk in action");
        let id_in = match action {
            UsbHostAction::BulkIn {
                id,
                endpoint,
                length,
            } => {
                assert_eq!(endpoint, 0x81);
                assert_eq!(length, 8);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        assert!(dev.pop_action().is_none(), "no duplicate action");

        dev.push_completion(UsbHostCompletion::BulkIn {
            id: id_in,
            result: UsbHostCompletionIn::Success {
                data: vec![0x11, 0x22],
            },
        });
        assert_eq!(
            dev.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![0x11, 0x22])
        );

        // Bulk OUT.
        let out_payload = [9u8, 8, 7, 6];
        assert_eq!(
            dev.handle_out_transfer(0x02, &out_payload),
            UsbOutResult::Nak
        );
        let action = dev.pop_action().expect("bulk out action");
        let id_out = match action {
            UsbHostAction::BulkOut { id, endpoint, data } => {
                assert_eq!(endpoint, 0x02);
                assert_eq!(data, out_payload);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(
            dev.handle_out_transfer(0x02, &out_payload),
            UsbOutResult::Nak
        );
        assert!(dev.pop_action().is_none(), "no duplicate action");

        dev.push_completion(UsbHostCompletion::BulkOut {
            id: id_out,
            result: UsbHostCompletionOut::Success {
                bytes_written: out_payload.len() as u32,
            },
        });
        assert_eq!(
            dev.handle_out_transfer(0x02, &out_payload),
            UsbOutResult::Ack
        );
    }

    #[test]
    fn error_completion_maps_to_timeout() {
        let mut dev = UsbPassthroughDevice::new();

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let id = match dev.pop_action().unwrap() {
            UsbHostAction::BulkIn { id, .. } => id,
            other => panic!("unexpected action: {other:?}"),
        };

        dev.push_completion(UsbHostCompletion::BulkIn {
            id,
            result: UsbHostCompletionIn::Error {
                message: "boom".to_string(),
            },
        });

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Timeout);
    }

    #[test]
    fn reset_cancels_inflight_and_clears_action_queue() {
        let mut dev = UsbPassthroughDevice::new();

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let id1 = match dev.pop_action().unwrap() {
            UsbHostAction::BulkIn { id, .. } => id,
            other => panic!("unexpected action: {other:?}"),
        };

        dev.reset();
        assert!(dev.pop_action().is_none(), "reset clears action queue");

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let id2 = match dev.pop_action().unwrap() {
            UsbHostAction::BulkIn { id, .. } => id,
            other => panic!("unexpected action: {other:?}"),
        };
        assert_ne!(id1, id2);

        // Stale completion for the canceled transfer should be ignored.
        dev.push_completion(UsbHostCompletion::BulkIn {
            id: id1,
            result: UsbHostCompletionIn::Success { data: vec![1] },
        });

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }

    #[test]
    fn bulk_in_completion_is_truncated_to_requested_len() {
        let mut dev = UsbPassthroughDevice::new();

        assert_eq!(dev.handle_in_transfer(0x81, 2), UsbInResult::Nak);
        let id = match dev.pop_action().expect("expected BulkIn action") {
            UsbHostAction::BulkIn {
                id,
                endpoint,
                length,
            } => {
                assert_eq!(endpoint, 0x81);
                assert_eq!(length, 2);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };

        // UHCI may retry while providing a different `max_len`; we must still truncate to the
        // original requested length.
        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        assert!(dev.pop_action().is_none(), "no duplicate action");

        dev.push_completion(UsbHostCompletion::BulkIn {
            id,
            result: UsbHostCompletionIn::Success {
                data: vec![1, 2, 3, 4],
            },
        });

        assert_eq!(
            dev.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![1, 2])
        );
    }

    #[test]
    fn new_setup_cancels_previous_inflight_and_drops_queued_action() {
        let mut dev = UsbPassthroughDevice::new();

        let setup1 = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 4,
        };
        let setup2 = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 4,
        };

        assert_eq!(
            dev.handle_control_request(setup1, None),
            ControlResponse::Nak
        );
        let id1 = dev.pending_summary().inflight_control.expect("inflight id");

        assert_eq!(
            dev.handle_control_request(setup2, None),
            ControlResponse::Nak
        );
        let id2 = dev.pending_summary().inflight_control.expect("inflight id");
        assert_ne!(id1, id2);

        // Only the newer request should remain queued for the host.
        match dev.pop_action().expect("expected action") {
            UsbHostAction::ControlIn { id, setup } => {
                assert_eq!(id, id2);
                assert_eq!(setup, setup2);
            }
            other => panic!("unexpected action: {other:?}"),
        }
        assert!(dev.pop_action().is_none(), "stale action should be dropped");

        // Stale completion must be ignored.
        dev.push_completion(UsbHostCompletion::ControlIn {
            id: id1,
            result: UsbHostCompletionIn::Success {
                data: vec![1, 2, 3],
            },
        });
        assert_eq!(dev.pending_summary().queued_completions, 0);

        dev.push_completion(UsbHostCompletion::ControlIn {
            id: id2,
            result: UsbHostCompletionIn::Success {
                data: vec![9, 8, 7, 6],
            },
        });

        assert_eq!(
            dev.handle_control_request(setup2, None),
            ControlResponse::Data(vec![9, 8, 7, 6])
        );
    }
}
