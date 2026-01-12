//! WASM bindings for operating on a JS `SharedArrayBuffer`.
//!
//! The core ring buffer algorithm is implemented in [`crate::ring`] with
//! `std::sync::atomic` primitives for host testing. In production, each worker
//! (TS or Rust/WASM) will receive the same `SharedArrayBuffer` and operate on it
//! with `Atomics`.
//!
//! This module is compiled only when targeting `wasm32` with the crate feature
//! `"wasm"` enabled.
//!
//! This module provides helpers to:
//! - create typed views (`Int32Array`, `Uint8Array`) for a ring buffer region
//! - perform atomic operations through JS `Atomics.*`
//!
//! Note: Browsers currently allow `Atomics.wait` only in worker contexts.

use crate::layout::{
    align_up, ipc_header, queue_desc, ring_ctrl, IPC_MAGIC, IPC_VERSION, RECORD_ALIGN, WRAP_MARKER,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Atomics, js_name = load)]
    fn atomics_load_i32(arr: &js_sys::Int32Array, index: u32) -> i32;

    #[wasm_bindgen(js_namespace = Atomics, js_name = store)]
    fn atomics_store_i32(arr: &js_sys::Int32Array, index: u32, value: i32) -> i32;

    #[wasm_bindgen(js_namespace = Atomics, js_name = compareExchange)]
    fn atomics_compare_exchange_i32(
        arr: &js_sys::Int32Array,
        index: u32,
        expected: i32,
        replacement: i32,
    ) -> i32;

    #[wasm_bindgen(js_namespace = Atomics, js_name = wait)]
    fn atomics_wait_i32(arr: &js_sys::Int32Array, index: u32, value: i32) -> JsValue;

    #[wasm_bindgen(js_namespace = Atomics, js_name = notify)]
    fn atomics_notify_i32(arr: &js_sys::Int32Array, index: u32, count: i32) -> i32;
}

#[wasm_bindgen]
pub struct SharedRingBuffer {
    ctrl: js_sys::Int32Array,
    data: js_sys::Uint8Array,
    cap: u32,
}

