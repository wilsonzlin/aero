use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use js_sys::{Array, Object, Reflect, Uint8Array};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupPacket {
    pub bm_request_type: u8,
    pub b_request: u8,
    pub w_value: u16,
    pub w_index: u16,
    pub w_length: u16,
}

impl SetupPacket {
    pub fn request_direction(&self) -> RequestDirection {
        if (self.bm_request_type & 0x80) != 0 {
            RequestDirection::DeviceToHost
        } else {
            RequestDirection::HostToDevice
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestDirection {
    HostToDevice,
    DeviceToHost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlResponse {
    Data(Vec<u8>),
    Ack,
    /// Indicates the request is still in-flight.
    Nak,
    Stall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbInResult {
    Data(Vec<u8>),
    Nak,
    Stall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbOutResult {
    Ack,
    Nak,
    Stall,
}

pub trait UsbDeviceModel {
    fn reset(&mut self) {}
    fn cancel_control_transfer(&mut self) {}

    fn handle_control_request(
        &mut self,
        setup: SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> ControlResponse;

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> UsbInResult;

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> UsbOutResult;
}

/// Host-side action emitted by a [`UsbPassthroughDevice`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbHostAction {
    /// Control transfer, IN direction (device-to-host).
    ///
    /// `len` is the requested length from `wLength`.
    ControlIn {
        id: u64,
        setup: SetupPacket,
        len: u16,
    },
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

impl UsbHostAction {
    fn id(&self) -> u64 {
        match self {
            UsbHostAction::ControlIn { id, .. } => *id,
            UsbHostAction::ControlOut { id, .. } => *id,
            UsbHostAction::BulkIn { id, .. } => *id,
            UsbHostAction::BulkOut { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbHostResult {
    OkIn { data: Vec<u8> },
    OkOut { bytes_written: usize },
    Stall,
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

    fn drop_queued_action(&mut self, id: u64) {
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
    fn reset(&mut self) {
        self.actions.clear();
        self.completions.clear();
        self.control_inflight = None;
        self.ep_inflight.clear();
    }

    fn cancel_control_transfer(&mut self) {
        self.cancel_inflight_control();
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
            self.cancel_inflight_control();

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
        self.actions
            .push_back(UsbHostAction::BulkIn { id, ep, len });
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

    pub fn reset(&self) {
        self.0.borrow_mut().reset();
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

impl RequestDirectionExt for RequestDirection {
    fn is_device_to_host(self) -> bool {
        matches!(self, RequestDirection::DeviceToHost)
    }
}

#[cfg(target_arch = "wasm32")]
fn setup_packet_to_js(setup: SetupPacket) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("bmRequestType"),
        &JsValue::from_f64(setup.bm_request_type as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("bRequest"),
        &JsValue::from_f64(setup.b_request as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wValue"),
        &JsValue::from_f64(setup.w_value as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wIndex"),
        &JsValue::from_f64(setup.w_index as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("wLength"),
        &JsValue::from_f64(setup.w_length as f64),
    );
    obj.into()
}

#[cfg(target_arch = "wasm32")]
fn host_action_to_js(action: UsbHostAction) -> JsValue {
    let obj = Object::new();
    match action {
        UsbHostAction::ControlIn { id, setup, .. } => {
            let _ = Reflect::set(&obj, &JsValue::from_str("kind"), &JsValue::from_str("controlIn"));
            let _ = Reflect::set(&obj, &JsValue::from_str("id"), &JsValue::from_f64(id as f64));
            let _ = Reflect::set(&obj, &JsValue::from_str("setup"), &setup_packet_to_js(setup));
        }
        UsbHostAction::ControlOut { id, setup, data } => {
            let _ =
                Reflect::set(&obj, &JsValue::from_str("kind"), &JsValue::from_str("controlOut"));
            let _ = Reflect::set(&obj, &JsValue::from_str("id"), &JsValue::from_f64(id as f64));
            let _ = Reflect::set(&obj, &JsValue::from_str("setup"), &setup_packet_to_js(setup));
            let bytes = Uint8Array::from(data.as_slice());
            let _ = Reflect::set(&obj, &JsValue::from_str("data"), &bytes);
        }
        UsbHostAction::BulkIn { id, ep, len } => {
            let _ = Reflect::set(&obj, &JsValue::from_str("kind"), &JsValue::from_str("bulkIn"));
            let _ = Reflect::set(&obj, &JsValue::from_str("id"), &JsValue::from_f64(id as f64));
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("endpoint"),
                &JsValue::from_f64(ep as f64),
            );
            let _ =
                Reflect::set(&obj, &JsValue::from_str("length"), &JsValue::from_f64(len as f64));
        }
        UsbHostAction::BulkOut { id, ep, data } => {
            let _ =
                Reflect::set(&obj, &JsValue::from_str("kind"), &JsValue::from_str("bulkOut"));
            let _ = Reflect::set(&obj, &JsValue::from_str("id"), &JsValue::from_f64(id as f64));
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("endpoint"),
                &JsValue::from_f64(ep as f64),
            );
            let bytes = Uint8Array::from(data.as_slice());
            let _ = Reflect::set(&obj, &JsValue::from_str("data"), &bytes);
        }
    }
    obj.into()
}

#[cfg(target_arch = "wasm32")]
fn parse_completion(js: JsValue) -> Option<UsbHostCompletion> {
    let kind = Reflect::get(&js, &JsValue::from_str("kind"))
        .ok()?
        .as_string()?;
    let status = Reflect::get(&js, &JsValue::from_str("status"))
        .ok()?
        .as_string()?;
    let id_val = Reflect::get(&js, &JsValue::from_str("id")).ok()?;
    let id_f = id_val.as_f64()?;
    if !id_f.is_finite() || id_f < 0.0 {
        return None;
    }
    let id = id_f as u64;

    let result = match status.as_str() {
        "success" => match kind.as_str() {
            "controlIn" | "bulkIn" => {
                let data_val = Reflect::get(&js, &JsValue::from_str("data")).ok()?;
                let arr = Uint8Array::new(&data_val);
                let mut data = vec![0u8; arr.length() as usize];
                arr.copy_to(&mut data);
                UsbHostResult::OkIn { data }
            }
            "controlOut" | "bulkOut" => {
                let bytes_val = Reflect::get(&js, &JsValue::from_str("bytesWritten")).ok()?;
                let bytes_f = bytes_val.as_f64()?;
                if !bytes_f.is_finite() || bytes_f < 0.0 {
                    return None;
                }
                UsbHostResult::OkOut {
                    bytes_written: bytes_f as usize,
                }
            }
            _ => return None,
        },
        "stall" => UsbHostResult::Stall,
        "error" => {
            let msg = Reflect::get(&js, &JsValue::from_str("message"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            UsbHostResult::Error(msg)
        }
        _ => return None,
    };

    Some(UsbHostCompletion::Completed { id, result })
}

#[cfg(target_arch = "wasm32")]
fn summary_to_js(summary: PendingSummary) -> JsValue {
    let obj = Object::new();
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("queuedActions"),
        &JsValue::from_f64(summary.queued_actions as f64),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("queuedCompletions"),
        &JsValue::from_f64(summary.queued_completions as f64),
    );
    let inflight = match summary.inflight_control {
        Some(id) => JsValue::from_f64(id as f64),
        None => JsValue::NULL,
    };
    let _ = Reflect::set(&obj, &JsValue::from_str("inflightControl"), &inflight);
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("inflightEndpoints"),
        &JsValue::from_f64(summary.inflight_endpoints as f64),
    );
    obj.into()
}

/// WASM-facing wrapper around [`UsbPassthroughHandle`] for the WebUSB broker.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct UsbPassthroughBridge {
    handle: UsbPassthroughHandle,
}

impl UsbPassthroughBridge {
    pub fn handle(&self) -> UsbPassthroughHandle {
        self.handle.clone()
    }

    pub fn pop_action_rust(&self) -> Option<UsbHostAction> {
        self.handle.pop_action()
    }

    pub fn push_completion_rust(&self, completion: UsbHostCompletion) {
        self.handle.push_completion(completion);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl UsbPassthroughBridge {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new() -> Self {
        Self {
            handle: UsbPassthroughHandle::new(),
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn pop_action(&self) -> JsValue {
        match self.handle.pop_action() {
            Some(action) => host_action_to_js(action),
            None => JsValue::NULL,
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn drain_actions(&self) -> JsValue {
        let actions = self.handle.drain_actions();
        let out = Array::new();
        for action in actions {
            out.push(&host_action_to_js(action));
        }
        out.into()
    }

    #[cfg(target_arch = "wasm32")]
    pub fn push_completion(&self, completion: JsValue) {
        let Some(parsed) = parse_completion(completion) else {
            return;
        };
        self.handle.push_completion(parsed);
    }

    #[cfg(target_arch = "wasm32")]
    pub fn reset(&self) {
        self.handle.reset();
    }

    #[cfg(target_arch = "wasm32")]
    pub fn pending_summary(&self) -> JsValue {
        summary_to_js(self.handle.pending_summary())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_drains_action_and_accepts_completion() {
        let bridge = UsbPassthroughBridge::new();

        let mut dev = bridge.handle();
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

        let action = bridge.pop_action_rust().expect("expected queued action");
        let id = match action {
            UsbHostAction::ControlIn {
                id,
                setup: action_setup,
                len,
            } => {
                assert_eq!(len, 4);
                assert_eq!(action_setup, setup);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };

        bridge.push_completion_rust(UsbHostCompletion::Completed {
            id,
            result: UsbHostResult::OkIn {
                data: vec![1, 2, 3, 4],
            },
        });

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Data(vec![1, 2, 3, 4])
        );
    }
}
