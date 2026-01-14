//! USB passthrough bridge state machine.
//!
//! This module implements the Rust-side half of the WebUSB passthrough contract described in
//! `docs/webusb-passthrough.md`.
//! For canonical stack selection and deprecation of parallel USB stacks, see
//! `docs/adr/0015-canonical-usb-stack.md`.
//!
//! The device model queues host actions (`UsbHostAction`) when the guest attempts a USB
//! transfer, and consumes host completions (`UsbHostCompletion`) to finish those transfers.
//! While an action is in-flight, repeated attempts return NAK without emitting duplicate
//! host actions.

use std::collections::{HashMap, HashSet, VecDeque};

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
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
        // Allocate a new non-zero transfer ID, skipping any IDs that are currently in-flight.
        //
        // `id` is part of the Rust<->TypeScript wire contract and must fit in a JS number, so we
        // use `u32` and wrap on overflow. Skipping in-flight IDs ensures that a wrap-around cannot
        // collide with transfers that are still pending (the in-flight set is small, so this loop
        // is bounded).
        loop {
            let id = self.next_id.max(1);
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.is_inflight_id(id) {
                return id;
            }
        }
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

    pub(crate) fn snapshot_save(&self) -> Vec<u8> {
        fn enc_setup(enc: Encoder, setup: SetupPacket) -> Encoder {
            enc.u8(setup.bm_request_type)
                .u8(setup.b_request)
                .u16(setup.w_value)
                .u16(setup.w_index)
                .u16(setup.w_length)
        }

        fn enc_action(enc: Encoder, action: &UsbHostAction) -> Encoder {
            match action {
                UsbHostAction::ControlIn { id, setup } => enc_setup(enc.u8(1).u32(*id), *setup),
                UsbHostAction::ControlOut { id, setup, data } => {
                    let enc = enc_setup(enc.u8(2).u32(*id), *setup).u32(data.len() as u32);
                    enc.bytes(data)
                }
                UsbHostAction::BulkIn {
                    id,
                    endpoint,
                    length,
                } => enc.u8(3).u32(*id).u8(*endpoint).u32(*length),
                UsbHostAction::BulkOut { id, endpoint, data } => enc
                    .u8(4)
                    .u32(*id)
                    .u8(*endpoint)
                    .u32(data.len() as u32)
                    .bytes(data),
            }
        }

        fn enc_result(enc: Encoder, result: &UsbHostResult) -> Encoder {
            match result {
                UsbHostResult::OkIn { data } => enc.u8(1).u32(data.len() as u32).bytes(data),
                UsbHostResult::OkOut { bytes_written } => enc.u8(2).u32(*bytes_written as u32),
                UsbHostResult::Stall => enc.u8(3),
                UsbHostResult::Error(msg) => enc.u8(4).u32(msg.len() as u32).bytes(msg.as_bytes()),
            }
        }

        // Deterministic encoding: all HashMap-backed collections are sorted.
        let mut enc = Encoder::new()
            .u32(self.next_id)
            .u32(self.actions.len() as u32);
        for action in &self.actions {
            enc = enc_action(enc, action);
        }

        let mut completion_ids: Vec<u32> = self.completions.keys().copied().collect();
        completion_ids.sort_unstable();
        enc = enc.u32(completion_ids.len() as u32);
        for id in completion_ids {
            let result = self.completions.get(&id).expect("completion id exists");
            enc = enc.u32(id);
            enc = enc_result(enc, result);
        }

        if let Some(ctl) = self.control_inflight.as_ref() {
            enc = enc.bool(true);
            enc = enc.u32(ctl.id);
            enc = enc_setup(enc, ctl.setup);
            match ctl.data.as_ref() {
                Some(data) => enc = enc.bool(true).u32(data.len() as u32).bytes(data),
                None => enc = enc.bool(false),
            }
        } else {
            enc = enc.bool(false);
        }

        let mut eps: Vec<(u8, &EpInflight)> =
            self.ep_inflight.iter().map(|(&k, v)| (k, v)).collect();
        eps.sort_by_key(|(ep, _)| *ep);
        enc = enc.u32(eps.len() as u32);
        for (endpoint, inflight) in eps {
            enc = enc.u8(endpoint).u32(inflight.id).u32(inflight.len as u32);
        }

        enc.finish()
    }

    pub(crate) fn snapshot_load(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const MAX_ACTIONS: usize = 1024;
        const MAX_COMPLETIONS: usize = 1024;
        const MAX_EP_INFLIGHT: usize = 64;
        const MAX_DATA_BYTES: usize = 4 * 1024 * 1024; // 4MiB
        const MAX_ERROR_BYTES: usize = 16 * 1024;
        const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024; // 16MiB

        fn dec_setup(d: &mut Decoder<'_>) -> SnapshotResult<SetupPacket> {
            Ok(SetupPacket {
                bm_request_type: d.u8()?,
                b_request: d.u8()?,
                w_value: d.u16()?,
                w_index: d.u16()?,
                w_length: d.u16()?,
            })
        }

        fn dec_bytes_limited(
            d: &mut Decoder<'_>,
            max: usize,
            total: &mut usize,
            max_total: usize,
        ) -> SnapshotResult<Vec<u8>> {
            let len = d.u32()? as usize;
            if len > max {
                return Err(SnapshotError::InvalidFieldEncoding("buffer too large"));
            }
            let next_total = total
                .checked_add(len)
                .ok_or(SnapshotError::InvalidFieldEncoding("buffer too large"))?;
            if next_total > max_total {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "snapshot buffers too large",
                ));
            }
            *total = next_total;
            Ok(d.bytes(len)?.to_vec())
        }

        let mut d = Decoder::new(bytes);

        self.next_id = d.u32()?;
        if self.next_id == 0 {
            return Err(SnapshotError::InvalidFieldEncoding("next_id must be non-zero"));
        }
        let mut total_bytes = 0usize;

        let mut action_ids = HashSet::<u32>::new();
        self.actions.clear();
        let action_count = d.u32()? as usize;
        if action_count > MAX_ACTIONS {
            return Err(SnapshotError::InvalidFieldEncoding(
                "too many queued actions",
            ));
        }
        for _ in 0..action_count {
            let kind = d.u8()?;
            let id = d.u32()?;
            if id == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("action id must be non-zero"));
            }
            if !action_ids.insert(id) {
                return Err(SnapshotError::InvalidFieldEncoding("duplicate action id"));
            }
            let action = match kind {
                1 => {
                    let setup = dec_setup(&mut d)?;
                    if !setup.is_device_to_host() {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "controlIn setup must be device-to-host",
                        ));
                    }
                    UsbHostAction::ControlIn { id, setup }
                }
                2 => {
                    let setup = dec_setup(&mut d)?;
                    if setup.is_device_to_host() {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "controlOut setup must be host-to-device",
                        ));
                    }
                    let data = dec_bytes_limited(
                        &mut d,
                        MAX_DATA_BYTES,
                        &mut total_bytes,
                        MAX_TOTAL_BYTES,
                    )?;
                    if data.len() != setup.w_length as usize {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "controlOut data length mismatch",
                        ));
                    }
                    UsbHostAction::ControlOut { id, setup, data }
                }
                3 => {
                    let endpoint = d.u8()?;
                    if (endpoint & 0x80) == 0 {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "bulkIn endpoint must be IN",
                        ));
                    }
                    if (endpoint & 0x0f) == 0 || (endpoint & 0x70) != 0 {
                        return Err(SnapshotError::InvalidFieldEncoding("invalid endpoint address"));
                    }
                    let length = d.u32()?;
                    if length as usize > MAX_DATA_BYTES {
                        return Err(SnapshotError::InvalidFieldEncoding("bulkIn length too large"));
                    }
                    UsbHostAction::BulkIn {
                        id,
                        endpoint,
                        length,
                    }
                }
                4 => {
                    let endpoint = d.u8()?;
                    if (endpoint & 0x80) != 0 {
                        return Err(SnapshotError::InvalidFieldEncoding(
                            "bulkOut endpoint must be OUT",
                        ));
                    }
                    if (endpoint & 0x0f) == 0 || (endpoint & 0x70) != 0 {
                        return Err(SnapshotError::InvalidFieldEncoding("invalid endpoint address"));
                    }
                    let data = dec_bytes_limited(
                        &mut d,
                        MAX_DATA_BYTES,
                        &mut total_bytes,
                        MAX_TOTAL_BYTES,
                    )?;
                    UsbHostAction::BulkOut { id, endpoint, data }
                }
                _ => return Err(SnapshotError::InvalidFieldEncoding("invalid action kind")),
            };
            self.actions.push_back(action);
        }

        self.completions.clear();
        let completion_count = d.u32()? as usize;
        if completion_count > MAX_COMPLETIONS {
            return Err(SnapshotError::InvalidFieldEncoding(
                "too many queued completions",
            ));
        }
        for _ in 0..completion_count {
            let id = d.u32()?;
            if id == 0 {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "completion id must be non-zero",
                ));
            }
            if self.completions.contains_key(&id) {
                return Err(SnapshotError::InvalidFieldEncoding("duplicate completion id"));
            }
            let kind = d.u8()?;
            let result = match kind {
                1 => UsbHostResult::OkIn {
                    data: dec_bytes_limited(
                        &mut d,
                        MAX_DATA_BYTES,
                        &mut total_bytes,
                        MAX_TOTAL_BYTES,
                    )?,
                },
                2 => UsbHostResult::OkOut {
                    bytes_written: {
                        let bytes_written = d.u32()? as usize;
                        if bytes_written > MAX_DATA_BYTES {
                            return Err(SnapshotError::InvalidFieldEncoding(
                                "bytesWritten is too large",
                            ));
                        }
                        bytes_written
                    },
                },
                3 => UsbHostResult::Stall,
                4 => {
                    let msg_bytes = dec_bytes_limited(
                        &mut d,
                        MAX_ERROR_BYTES,
                        &mut total_bytes,
                        MAX_TOTAL_BYTES,
                    )?;
                    let msg = String::from_utf8(msg_bytes)
                        .map_err(|_| SnapshotError::InvalidFieldEncoding("invalid utf-8"))?;
                    UsbHostResult::Error(msg)
                }
                _ => {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "invalid completion kind",
                    ))
                }
            };
            self.completions.insert(id, result);
        }

        let has_control = d.bool()?;
        self.control_inflight = if has_control {
            let id = d.u32()?;
            if id == 0 {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "control inflight id must be non-zero",
                ));
            }
            let setup = dec_setup(&mut d)?;
            let has_data = d.bool()?;
            let data = has_data
                .then(|| {
                    dec_bytes_limited(&mut d, MAX_DATA_BYTES, &mut total_bytes, MAX_TOTAL_BYTES)
                })
                .transpose()?;
            let expected_len = setup.w_length as usize;
            if setup.is_device_to_host() {
                if has_data {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "control inflight DATA stage must be absent for device-to-host requests",
                    ));
                }
            } else {
                match expected_len {
                    0 => {
                        if has_data {
                            return Err(SnapshotError::InvalidFieldEncoding(
                                "control inflight DATA stage must be absent when wLength=0",
                            ));
                        }
                    }
                    _ => {
                        let Some(buf) = data.as_ref() else {
                            return Err(SnapshotError::InvalidFieldEncoding(
                                "control inflight DATA stage missing",
                            ));
                        };
                        if buf.len() != expected_len {
                            return Err(SnapshotError::InvalidFieldEncoding(
                                "control inflight DATA stage length mismatch",
                            ));
                        }
                    }
                }
            }
            Some(ControlInflight { id, setup, data })
        } else {
            None
        };

        self.ep_inflight.clear();
        let ep_count = d.u32()? as usize;
        if ep_count > MAX_EP_INFLIGHT {
            return Err(SnapshotError::InvalidFieldEncoding(
                "too many inflight endpoints",
            ));
        }
        for _ in 0..ep_count {
            let endpoint = d.u8()?;
            if (endpoint & 0x0f) == 0 || (endpoint & 0x70) != 0 {
                return Err(SnapshotError::InvalidFieldEncoding("invalid endpoint address"));
            }
            if self.ep_inflight.contains_key(&endpoint) {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "duplicate inflight endpoint",
                ));
            }
            let id = d.u32()?;
            if id == 0 {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "endpoint inflight id must be non-zero",
                ));
            }
            let len = d.u32()? as usize;
            if len > MAX_DATA_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "inflight endpoint length too large",
                ));
            }
            self.ep_inflight.insert(endpoint, EpInflight { id, len });
        }

        // Invariant validation (defensive): ensure that parsed actions/completions are consistent
        // with the in-flight transfer IDs, and that in-flight IDs do not collide.
        let mut inflight_ids = HashSet::<u32>::new();
        if let Some(ctl) = self.control_inflight.as_ref() {
            if !inflight_ids.insert(ctl.id) {
                return Err(SnapshotError::InvalidFieldEncoding("duplicate inflight id"));
            }
        }
        for inflight in self.ep_inflight.values() {
            if !inflight_ids.insert(inflight.id) {
                return Err(SnapshotError::InvalidFieldEncoding("duplicate inflight id"));
            }
        }

        for action in self.actions.iter() {
            if !inflight_ids.contains(&action.id()) {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "queued action without inflight transfer",
                ));
            }
        }
        for id in self.completions.keys() {
            if !inflight_ids.contains(id) {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "queued completion without inflight transfer",
                ));
            }
        }

        d.finish()
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
        let expected_len = setup.w_length as usize;
        // Normalize empty `data_stage` for no-data control writes.
        //
        // Some callers represent "no DATA stage" as `None`, while others may pass `Some(&[])`.
        // Treat them as equivalent when `wLength == 0` so we don't re-emit duplicate host actions.
        let data_stage = if !req_dir_in && expected_len == 0 {
            match data_stage {
                Some([]) => None,
                other => other,
            }
        } else {
            data_stage
        };

        // Defensive: enforce that control OUT requests always provide exactly `wLength` bytes.
        //
        // The UHCI glue in `AttachedUsbDevice` is expected to only call us with a correct payload,
        // but this is a public API and we should not silently emit malformed host actions.
        if !req_dir_in {
            match (expected_len, data_stage) {
                (0, None) => {}
                (0, Some(_)) => return ControlResponse::Stall,
                (_, None) => return ControlResponse::Stall,
                (expected, Some(buf)) if buf.len() != expected => return ControlResponse::Stall,
                _ => {}
            }
        }

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
        // Defensive: if the host somehow produced a completion without dequeuing the corresponding
        // action, ensure the stale action cannot be executed later.
        self.drop_queued_action(inflight.id);

        if req_dir_in {
            Self::map_in_result(inflight.setup, result)
        } else {
            if let UsbHostResult::OkOut { bytes_written } = result {
                if bytes_written != expected_len {
                    return ControlResponse::Timeout;
                }
            }
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
            // The guest may retry an IN TD while rewriting its max length field; keep the original
            // requested length so we don't emit duplicate host actions.
            let inflight_id = inflight.id;
            let inflight_len = inflight.len;
            if let Some(result) = self.take_result(inflight_id) {
                self.ep_inflight.remove(&endpoint);
                // Defensive: if a completion arrives before the host dequeues the action, drop the
                // queued action to prevent executing a stale transfer later.
                self.drop_queued_action(inflight_id);
                return match result {
                    UsbHostResult::OkIn { mut data } => {
                        if data.len() > inflight_len {
                            data.truncate(inflight_len);
                        }
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
            let inflight_id = inflight.id;
            let expected_len = inflight.len;
            if let Some(result) = self.take_result(inflight_id) {
                self.ep_inflight.remove(&endpoint);
                // Defensive: if a completion arrives before the host dequeues the action, drop the
                // queued action to prevent executing a stale transfer later.
                self.drop_queued_action(inflight_id);
                return match result {
                    UsbHostResult::OkOut { bytes_written } => {
                        if bytes_written != expected_len {
                            UsbOutResult::Timeout
                        } else {
                            UsbOutResult::Ack
                        }
                    }
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

impl IoSnapshot for UsbPassthroughDevice {
    const DEVICE_ID: [u8; 4] = *b"USBP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_NEXT_ID: u16 = 1;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_NEXT_ID, self.next_id);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_NEXT_ID: u16 = 1;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Host actions/completions represent asynchronous host I/O that may have side effects.
        // To avoid replaying those side effects after restore, drop all in-flight/queued host I/O
        // state. The guest will re-issue transfers, which will re-queue fresh host actions.
        self.reset();

        self.next_id = r.u32(TAG_NEXT_ID)?.unwrap_or(1).max(1);

        Ok(())
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

    fn snapshot_load_err(bytes: Vec<u8>) -> SnapshotError {
        let res = std::panic::catch_unwind(move || {
            let mut dev = UsbPassthroughDevice::new();
            dev.snapshot_load(&bytes)
        });
        let result = res.expect("snapshot_load panicked");
        result.expect_err("expected snapshot_load to return Err")
    }

    fn assert_invalid_field_encoding(err: SnapshotError) {
        assert!(
            matches!(err, SnapshotError::InvalidFieldEncoding(_)),
            "expected InvalidFieldEncoding, got {err:?}"
        );
    }

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
    fn completion_consumption_drops_stale_queued_action_for_bulk_in() {
        let mut dev = UsbPassthroughDevice::new();

        assert_eq!(dev.handle_in_transfer(0x81, 8), UsbInResult::Nak);
        let id = dev.ep_inflight.get(&0x81).expect("ep inflight").id;

        // Push a completion without dequeuing the action. This should still be handled safely.
        dev.push_completion(UsbHostCompletion::BulkIn {
            id,
            result: UsbHostCompletionIn::Success { data: vec![1, 2] },
        });

        assert_eq!(
            dev.handle_in_transfer(0x81, 8),
            UsbInResult::Data(vec![1, 2])
        );
        assert!(
            dev.pop_action().is_none(),
            "completion consumption should drop any stale queued action"
        );
    }

    #[test]
    fn completion_consumption_drops_stale_queued_action_for_control() {
        let mut dev = UsbPassthroughDevice::new();
        let setup = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 2,
        };

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Nak
        );
        let id = dev
            .pending_summary()
            .inflight_control
            .expect("expected inflight control");

        // Push a completion without dequeuing the action.
        dev.push_completion(UsbHostCompletion::ControlIn {
            id,
            result: UsbHostCompletionIn::Success { data: vec![9, 8] },
        });

        assert_eq!(
            dev.handle_control_request(setup, None),
            ControlResponse::Data(vec![9, 8])
        );
        assert!(
            dev.pop_action().is_none(),
            "completion consumption should drop any stale queued action"
        );
    }

    #[test]
    fn alloc_id_skips_inflight_id_collisions() {
        let mut dev = UsbPassthroughDevice::new();
        dev.next_id = 1;

        // Pretend there's already an in-flight transfer with id=1.
        dev.ep_inflight.insert(0x81, EpInflight { id: 1, len: 8 });

        // Now allocate a new ID via a new IN transfer; it should not reuse id=1.
        assert_eq!(dev.handle_in_transfer(0x82, 8), UsbInResult::Nak);
        let action = dev.pop_action().expect("expected queued action");
        let id = match action {
            UsbHostAction::BulkIn { id, endpoint, .. } => {
                assert_eq!(endpoint, 0x82);
                id
            }
            other => panic!("unexpected action: {other:?}"),
        };
        assert_ne!(id, 1);
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

    #[test]
    fn snapshot_load_rejects_action_count_over_limit() {
        // Intentionally truncate the snapshot after `action_count`. Without the guard, this would
        // attempt to decode 1025 actions and hit UnexpectedEof.
        let bytes = Encoder::new().u32(1).u32(1025).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_empty_bytes() {
        let err = snapshot_load_err(Vec::new());
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_truncated_after_action_count() {
        // Declare one action but omit the body; should report UnexpectedEof.
        let bytes = Encoder::new().u32(1).u32(1).finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_truncated_control_out_action_payload_bytes() {
        // Truncate the snapshot immediately after the ControlOut data length; decoding should fail
        // while attempting to read the payload bytes.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(2) // ControlOut
            .u32(1) // action id
            // SetupPacket (host-to-device, wLength=1)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(1)
            // data length (missing bytes)
            .u32(1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_truncated_bulk_out_action_payload_bytes() {
        // Truncate the snapshot immediately after the BulkOut data length; decoding should fail
        // while attempting to read the payload bytes.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(4) // BulkOut
            .u32(1) // action id
            .u8(0x01) // endpoint
            .u32(1) // data length (missing bytes)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_completion_count_over_limit() {
        // Intentionally truncate the snapshot after `completion_count`. Without the guard, this
        // would attempt to decode 1025 completions and hit UnexpectedEof.
        let bytes = Encoder::new().u32(1).u32(0).u32(1025).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_truncated_after_completion_count() {
        // Declare one completion but omit the body; should report UnexpectedEof.
        let bytes = Encoder::new().u32(1).u32(0).u32(1).finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_ep_inflight_count_over_limit() {
        // Intentionally truncate the snapshot after `ep_count`. Without the guard, this would
        // attempt to decode 65 endpoint entries and hit UnexpectedEof.
        let bytes = Encoder::new().u32(1).u32(0).u32(0).bool(false).u32(65).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_action_kind_tag() {
        // action_count=1, invalid action kind tag (0).
        let bytes = Encoder::new().u32(1).u32(1).u8(0).u32(1).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_completion_kind_tag() {
        // completion_count=1, invalid completion kind tag (9).
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(1)
            .u32(1)
            .u8(9)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_in_action_with_host_to_device_setup() {
        // ControlIn actions must encode a device-to-host setup packet.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(1) // ControlIn
            .u32(1) // action id
            // SetupPacket (host-to-device, invalid for ControlIn)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_out_action_with_device_to_host_setup() {
        // ControlOut actions must encode a host-to-device setup packet.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(2) // ControlOut
            .u32(1) // action id
            // SetupPacket (device-to-host, invalid for ControlOut)
            .u8(0x80)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_out_action_with_data_length_mismatch() {
        // ControlOut data length must match wLength.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(2) // ControlOut
            .u32(1) // action id
            // SetupPacket (host-to-device, wLength=2)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(2)
            // data length (1) + data bytes
            .u32(1)
            .u8(0xaa)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_in_action_with_out_endpoint() {
        // BulkIn endpoint must be an IN endpoint address.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(3) // BulkIn
            .u32(1)
            .u8(0x01) // OUT endpoint address
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_out_action_with_in_endpoint() {
        // BulkOut endpoint must be an OUT endpoint address.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(4) // BulkOut
            .u32(1)
            .u8(0x81) // IN endpoint address
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_in_action_with_invalid_endpoint_address() {
        // IN endpoint, but endpoint number is 0 (control endpoint) which is invalid for BulkIn.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(3) // BulkIn
            .u32(1)
            .u8(0x80) // invalid endpoint address
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_in_action_with_reserved_bits_set_in_endpoint() {
        // Endpoint is IN, but reserved bits (4-6) are not zero.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(3) // BulkIn
            .u32(1)
            .u8(0x91) // endpoint=1 with bit4 set
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_out_action_with_invalid_endpoint_address() {
        // OUT endpoint, but endpoint number is 0 (control endpoint) which is invalid for BulkOut.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(4) // BulkOut
            .u32(1)
            .u8(0x00) // invalid endpoint address
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_out_action_with_reserved_bits_set_in_endpoint() {
        // Endpoint is OUT, but reserved bits (4-6) are not zero.
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(4) // BulkOut
            .u32(1)
            .u8(0x11) // endpoint=1 with bit4 set
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_in_length_over_limit() {
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(3) // BulkIn
            .u32(1)
            .u8(0x81)
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_action_payload_over_limit_before_reading_bytes() {
        // Intentionally truncate the snapshot after the oversized action payload length.
        // If the implementation tried to read the payload, we'd get UnexpectedEof instead of an
        // InvalidFieldEncoding bounds-check error.
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(2) // ControlOut
            .u32(1) // action id
            // SetupPacket
            .u8(0)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            // data length
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_bulk_out_payload_over_limit_before_reading_bytes() {
        // Same as `snapshot_load_rejects_action_payload_over_limit_before_reading_bytes`, but for
        // the BulkOut action path.
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(1)
            .u8(4) // BulkOut
            .u32(1) // action id
            .u8(0x01) // endpoint
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_completion_error_message_over_limit_before_reading_bytes() {
        // Intentionally truncate the snapshot after the oversized error length.
        // If the implementation tried to read the message bytes, we'd get UnexpectedEof instead of
        // an InvalidFieldEncoding bounds-check error.
        const MAX_ERROR_BYTES: u32 = 16 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(1)
            .u32(1) // completion id
            .u8(4) // Error
            .u32(MAX_ERROR_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_truncated_completion_ok_in_payload_bytes() {
        // Truncate after the OkIn payload length so decoding fails while reading the bytes.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(0) // action_count
            .u32(1) // completion_count
            .u32(1) // completion id
            .u8(1) // OkIn
            .u32(1) // payload length (missing bytes)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_truncated_completion_error_message_bytes() {
        // Truncate after the Error message length so decoding fails while reading the bytes.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(0) // action_count
            .u32(1) // completion_count
            .u32(1) // completion id
            .u8(4) // Error
            .u32(1) // message length (missing bytes)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_completion_ok_in_payload_over_limit_before_reading_bytes() {
        // Same as `snapshot_load_rejects_action_payload_over_limit_before_reading_bytes`, but for
        // the completion OkIn data path.
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(1)
            .u32(1) // completion id
            .u8(1) // OkIn
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_completion_ok_out_bytes_written_over_limit() {
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(0) // action_count
            .u32(1) // completion_count
            .u32(1) // completion id
            .u8(2) // OkOut
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_utf8_error_message() {
        // One completion with Error result and invalid UTF-8 bytes.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(1)
            .u32(1) // completion id
            .u8(4) // Error
            .u32(1)
            .u8(0xff)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_total_buffers_exceed_max_total_bytes() {
        const MAX_DATA_BYTES: usize = 4 * 1024 * 1024;

        // Four buffers at MAX_DATA_BYTES == 16MiB total are allowed; the next byte should fail the
        // MAX_TOTAL_BYTES guard.
        let chunk = vec![0u8; MAX_DATA_BYTES];

        // Include buffers across both the action and completion sections to ensure the running
        // `total_bytes` guard applies globally.
        let mut enc = Encoder::new().u32(1).u32(2);
        for i in 0..2u32 {
            enc = enc
                .u8(4) // BulkOut action
                .u32(i + 1)
                .u8(0x01)
                .u32(MAX_DATA_BYTES as u32)
                .bytes(&chunk);
        }

        enc = enc.u32(3); // completion_count
        for i in 0..2u32 {
            enc = enc
                .u32(100 + i)
                .u8(1) // OkIn
                .u32(MAX_DATA_BYTES as u32)
                .bytes(&chunk);
        }

        // This final completion would push total_bytes over MAX_TOTAL_BYTES. Intentionally omit the
        // trailing byte; the decoder should error before reading it.
        enc = enc.u32(200).u8(1).u32(1);
        let bytes = enc.finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_zero_next_id() {
        // Minimal snapshot with next_id=0 is invalid; IDs are expected to be non-zero so they don't
        // become falsy in JS host integrations.
        let bytes = Encoder::new().u32(0).u32(0).u32(0).bool(false).u32(0).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_zero_action_id() {
        // action_count=1, valid kind tag, but id=0.
        // Intentionally truncate after the ID; the decoder should fail before reading setup bytes.
        let bytes = Encoder::new().u32(1).u32(1).u8(1).u32(0).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_zero_completion_id() {
        // completion_count=1, id=0.
        let bytes = Encoder::new().u32(1).u32(0).u32(1).u32(0).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_zero_control_inflight_id() {
        // has_control=true but inflight id=0.
        let bytes = Encoder::new().u32(1).u32(0).u32(0).bool(true).u32(0).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_zero_endpoint_inflight_id() {
        // ep_count=1, endpoint=1, inflight id=0.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(1) // ep_count
            .u8(1) // endpoint
            .u32(0)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_queued_action_without_matching_inflight_id() {
        // One queued action but no control/endpoint inflight entries.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(1) // action_count
            .u8(1) // ControlIn
            .u32(1) // action id
            // SetupPacket (device-to-host)
            .u8(0x80)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .u32(0) // completion_count
            .bool(false) // has_control
            .u32(0) // ep_count
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_queued_completion_without_matching_inflight_id() {
        // One queued completion but no inflight entries.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(0) // action_count
            .u32(1) // completion_count
            .u32(1) // completion id
            .u8(3) // Stall
            .bool(false) // has_control
            .u32(0) // ep_count
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_duplicate_action_id() {
        // Second action reuses the same id (duplicate). This should be rejected before attempting to
        // read the rest of the action fields.
        let bytes = Encoder::new()
            .u32(1)
            .u32(2) // action_count
            // BulkIn #1 (id=1)
            .u8(3)
            .u32(1)
            .u8(0x81)
            .u32(8)
            // BulkIn #2 (duplicate id=1)
            .u8(3)
            .u32(1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_duplicate_completion_id() {
        // Second completion reuses the same id (duplicate). This should be rejected before reading
        // the completion kind/payload.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0) // action_count
            .u32(2) // completion_count
            // Completion #1 (id=1, Stall)
            .u32(1)
            .u8(3)
            // Completion #2 (duplicate id=1)
            .u32(1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_duplicate_inflight_endpoint_entry() {
        // Two inflight endpoint entries for the same endpoint address.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(2) // ep_count
            // endpoint 0x81, id=1
            .u8(0x81)
            .u32(1)
            .u32(8)
            // duplicate endpoint 0x81
            .u8(0x81)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_inflight_endpoint_address() {
        // endpoint address must have endpoint number 1..=15, reserved bits clear.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(1) // ep_count
            .u8(0x80) // invalid endpoint number 0
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_inflight_endpoint_address_with_reserved_bits_set() {
        // Bits 4-6 are reserved and must be zero.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(1) // ep_count
            .u8(0x91) // endpoint=1 with reserved bit4 set
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_inflight_endpoint_length_over_limit() {
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(1) // ep_count
            .u8(0x81)
            .u32(1) // inflight id
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_inflight_id_collision_between_endpoints() {
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(2) // ep_count
            // endpoint 0x81, inflight id=1
            .u8(0x81)
            .u32(1)
            .u32(8)
            // endpoint 0x82, same inflight id=1 (collision)
            .u8(0x82)
            .u32(1)
            .u32(8)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_inflight_id_collision_between_control_and_endpoint() {
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (device-to-host, wLength=0)
            .u8(0x80)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .bool(false) // has_data
            .u32(1) // ep_count
            .u8(0x81)
            .u32(1) // duplicate inflight id
            .u32(8)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_bool_encoding() {
        // `has_control` is decoded as a bool; only 0/1 are valid.
        let bytes = Encoder::new().u32(1).u32(0).u32(0).u8(2).finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_invalid_has_data_bool_encoding() {
        // Encode an inflight control transfer but corrupt the `has_data` bool discriminator.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket
            .u8(0)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            // has_data bool (invalid)
            .u8(2)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_inflight_device_to_host_with_data_stage() {
        // Device-to-host (IN) control transfers must not include an OUT DATA stage.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (device-to-host, wLength=0)
            .u8(0x80)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .bool(true) // has_data (invalid for device-to-host)
            .u32(0) // data length
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_inflight_wlength_zero_with_data_stage() {
        // Host-to-device (OUT) control transfers with wLength=0 must not include a DATA stage.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (host-to-device, wLength=0)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            .bool(true) // has_data (invalid when wLength=0)
            .u32(0)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_inflight_missing_data_stage() {
        // Host-to-device control transfers with wLength>0 must include a DATA stage.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (host-to-device, wLength=1)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(1)
            .bool(false) // has_data (missing)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_inflight_data_stage_length_mismatch() {
        // Host-to-device control transfers must have DATA stage length == wLength.
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (host-to-device, wLength=2)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(2)
            .bool(true) // has_data
            .u32(1) // data length (mismatch)
            .u8(0xaa)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_control_inflight_data_payload_over_limit_before_reading_bytes() {
        // Intentionally truncate the snapshot after the oversized inflight control data length.
        // If the implementation tried to read the payload, we'd get UnexpectedEof instead of an
        // InvalidFieldEncoding bounds-check error.
        const MAX_DATA_BYTES: u32 = 4 * 1024 * 1024;
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket
            .u8(0)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(0)
            // has_data
            .bool(true)
            // data length
            .u32(MAX_DATA_BYTES + 1)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_truncated_control_inflight_data_payload_bytes() {
        // Truncate the snapshot immediately after the inflight control data length; decoding should
        // fail while attempting to read the payload bytes.
        let bytes = Encoder::new()
            .u32(1) // next_id
            .u32(0) // action_count
            .u32(0) // completion_count
            .bool(true)
            .u32(1) // control inflight id
            // SetupPacket (host-to-device, wLength=1)
            .u8(0x00)
            .u8(0)
            .u16(0)
            .u16(0)
            .u16(1)
            .bool(true) // has_data
            .u32(1) // data length (missing bytes)
            .finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_load_rejects_trailing_bytes() {
        let bytes = Encoder::new()
            .u32(1)
            .u32(0)
            .u32(0)
            .bool(false)
            .u32(0)
            .u8(0xAA) // trailing byte
            .finish();
        let err = snapshot_load_err(bytes);
        assert_invalid_field_encoding(err);
    }

    #[test]
    fn snapshot_load_rejects_truncated_bytes() {
        // Truncated immediately after `next_id`; the decoder should report UnexpectedEof.
        let bytes = Encoder::new().u32(1).finish();
        let err = snapshot_load_err(bytes);
        assert_eq!(err, SnapshotError::UnexpectedEof);
    }

    #[test]
    fn snapshot_save_is_deterministic_independent_of_hashmap_order() {
        let mut dev_a = UsbPassthroughDevice::new();
        let mut dev_b = UsbPassthroughDevice::new();

        // Use a larger set of endpoints so the test reliably fails if snapshot_save starts
        // iterating HashMaps without sorting.
        let endpoints: Vec<u8> = (1u8..=15).map(|n| 0x80 | n).collect();

        // Create identical in-flight endpoints so both devices allocate the same IDs.
        for &ep in &endpoints {
            assert_eq!(dev_a.handle_in_transfer(ep, 8), UsbInResult::Nak);
            assert_eq!(dev_b.handle_in_transfer(ep, 8), UsbInResult::Nak);
        }

        let mut ids = Vec::with_capacity(endpoints.len());
        for &ep in &endpoints {
            let id_a = dev_a.ep_inflight.get(&ep).expect("ep inflight").id;
            let id_b = dev_b.ep_inflight.get(&ep).expect("ep inflight").id;
            assert_eq!(id_a, id_b);
            ids.push(id_a);
        }

        // Push completions in opposite orders; this exercises the completion HashMap ordering.
        for (&ep, &id) in endpoints.iter().zip(ids.iter()) {
            dev_a.push_completion(UsbHostCompletion::BulkIn {
                id,
                result: UsbHostCompletionIn::Success { data: vec![ep] },
            });
        }
        for (&ep, &id) in endpoints.iter().rev().zip(ids.iter().rev()) {
            dev_b.push_completion(UsbHostCompletion::BulkIn {
                id,
                result: UsbHostCompletionIn::Success { data: vec![ep] },
            });
        }

        // Also ensure the endpoint-inflight HashMap sees a different insertion order while
        // preserving identical logical state (same keys + values).
        let mut inflight_reinsert = Vec::with_capacity(endpoints.len());
        for &ep in &endpoints {
            inflight_reinsert.push((ep, dev_b.ep_inflight.remove(&ep).expect("ep inflight")));
        }
        for (ep, inflight) in inflight_reinsert.into_iter().rev() {
            dev_b.ep_inflight.insert(ep, inflight);
        }

        let snap_a = dev_a.snapshot_save();
        let snap_b = dev_b.snapshot_save();
        assert_eq!(snap_a, snap_b);
        
        // The on-wire encoding must also be stable in ordering (not just stable between two
        // instances by chance). Decode the snapshot and ensure HashMap-backed collections are
        // emitted in sorted order.
        fn decode_completion_and_ep_order(bytes: &[u8]) -> (Vec<u32>, Vec<u8>) {
            fn skip_setup(d: &mut Decoder<'_>) {
                d.u8().unwrap();
                d.u8().unwrap();
                d.u16().unwrap();
                d.u16().unwrap();
                d.u16().unwrap();
            }

            let mut d = Decoder::new(bytes);
            let _next_id = d.u32().unwrap();

            let action_count = d.u32().unwrap() as usize;
            for _ in 0..action_count {
                let kind = d.u8().unwrap();
                let _id = d.u32().unwrap();
                match kind {
                    1 => skip_setup(&mut d), // ControlIn
                    2 => {
                        // ControlOut
                        skip_setup(&mut d);
                        let len = d.u32().unwrap() as usize;
                        d.bytes(len).unwrap();
                    }
                    3 => {
                        // BulkIn
                        d.u8().unwrap(); // endpoint
                        d.u32().unwrap(); // length
                    }
                    4 => {
                        // BulkOut
                        d.u8().unwrap(); // endpoint
                        let len = d.u32().unwrap() as usize;
                        d.bytes(len).unwrap();
                    }
                    other => panic!("unexpected action kind {other}"),
                }
            }

            let completion_count = d.u32().unwrap() as usize;
            let mut completion_ids = Vec::with_capacity(completion_count);
            for _ in 0..completion_count {
                let id = d.u32().unwrap();
                completion_ids.push(id);
                let kind = d.u8().unwrap();
                match kind {
                    1 => {
                        // OkIn
                        let len = d.u32().unwrap() as usize;
                        d.bytes(len).unwrap();
                    }
                    2 => {
                        // OkOut
                        d.u32().unwrap();
                    }
                    3 => {} // Stall
                    4 => {
                        // Error
                        let len = d.u32().unwrap() as usize;
                        d.bytes(len).unwrap();
                    }
                    other => panic!("unexpected completion kind {other}"),
                }
            }

            let has_control = d.bool().unwrap();
            if has_control {
                let _id = d.u32().unwrap();
                d.u8().unwrap();
                d.u8().unwrap();
                d.u16().unwrap();
                d.u16().unwrap();
                d.u16().unwrap();
                let has_data = d.bool().unwrap();
                if has_data {
                    let len = d.u32().unwrap() as usize;
                    d.bytes(len).unwrap();
                }
            }

            let ep_count = d.u32().unwrap() as usize;
            let mut eps = Vec::with_capacity(ep_count);
            for _ in 0..ep_count {
                let ep = d.u8().unwrap();
                eps.push(ep);
                d.u32().unwrap(); // id
                d.u32().unwrap(); // len
            }

            d.finish().unwrap();
            (completion_ids, eps)
        }

        let (completion_ids, inflight_eps) = decode_completion_and_ep_order(&snap_a);
        assert!(
            completion_ids.windows(2).all(|w| w[0] < w[1]),
            "completion IDs must be sorted: {completion_ids:?}"
        );
        assert_eq!(
            completion_ids, ids,
            "snapshot must preserve all completion IDs in sorted order"
        );
        assert!(
            inflight_eps.windows(2).all(|w| w[0] < w[1]),
            "inflight endpoints must be sorted: {inflight_eps:?}"
        );
        assert_eq!(
            inflight_eps, endpoints,
            "snapshot must preserve all inflight endpoints in sorted order"
        );

        // Round-trip and ensure the canonical encoding is stable.
        let mut loaded = UsbPassthroughDevice::new();
        loaded.snapshot_load(&snap_a).unwrap();
        assert_eq!(loaded.snapshot_save(), snap_a);
    }
}