#[wasm_bindgen]
impl SharedRingBuffer {
    /// Create a ring buffer view from a shared buffer and an offset.
    ///
    /// `offset_bytes` must point at the start of the ring header as defined by
    /// `layout::ring_ctrl`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        buffer: js_sys::SharedArrayBuffer,
        offset_bytes: u32,
    ) -> Result<SharedRingBuffer, JsValue> {
        let ctrl = js_sys::Int32Array::new_with_byte_offset_and_length(
            &buffer,
            offset_bytes,
            ring_ctrl::WORDS as u32,
        );
        let cap = atomics_load_i32(&ctrl, ring_ctrl::CAPACITY as u32) as u32;
        let data = js_sys::Uint8Array::new_with_byte_offset_and_length(
            &buffer,
            offset_bytes + ring_ctrl::BYTES as u32,
            cap,
        );
        Ok(Self { ctrl, data, cap })
    }

    pub fn capacity_bytes(&self) -> u32 {
        self.cap
    }

    /// Non-blocking MPSC-safe push.
    pub fn try_push(&self, payload: &[u8]) -> bool {
        let payload_len = payload.len();
        let record_size = align_up(4 + payload_len, RECORD_ALIGN);
        if record_size > self.cap as usize {
            return false;
        }

        loop {
            let head = atomics_load_i32(&self.ctrl, ring_ctrl::HEAD as u32) as u32;
            let tail = atomics_load_i32(&self.ctrl, ring_ctrl::TAIL_RESERVE as u32) as u32;
            let used = tail.wrapping_sub(head);
            if used > self.cap {
                continue;
            }
            let free = self.cap - used;

            let tail_index = (tail % self.cap) as usize;
            let remaining = (self.cap as usize) - tail_index;

            let (padding, write_wrap_marker) = if remaining < 4 {
                (remaining, false)
            } else if remaining < record_size {
                (remaining, true)
            } else {
                (0, false)
            };

            let reserve = padding + record_size;
            if reserve as u32 > free {
                return false;
            }

            let new_tail = tail.wrapping_add(reserve as u32);
            let prev = atomics_compare_exchange_i32(
                &self.ctrl,
                ring_ctrl::TAIL_RESERVE as u32,
                tail as i32,
                new_tail as i32,
            ) as u32;
            if prev != tail {
                continue;
            }

            if write_wrap_marker {
                write_u32_le(&self.data, tail_index, WRAP_MARKER);
            }

            let start = tail.wrapping_add(padding as u32);
            let start_index = (start % self.cap) as usize;
            write_u32_le(&self.data, start_index, payload_len as u32);
            self.data
                .subarray(
                    start_index as u32 + 4,
                    start_index as u32 + 4 + payload_len as u32,
                )
                .copy_from(payload);

            // Commit in order.
            loop {
                let committed = atomics_load_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32) as u32;
                if committed == tail {
                    break;
                }
                // Block until commit changes.
                let _ =
                    atomics_wait_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32, committed as i32);
            }
            atomics_store_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32, new_tail as i32);
            atomics_notify_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32, 1);
            return true;
        }
    }

    /// Non-blocking pop.
    ///
    /// Returns an owned `Uint8Array` containing the payload, or `null` if empty.
    pub fn try_pop(&self) -> Option<js_sys::Uint8Array> {
        loop {
            let head = atomics_load_i32(&self.ctrl, ring_ctrl::HEAD as u32) as u32;
            let tail = atomics_load_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32) as u32;
            if head == tail {
                return None;
            }

            let head_index = (head % self.cap) as usize;
            let remaining = (self.cap as usize) - head_index;

            if remaining < 4 {
                let new_head = head.wrapping_add(remaining as u32);
                atomics_store_i32(&self.ctrl, ring_ctrl::HEAD as u32, new_head as i32);
                atomics_notify_i32(&self.ctrl, ring_ctrl::HEAD as u32, 1);
                continue;
            }

            let len = read_u32_le(&self.data, head_index);
            if len == WRAP_MARKER {
                let new_head = head.wrapping_add(remaining as u32);
                atomics_store_i32(&self.ctrl, ring_ctrl::HEAD as u32, new_head as i32);
                atomics_notify_i32(&self.ctrl, ring_ctrl::HEAD as u32, 1);
                continue;
            }

            let total = align_up(4 + len as usize, RECORD_ALIGN);
            if total > remaining {
                // Corrupt; treat as empty.
                return None;
            }

            let start = head_index + 4;
            let end = start + len as usize;
            let out = self.data.slice(start as u32, end as u32);

            let new_head = head.wrapping_add(total as u32);
            atomics_store_i32(&self.ctrl, ring_ctrl::HEAD as u32, new_head as i32);
            atomics_notify_i32(&self.ctrl, ring_ctrl::HEAD as u32, 1);
            return Some(out);
        }
    }

    /// Block until the queue becomes non-empty (worker contexts only).
    pub fn wait_for_data(&self) {
        loop {
            let head = atomics_load_i32(&self.ctrl, ring_ctrl::HEAD as u32);
            let tail = atomics_load_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32);
            if head != tail {
                return;
            }
            let _ = atomics_wait_i32(&self.ctrl, ring_ctrl::TAIL_COMMIT as u32, tail);
        }
    }

    /// Block until `payload` can be pushed (worker contexts only).
    pub fn push_blocking(&self, payload: &[u8]) -> Result<(), JsValue> {
        let record_size = align_up(4 + payload.len(), RECORD_ALIGN);
        if record_size > self.cap as usize {
            return Err(JsValue::from_str("payload too large for ring buffer"));
        }

        loop {
            if self.try_push(payload) {
                return Ok(());
            }
            let head = atomics_load_i32(&self.ctrl, ring_ctrl::HEAD as u32);
            let _ = atomics_wait_i32(&self.ctrl, ring_ctrl::HEAD as u32, head);
        }
    }

    /// Block until a record is available, then pop it.
    pub fn pop_blocking(&self) -> js_sys::Uint8Array {
        loop {
            if let Some(v) = self.try_pop() {
                return v;
            }
            self.wait_for_data();
        }
    }
}

fn read_u32_le(data: &js_sys::Uint8Array, offset: usize) -> u32 {
    let mut tmp = [0u8; 4];
    data.subarray(offset as u32, offset as u32 + 4)
        .copy_to(&mut tmp);
    u32::from_le_bytes(tmp)
}

fn write_u32_le(data: &js_sys::Uint8Array, offset: usize, v: u32) {
    let bytes = v.to_le_bytes();
    data.subarray(offset as u32, offset as u32 + 4)
        .copy_from(&bytes);
}

