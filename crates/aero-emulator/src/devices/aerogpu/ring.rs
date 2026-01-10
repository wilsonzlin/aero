use super::protocol::{align_ring, CmdHeader, Opcode, RING_ALIGNMENT};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingPushError {
    Full,
    InvalidAlignment,
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingCorruptInfo {
    pub opcode: u32,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingPopError {
    Corrupt(RingCorruptInfo),
}

/// Lock-free single-producer/single-consumer ring buffer for variable-sized entries.
///
/// The ring stores entries as little-endian bytes, but the backing storage is a `u32` array of
/// atomics to keep the implementation safe under `#![forbid(unsafe_code)]` while still allowing
/// concurrent access.
pub struct RingBuffer {
    capacity_bytes: u32,
    head: AtomicU32,
    tail: AtomicU32,
    words: Box<[AtomicU32]>,
    wait_lock: Mutex<()>,
    wait_cv: Condvar,
}

impl RingBuffer {
    pub fn new(capacity_bytes: u32) -> Self {
        assert!(capacity_bytes as usize % RING_ALIGNMENT == 0);
        assert!(capacity_bytes >= (RING_ALIGNMENT as u32) * 2);
        assert!(capacity_bytes % 4 == 0, "ring must be u32-addressable");

        let capacity_words = (capacity_bytes / 4) as usize;
        let words = std::iter::repeat_with(|| AtomicU32::new(0))
            .take(capacity_words)
            .collect::<Vec<_>>()
            .into_boxed_slice();

        Self {
            capacity_bytes,
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            words,
            wait_lock: Mutex::new(()),
            wait_cv: Condvar::new(),
        }
    }

    pub fn split(self) -> (Arc<Self>, RingProducer, RingConsumer) {
        let shared = Arc::new(self);
        let prod = RingProducer::new(Arc::clone(&shared));
        let cons = RingConsumer::new(Arc::clone(&shared));
        (shared, prod, cons)
    }

    pub fn producer(self: &Arc<Self>) -> RingProducer {
        RingProducer::new(Arc::clone(self))
    }

    pub fn consumer(self: &Arc<Self>) -> RingConsumer {
        RingConsumer::new(Arc::clone(self))
    }

    pub fn reset(&self) {
        self.head.store(0, Ordering::Release);
        self.tail.store(0, Ordering::Release);
        self.wait_cv.notify_all();
    }

    pub fn head_mod(&self) -> u32 {
        self.head.load(Ordering::Acquire) % self.capacity_bytes
    }

    pub fn tail_mod(&self) -> u32 {
        self.tail.load(Ordering::Acquire) % self.capacity_bytes
    }

    fn write_words(&self, byte_offset: usize, bytes: &[u8]) {
        debug_assert_eq!(byte_offset % 4, 0);
        debug_assert_eq!(bytes.len() % 4, 0);
        let first_word = byte_offset / 4;
        for (i, chunk) in bytes.chunks_exact(4).enumerate() {
            let val = u32::from_le_bytes(chunk.try_into().expect("4 bytes"));
            self.words[first_word + i].store(val, Ordering::Relaxed);
        }
    }

    fn zero_words(&self, byte_offset: usize, byte_len: usize) {
        debug_assert_eq!(byte_offset % 4, 0);
        debug_assert_eq!(byte_len % 4, 0);
        let first_word = byte_offset / 4;
        let word_len = byte_len / 4;
        for w in &self.words[first_word..first_word + word_len] {
            w.store(0, Ordering::Relaxed);
        }
    }

    fn read_words_to_bytes(&self, byte_offset: usize, out: &mut [u8]) {
        debug_assert_eq!(byte_offset % 4, 0);
        debug_assert_eq!(out.len() % 4, 0);
        let first_word = byte_offset / 4;
        for (i, chunk) in out.chunks_exact_mut(4).enumerate() {
            let val = self.words[first_word + i].load(Ordering::Relaxed);
            chunk.copy_from_slice(&val.to_le_bytes());
        }
    }
}

#[derive(Clone)]
pub struct RingProducer {
    ring: Arc<RingBuffer>,
}

impl RingProducer {
    pub(crate) fn new(ring: Arc<RingBuffer>) -> Self {
        Self { ring }
    }

    pub fn try_push(&self, bytes: &[u8]) -> Result<(), RingPushError> {
        let len = bytes.len();
        if len == 0 || len % RING_ALIGNMENT != 0 {
            return Err(RingPushError::InvalidAlignment);
        }
        if len > self.ring.capacity_bytes as usize {
            return Err(RingPushError::TooLarge);
        }

        let head = self.ring.head.load(Ordering::Acquire);
        let tail = self.ring.tail.load(Ordering::Relaxed);
        let used = tail.wrapping_sub(head);
        debug_assert!(used <= self.ring.capacity_bytes);
        let free = self.ring.capacity_bytes - used;

        let cap = self.ring.capacity_bytes as usize;
        let tail_off = (tail % self.ring.capacity_bytes) as usize;

        let mut total = len as u32;
        let mut wrote_wrap = false;
        if tail_off + len > cap {
            let pad = cap - tail_off;
            debug_assert!(pad >= CmdHeader::SIZE_BYTES);
            let pad = align_ring(pad) as u32;
            total = total.wrapping_add(pad);
            wrote_wrap = true;
        }

        if free < total {
            return Err(RingPushError::Full);
        }

        if wrote_wrap {
            let pad = align_ring(cap - tail_off);
            let hdr = CmdHeader {
                opcode: Opcode::NOP,
                size_bytes: pad as u32,
            };
            self.ring
                .write_words(tail_off, &hdr.encode().as_slice()[..]);
            // Zero remaining padding for deterministic testing.
            self.ring.zero_words(
                tail_off + CmdHeader::SIZE_BYTES,
                pad - CmdHeader::SIZE_BYTES,
            );
        }

        let write_off = if wrote_wrap { 0 } else { tail_off };
        self.ring.write_words(write_off, bytes);

        self.ring
            .tail
            .store(tail.wrapping_add(total), Ordering::Release);
        self.ring.wait_cv.notify_all();
        Ok(())
    }

    pub fn push_blocking(&self, bytes: &[u8]) -> Result<(), RingPushError> {
        let mut guard = self.ring.wait_lock.lock().expect("mutex poisoned");
        loop {
            match self.try_push(bytes) {
                Ok(()) => return Ok(()),
                Err(RingPushError::Full) => {
                    guard = self.ring.wait_cv.wait(guard).expect("mutex poisoned");
                }
                Err(e) => return Err(e),
            }
        }
    }
}

pub struct RingConsumer {
    ring: Arc<RingBuffer>,
}

impl RingConsumer {
    pub(crate) fn new(ring: Arc<RingBuffer>) -> Self {
        Self { ring }
    }

    pub fn try_pop(&self) -> Result<Option<Vec<u8>>, RingPopError> {
        loop {
            let head = self.ring.head.load(Ordering::Relaxed);
            let tail = self.ring.tail.load(Ordering::Acquire);
            if head == tail {
                return Ok(None);
            }

            let cap = self.ring.capacity_bytes as usize;
            let head_off = (head % self.ring.capacity_bytes) as usize;

            if head_off + CmdHeader::SIZE_BYTES > cap {
                self.ring.reset();
                return Err(RingPopError::Corrupt(RingCorruptInfo {
                    opcode: 0,
                    size_bytes: 0,
                }));
            }

            // Decode header via atomic u32 loads.
            let opcode = self.ring.words[head_off / 4].load(Ordering::Relaxed);
            let size_bytes = self.ring.words[head_off / 4 + 1].load(Ordering::Relaxed);
            let size_usize = size_bytes as usize;

            if size_usize == 0 || size_usize % RING_ALIGNMENT != 0 || size_usize > cap {
                self.ring.reset();
                return Err(RingPopError::Corrupt(RingCorruptInfo { opcode, size_bytes }));
            }
            if head_off + size_usize > cap {
                self.ring.reset();
                return Err(RingPopError::Corrupt(RingCorruptInfo { opcode, size_bytes }));
            }

            if opcode == Opcode::NOP {
                self.ring.head.store(head.wrapping_add(size_bytes), Ordering::Release);
                self.ring.wait_cv.notify_all();
                continue;
            }

            let mut out = vec![0u8; size_usize];
            self.ring.read_words_to_bytes(head_off, &mut out);
            self.ring.head.store(head.wrapping_add(size_bytes), Ordering::Release);
            self.ring.wait_cv.notify_all();
            return Ok(Some(out));
        }
    }

    pub fn pop_blocking(&self) -> Result<Vec<u8>, RingPopError> {
        let mut guard = self.ring.wait_lock.lock().expect("mutex poisoned");
        loop {
            match self.try_pop()? {
                Some(bytes) => return Ok(bytes),
                None => {}
            }
            guard = self.ring.wait_cv.wait(guard).expect("mutex poisoned");
        }
    }
}
