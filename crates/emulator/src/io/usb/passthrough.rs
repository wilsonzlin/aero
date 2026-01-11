use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

/// Host-side action emitted by a [`UsbPassthroughDevice`].
///
/// These actions are intentionally platform-agnostic so the host integration can be implemented
/// using WebUSB (Promise-based) or native libusb (async/worker thread) without pulling any of
/// those dependencies into `crates/emulator`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbHostAction {
    /// Control transfer, IN direction (device-to-host).
    ///
    /// `len` is the requested length from `wLength`.
    ControlIn { id: u64, setup: SetupPacket, len: u16 },
    /// Control transfer, OUT direction (host-to-device).
    ControlOut {
        id: u64,
        setup: SetupPacket,
        data: Vec<u8>,
    },
    /// Bulk/interrupt transfer, IN direction.
    BulkIn { id: u64, ep: u8, len: usize },
    /// Bulk/interrupt transfer, OUT direction.
    BulkOut { id: u64, ep: u8, data: Vec<u8> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbHostResult {
    OkIn { data: Vec<u8> },
    OkOut { bytes_written: usize },
    Stall,
    /// Host timed out waiting for the USB operation.
    ///
    /// The passthrough model maps this to a STALL to unblock the guest.
    Timeout,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbHostCompletion {
    Completed { id: u64, result: UsbHostResult },
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
        let UsbHostCompletion::Completed { id, result } = completion;
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
            UsbHostResult::Stall | UsbHostResult::Timeout | UsbHostResult::Error(_) => {
                ControlResponse::Stall
            }
            UsbHostResult::OkOut { .. } => ControlResponse::Stall,
        }
    }

    fn map_out_result(result: UsbHostResult) -> ControlResponse {
        match result {
            UsbHostResult::OkOut { .. } => ControlResponse::Ack,
            UsbHostResult::Stall | UsbHostResult::Timeout | UsbHostResult::Error(_) => {
                ControlResponse::Stall
            }
            UsbHostResult::OkIn { .. } => ControlResponse::Stall,
        }
    }
}

impl Default for UsbPassthroughDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbPassthroughDevice {
    fn get_device_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        &[]
    }

    fn reset(&mut self) {
        self.actions.clear();
        self.completions.clear();
        self.control_inflight = None;
        self.ep_inflight.clear();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        let req_dir_in = setup.request_direction().is_device_to_host();

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
                UsbHostAction::ControlIn {
                    id,
                    setup,
                    len: setup.w_length,
                }
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

    fn handle_in_transfer(&mut self, ep: u8, len: usize) -> UsbInResult {
        if let Some(inflight) = self.ep_inflight.get(&ep) {
            if let Some(result) = self.take_result(inflight.id) {
                self.ep_inflight.remove(&ep);
                return match result {
                    UsbHostResult::OkIn { data } => UsbInResult::Data(data),
                    UsbHostResult::Stall
                    | UsbHostResult::Timeout
                    | UsbHostResult::Error(_)
                    | UsbHostResult::OkOut { .. } => UsbInResult::Stall,
                };
            }
            return UsbInResult::Nak;
        }

        let id = self.alloc_id();
        self.actions.push_back(UsbHostAction::BulkIn { id, ep, len });
        self.ep_inflight.insert(ep, EpInflight { id });
        UsbInResult::Nak
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        if let Some(inflight) = self.ep_inflight.get(&ep) {
            if let Some(result) = self.take_result(inflight.id) {
                self.ep_inflight.remove(&ep);
                return match result {
                    UsbHostResult::OkOut { .. } => UsbOutResult::Ack,
                    UsbHostResult::Stall
                    | UsbHostResult::Timeout
                    | UsbHostResult::Error(_)
                    | UsbHostResult::OkIn { .. } => UsbOutResult::Stall,
                };
            }
            return UsbOutResult::Nak;
        }

        let id = self.alloc_id();
        self.actions.push_back(UsbHostAction::BulkOut {
            id,
            ep,
            data: data.to_vec(),
        });
        self.ep_inflight.insert(ep, EpInflight { id });
        UsbOutResult::Nak
    }
}