/// Parse an Aero IPC `SharedArrayBuffer` and open the `nth` queue whose descriptor
/// `kind` matches `kind`.
///
/// This mirrors `web/src/ipc/ipc.ts::openRingByKind` so WASM code does not need JS
/// to plumb ring offsets.
///
/// For the browser runtime's `ioIpcSab` segment, queue-kind values are also
/// exposed as constants in [`crate::layout::io_ipc_queue_kind`].
///
/// ```ignore
/// use aero_ipc::layout::io_ipc_queue_kind;
///
/// // Open the NET_TX ring (kind=2) from the ioIpc SharedArrayBuffer.
/// let ring = aero_ipc::wasm::open_ring_by_kind(io_ipc_sab, io_ipc_queue_kind::NET_TX, 0)?;
/// # Ok::<(), wasm_bindgen::JsValue>(())
/// ```
#[wasm_bindgen]
pub fn open_ring_by_kind(
    buffer: js_sys::SharedArrayBuffer,
    kind: u32,
    nth: u32,
) -> Result<SharedRingBuffer, JsValue> {
    let byte_len = buffer.byte_length();
    if byte_len < ipc_header::BYTES as u32 {
        return Err(JsValue::from_str("buffer too small for IPC header"));
    }

    // All header/descriptor fields are 32-bit words. Using a Uint32Array avoids
    // copying data out of the SharedArrayBuffer.
    let words = js_sys::Uint32Array::new(&buffer);

    let magic = words.get_index(ipc_header::MAGIC as u32);
    if magic != IPC_MAGIC {
        return Err(JsValue::from_str(&format!(
            "bad IPC magic (expected 0x{IPC_MAGIC:08x}, got 0x{magic:08x})"
        )));
    }

    let version = words.get_index(ipc_header::VERSION as u32);
    if version != IPC_VERSION {
        return Err(JsValue::from_str(&format!(
            "unsupported IPC version {version} (expected {IPC_VERSION})"
        )));
    }

    let total_bytes = words.get_index(ipc_header::TOTAL_BYTES as u32);
    if total_bytes != byte_len {
        return Err(JsValue::from_str(&format!(
            "buffer length mismatch (header={total_bytes} actual={byte_len})"
        )));
    }

    let queue_count = words.get_index(ipc_header::QUEUE_COUNT as u32);
    let desc_bytes = (ipc_header::BYTES as u32)
        .checked_add(
            queue_count
                .checked_mul(queue_desc::BYTES as u32)
                .ok_or_else(|| {
                    JsValue::from_str("queue descriptor region overflows (queue_count too large)")
                })?,
        )
        .ok_or_else(|| {
            JsValue::from_str("queue descriptor region overflows (queue_count too large)")
        })?;

    if byte_len < desc_bytes {
        return Err(JsValue::from_str("buffer too small for queue descriptors"));
    }

    // Mirror the TS behaviour: validate all descriptors before selecting a queue.
    let mut queues: Vec<(u32, u32)> = Vec::with_capacity(queue_count as usize);
    for i in 0..queue_count {
        let base = ipc_header::WORDS as u32 + i * queue_desc::WORDS as u32;
        let q_kind = words.get_index(base + queue_desc::KIND as u32);
        let offset_bytes = words.get_index(base + queue_desc::OFFSET_BYTES as u32);
        let capacity_bytes = words.get_index(base + queue_desc::CAPACITY_BYTES as u32);
        let reserved = words.get_index(base + queue_desc::RESERVED as u32);

        if reserved != 0 {
            return Err(JsValue::from_str(&format!(
                "queue descriptor {i} reserved field must be 0"
            )));
        }
        if !offset_bytes.is_multiple_of(RECORD_ALIGN as u32) {
            return Err(JsValue::from_str(&format!(
                "queue[{i}].offsetBytes must be aligned to {RECORD_ALIGN} bytes (got {offset_bytes})"
            )));
        }
        if !capacity_bytes.is_multiple_of(RECORD_ALIGN as u32) {
            return Err(JsValue::from_str(&format!(
                "queue[{i}].capacityBytes must be aligned to {RECORD_ALIGN} bytes (got {capacity_bytes})"
            )));
        }

        let region_end = offset_bytes
            .checked_add(ring_ctrl::BYTES as u32)
            .and_then(|v| v.checked_add(capacity_bytes))
            .ok_or_else(|| JsValue::from_str(&format!("queue descriptor {i} out of bounds")))?;
        if region_end > byte_len {
            return Err(JsValue::from_str(&format!(
                "queue descriptor {i} out of bounds"
            )));
        }

        // Validate ring header capacity matches the descriptor.
        let ctrl = js_sys::Int32Array::new_with_byte_offset_and_length(
            &buffer,
            offset_bytes,
            ring_ctrl::WORDS as u32,
        );
        let ring_cap = atomics_load_i32(&ctrl, ring_ctrl::CAPACITY as u32) as u32;
        if ring_cap != capacity_bytes {
            return Err(JsValue::from_str(&format!(
                "queue descriptor {i} capacity mismatch (desc={capacity_bytes} ringHeader={ring_cap})"
            )));
        }

        queues.push((q_kind, offset_bytes));
    }

    let mut seen = 0u32;
    for (q_kind, offset_bytes) in queues {
        if q_kind != kind {
            continue;
        }
        if seen == nth {
            return SharedRingBuffer::new(buffer, offset_bytes);
        }
        seen += 1;
    }

    Err(JsValue::from_str(&format!("queue kind {kind} not found")))
}
