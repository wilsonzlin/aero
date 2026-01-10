//! Lock-free bounded ring buffer for variable-length records.
//!
//! Design goals:
//! - Works on top of a shared byte buffer (e.g. `SharedArrayBuffer`).
//! - Variable-sized records (length-prefixed).
//! - Wrap-around handled via an explicit wrap marker plus implicit padding when
//!   fewer than 4 bytes remain at the end of the buffer.
//! - MPSC (multi-producer / single-consumer) via a reservation pointer
//!   (`tail_reserve`) and an in-order commit pointer (`tail_commit`).
//! - Can be used as a simpler SPSC queue by having a single producer.
//!
//! The JS/TS implementation mirrors this exactly. See `docs/ipc-protocol.md`.

use crate::layout::{align_up, ring_ctrl, RECORD_ALIGN, WRAP_MARKER};
use core::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushError {
    Full,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopError {
    /// The queue is empty.
    Empty,
    /// Corruption detected (e.g. a bogus length).
    Corrupt,
}

/// In-memory ring buffer implementation used for host-side tests and fuzzing.
///
/// The WASM build uses a `SharedArrayBuffer` view instead; the algorithm is the
/// same.
pub struct RingBuffer {
    cap: u32,
    head: AtomicU32,
    tail_reserve: AtomicU32,
    tail_commit: AtomicU32,

    data_ptr: *mut u8,
    _storage: Box<[u8]>,
}

unsafe impl Send for RingBuffer {}
unsafe impl Sync for RingBuffer {}

impl RingBuffer {
    pub fn new(capacity_bytes: usize) -> Self {
        assert!(capacity_bytes > 0);
        assert_eq!(capacity_bytes % RECORD_ALIGN, 0);
        assert!(capacity_bytes < u32::MAX as usize);
        let mut storage = vec![0u8; capacity_bytes].into_boxed_slice();
        let data_ptr = storage.as_mut_ptr();
        Self {
            cap: capacity_bytes as u32,
            head: AtomicU32::new(0),
            tail_reserve: AtomicU32::new(0),
            tail_commit: AtomicU32::new(0),
            data_ptr,
            _storage: storage,
        }
    }

    pub fn capacity_bytes(&self) -> usize {
        self.cap as usize
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) == self.tail_commit.load(Ordering::Acquire)
    }

    pub fn try_push(&self, payload: &[u8]) -> Result<(), PushError> {
        let payload_len = payload.len();
        if payload_len > (u32::MAX as usize).saturating_sub(4) {
            return Err(PushError::TooLarge);
        }

        let record_size = align_up(4 + payload_len, RECORD_ALIGN);
        if record_size > self.cap as usize {
            return Err(PushError::TooLarge);
        }

        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail_reserve.load(Ordering::Acquire);

            let used = tail.wrapping_sub(head);
            if used > self.cap {
                // We raced with the consumer advancing `head` between reads; retry.
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
                return Err(PushError::Full);
            }

            let new_tail = tail.wrapping_add(reserve as u32);
            if self
                .tail_reserve
                .compare_exchange(tail, new_tail, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }

            unsafe {
                if write_wrap_marker {
                    write_u32_le(self.data_ptr.add(tail_index), WRAP_MARKER);
                }

                let start = tail.wrapping_add(padding as u32);
                let start_index = (start % self.cap) as usize;

                // The record is guaranteed to fit contiguously from `start_index`.
                write_u32_le(self.data_ptr.add(start_index), payload_len as u32);
                core::ptr::copy_nonoverlapping(
                    payload.as_ptr(),
                    self.data_ptr.add(start_index + 4),
                    payload_len,
                );
            }

            // Commit in-order.
            loop {
                let committed = self.tail_commit.load(Ordering::Acquire);
                if committed == tail {
                    break;
                }
                core::hint::spin_loop();
            }

            self.tail_commit.store(new_tail, Ordering::Release);
            return Ok(());
        }
    }

    pub fn try_pop(&self) -> Result<Vec<u8>, PopError> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail_commit.load(Ordering::Acquire);
            if head == tail {
                return Err(PopError::Empty);
            }

            let head_index = (head % self.cap) as usize;
            let remaining = (self.cap as usize) - head_index;

            if remaining < 4 {
                // Implicit padding.
                let new_head = head.wrapping_add(remaining as u32);
                self.head.store(new_head, Ordering::Release);
                continue;
            }

            let len = unsafe { read_u32_le(self.data_ptr.add(head_index)) };
            if len == WRAP_MARKER {
                // Explicit wrap marker: skip to the start of the next segment.
                let new_head = head.wrapping_add(remaining as u32);
                self.head.store(new_head, Ordering::Release);
                continue;
            }

            let len_usize = len as usize;
            let total = align_up(4 + len_usize, RECORD_ALIGN);
            if total > remaining {
                return Err(PopError::Corrupt);
            }
            let committed = tail.wrapping_sub(head);
            if committed < total as u32 {
                // Shouldn't happen with in-order commits.
                return Err(PopError::Corrupt);
            }

            let mut out = vec![0u8; len_usize];
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data_ptr.add(head_index + 4),
                    out.as_mut_ptr(),
                    len_usize,
                );
            }

            let new_head = head.wrapping_add(total as u32);
            self.head.store(new_head, Ordering::Release);
            return Ok(out);
        }
    }

    /// Convenience helper used by tests to drain the queue without busy loops.
    pub fn pop_spinning(&self) -> Vec<u8> {
        loop {
            match self.try_pop() {
                Ok(v) => return v,
                Err(PopError::Empty) => core::hint::spin_loop(),
                Err(PopError::Corrupt) => panic!("ring buffer corruption"),
            }
        }
    }

    /// Convenience helper used by tests to push without dropping.
    pub fn push_spinning(&self, payload: &[u8]) {
        loop {
            match self.try_push(payload) {
                Ok(()) => return,
                Err(PushError::Full) => core::hint::spin_loop(),
                Err(PushError::TooLarge) => panic!("payload too large for ring buffer"),
            }
        }
    }
}

fn read_u32_le(ptr: *const u8) -> u32 {
    unsafe {
        let mut tmp = [0u8; 4];
        core::ptr::copy_nonoverlapping(ptr, tmp.as_mut_ptr(), 4);
        u32::from_le_bytes(tmp)
    }
}

fn write_u32_le(ptr: *mut u8, v: u32) {
    unsafe {
        let bytes = v.to_le_bytes();
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, 4);
    }
}

/// A `SharedArrayBuffer` ring buffer is represented as:
/// - `ring_ctrl::WORDS` 32-bit control words
/// - followed by a byte array of size `capacity`
///
/// This helper returns the number of bytes required to store the ring buffer.
pub fn bytes_for_ring(capacity_bytes: usize) -> usize {
    ring_ctrl::BYTES + capacity_bytes
}

/// A helper used by tests to compute a conservative payload size for a given
/// capacity.
pub fn max_payload_len_for_capacity(capacity_bytes: usize) -> usize {
    // Worst case: record has 4-byte length + alignment padding.
    capacity_bytes.saturating_sub(4 + (RECORD_ALIGN - 1))
}

/// Compute the number of bytes the next record will consume in the buffer.
pub fn record_size(payload_len: usize) -> usize {
    align_up(4 + payload_len, RECORD_ALIGN)
}