#[derive(Clone, Debug)]
pub struct UsbPassthroughHandle(Rc<RefCell<UsbPassthroughDevice>>);

impl UsbPassthroughHandle {
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
}

impl Default for UsbPassthroughHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl UsbDeviceModel for UsbPassthroughHandle {
    fn get_device_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        &[]
    }

    fn reset(&mut self) {
        self.0.borrow_mut().reset();
    }

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        self.0
            .borrow_mut()
            .handle_control_request(setup, data_stage)
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult {
        self.0.borrow_mut().handle_in_transfer(ep, max_len)
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult {
        self.0.borrow_mut().handle_out_transfer(ep, data)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSummary {
    pub queued_actions: usize,
    pub queued_completions: usize,
    pub inflight_control: Option<u64>,
    pub inflight_endpoints: usize,
}

trait RequestDirectionExt {
    fn is_device_to_host(self) -> bool;
}

impl RequestDirectionExt for crate::io::usb::RequestDirection {
    fn is_device_to_host(self) -> bool {
        matches!(self, crate::io::usb::RequestDirection::DeviceToHost)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_in_queues_once_then_returns_data() {
        let mut dev = UsbPassthroughHandle::new();
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
            UsbHostAction::ControlIn { id, setup, len } => {
                assert_eq!(len, 4);
                (id, setup)
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(action_setup, setup);

        // Poll again without completion: should still NAK and should not enqueue duplicates.
        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Nak
        );
        assert!(dev.pop_action().is_none());

        dev.push_completion(UsbHostCompletion::Completed {
            id,
            result: UsbHostResult::OkIn {
                data: vec![1, 2, 3, 4, 5],
            },
        });

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Data(vec![1, 2, 3, 4])
        );
    }

    #[test]
    fn control_out_includes_data_and_acks_on_completion() {
        let mut dev = UsbPassthroughHandle::new();
        let setup = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 0,
            w_index: 0,
            w_length: 3,
        };
        let payload = [0xaa, 0xbb, 0xcc];

        assert_eq!(
            dev.handle_control_request(setup, Some(&payload)),
            ControlResponse::Nak
        );

        let action = dev.pop_action().expect("expected queued action");
        let id = match action {
            UsbHostAction::ControlOut { id, setup: s, data } => {
                assert_eq!(s, setup);
                assert_eq!(data, payload);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };

        dev.push_completion(UsbHostCompletion::Completed {
            id,
            result: UsbHostResult::OkOut { bytes_written: 3 },
        });

        assert_eq!(
            dev.handle_control_request(setup, Some(&payload)),
            ControlResponse::Ack
        );
    }

    #[test]
    fn bulk_in_out_actions_are_not_duplicated_while_inflight() {
        let mut dev = UsbPassthroughHandle::new();

        // Bulk IN.
        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let action = dev.pop_action().expect("bulk in action");
        let id_in = match action {
            UsbHostAction::BulkIn { id, ep, len } => {
                assert_eq!(ep, 0x81);
                assert_eq!(len, 8);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        assert!(dev.pop_action().is_none(), "no duplicate action");

        dev.push_completion(UsbHostCompletion::Completed {
            id: id_in,
            result: UsbHostResult::OkIn {
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
            UsbHostAction::BulkOut { id, ep, data } => {
                assert_eq!(ep, 0x02);
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

        dev.push_completion(UsbHostCompletion::Completed {
            id: id_out,
            result: UsbHostResult::OkOut {
                bytes_written: out_payload.len(),
            },
        });
        assert_eq!(
            dev.handle_out_transfer(0x02, &out_payload),
            UsbOutResult::Ack
        );
    }

    #[test]
    fn reset_cancels_inflight_and_clears_action_queue() {
        let mut dev = UsbPassthroughHandle::new();

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
        dev.push_completion(UsbHostCompletion::Completed {
            id: id1,
            result: UsbHostResult::OkIn { data: vec![1] },
        });

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
    }
}
