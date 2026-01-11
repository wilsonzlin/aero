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
/// `web/src/usb/webusb_backend.ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsbHostAction {
    /// Control transfer, IN direction (device-to-host).
    ControlIn { id: u64, setup: SetupPacket },
    /// Control transfer, OUT direction (host-to-device).
    ControlOut {
        id: u64,
        setup: SetupPacket,
        data: Vec<u8>,
    },
    /// Bulk/interrupt transfer, IN direction.
    BulkIn { id: u64, endpoint: u8, length: u32 },
    /// Bulk/interrupt transfer, OUT direction.
    BulkOut { id: u64, endpoint: u8, data: Vec<u8> },
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
    Error { message: String },
}

/// Host-side completion pushed back into a [`UsbPassthroughDevice`].
///
/// See `UsbHostAction` for the canonical contract notes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsbHostCompletion {
    ControlIn {
        id: u64,
        #[serde(flatten)]
        result: UsbHostCompletionIn,
    },
    ControlOut {
        id: u64,
        #[serde(flatten)]
        result: UsbHostCompletionOut,
    },
    BulkIn {
        id: u64,
        #[serde(flatten)]
        result: UsbHostCompletionIn,
    },
    BulkOut {
        id: u64,
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
    fn from_completion(completion: UsbHostCompletion) -> (u64, Self) {
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
    id: u64,
    setup: SetupPacket,
    data: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct EpInflight {
    id: u64,
}

#[derive(Debug)]
pub struct UsbPassthroughDevice {
    next_id: u64,
    actions: VecDeque<UsbHostAction>,
    completions: HashMap<u64, UsbHostResult>,
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

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    fn is_inflight_id(&self, id: u64) -> bool {
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

    fn take_result(&mut self, id: u64) -> Option<UsbHostResult> {
        self.completions.remove(&id)
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
            if let Some(prev) = self.control_inflight.take() {
                self.completions.remove(&prev.id);
            }

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
        if let Some(inflight) = self.ep_inflight.get(&endpoint) {
            if let Some(result) = self.take_result(inflight.id) {
                self.ep_inflight.remove(&endpoint);
                return match result {
                    UsbHostResult::OkIn { mut data } => {
                        if data.len() > max_len {
                            data.truncate(max_len);
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
        self.ep_inflight.insert(endpoint, EpInflight { id });
        UsbInResult::Nak
    }

    pub fn handle_out_transfer(&mut self, endpoint: u8, data: &[u8]) -> UsbOutResult {
        if let Some(inflight) = self.ep_inflight.get(&endpoint) {
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
        self.ep_inflight.insert(endpoint, EpInflight { id });
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
    pub inflight_control: Option<u64>,
    pub inflight_endpoints: usize,
}

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
}

