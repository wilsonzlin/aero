//! Lock-free scanout descriptor shared between multiple workers.
//!
//! This structure is designed to be shared with JavaScript via
//! `SharedArrayBuffer` + `Int32Array`, using atomic operations only.

#[cfg(all(feature = "loom", test))]
use loom::sync::atomic::AtomicU32;
#[cfg(not(all(feature = "loom", test)))]
use std::sync::atomic::AtomicU32;

use std::sync::atomic::Ordering;

pub const SCANOUT_SOURCE_LEGACY_TEXT: u32 = 0;
pub const SCANOUT_SOURCE_LEGACY_VBE_LFB: u32 = 1;
pub const SCANOUT_SOURCE_WDDM: u32 = 2;

pub const SCANOUT_FORMAT_B8G8R8X8: u32 = 0;

/// The scanout state is an array of 32-bit words to keep it trivially shareable
/// with JS as an `Int32Array`.
pub const SCANOUT_STATE_U32_LEN: usize = 8;

pub mod header_index {
    //! Indices into the scanout state when viewed as a `u32[]` / `Int32Array`.

    pub const GENERATION: usize = 0;
    pub const SOURCE: usize = 1;
    pub const BASE_PADDR_LO: usize = 2;
    pub const BASE_PADDR_HI: usize = 3;
    pub const WIDTH: usize = 4;
    pub const HEIGHT: usize = 5;
    pub const PITCH_BYTES: usize = 6;
    pub const FORMAT: usize = 7;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanoutStateUpdate {
    pub source: u32,
    pub base_paddr_lo: u32,
    pub base_paddr_hi: u32,
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanoutStateSnapshot {
    pub generation: u32,
    pub source: u32,
    pub base_paddr_lo: u32,
    pub base_paddr_hi: u32,
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: u32,
}

impl ScanoutStateSnapshot {
    pub fn base_paddr(self) -> u64 {
        (self.base_paddr_hi as u64) << 32 | self.base_paddr_lo as u64
    }
}

#[repr(C)]
pub struct ScanoutState {
    /// Sequence counter used to publish updates.
    ///
    /// Stable snapshots always have an even `generation`. Writers make the
    /// counter odd while updating, then increment it again to publish.
    pub generation: AtomicU32,

    pub source: AtomicU32,
    pub base_paddr_lo: AtomicU32,
    pub base_paddr_hi: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub pitch_bytes: AtomicU32,
    pub format: AtomicU32,
}

impl ScanoutState {
    pub fn new() -> Self {
        Self {
            generation: AtomicU32::new(0),
            source: AtomicU32::new(SCANOUT_SOURCE_LEGACY_TEXT),
            base_paddr_lo: AtomicU32::new(0),
            base_paddr_hi: AtomicU32::new(0),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            pitch_bytes: AtomicU32::new(0),
            format: AtomicU32::new(SCANOUT_FORMAT_B8G8R8X8),
        }
    }

    /// Publish a complete scanout update.
    ///
    /// Protocol:
    /// 1) Mark the state as "in progress" by making `generation` odd.
    /// 2) Store all non-generation fields.
    /// 3) Increment `generation` again (even) as the final publish step.
    pub fn publish(&self, update: ScanoutStateUpdate) -> u32 {
        // Make generation odd to indicate an update is in progress.
        let start = self.generation.fetch_add(1, Ordering::SeqCst);
        debug_assert!(start & 1 == 0, "generation should be even before publish");

        test_yield();

        self.source.store(update.source, Ordering::SeqCst);
        test_yield();
        self.base_paddr_lo.store(update.base_paddr_lo, Ordering::SeqCst);
        test_yield();
        self.base_paddr_hi.store(update.base_paddr_hi, Ordering::SeqCst);
        test_yield();
        self.width.store(update.width, Ordering::SeqCst);
        test_yield();
        self.height.store(update.height, Ordering::SeqCst);
        test_yield();
        self.pitch_bytes
            .store(update.pitch_bytes, Ordering::SeqCst);
        test_yield();
        self.format.store(update.format, Ordering::SeqCst);

        test_yield();

        // Final publish step: make generation even again.
        let prev = self.generation.fetch_add(1, Ordering::SeqCst);
        debug_assert!(prev & 1 == 1, "generation should be odd during publish");
        prev.wrapping_add(1)
    }

    pub fn snapshot(&self) -> ScanoutStateSnapshot {
        loop {
            let gen0 = self.generation.load(Ordering::SeqCst);
            if gen0 & 1 != 0 {
                // Writer in progress.
                std::hint::spin_loop();
                continue;
            }

            let source = self.source.load(Ordering::SeqCst);
            let base_paddr_lo = self.base_paddr_lo.load(Ordering::SeqCst);
            let base_paddr_hi = self.base_paddr_hi.load(Ordering::SeqCst);
            let width = self.width.load(Ordering::SeqCst);
            let height = self.height.load(Ordering::SeqCst);
            let pitch_bytes = self.pitch_bytes.load(Ordering::SeqCst);
            let format = self.format.load(Ordering::SeqCst);

            let gen1 = self.generation.load(Ordering::SeqCst);
            if gen0 != gen1 {
                continue;
            }

            return ScanoutStateSnapshot {
                generation: gen0,
                source,
                base_paddr_lo,
                base_paddr_hi,
                width,
                height,
                pitch_bytes,
                format,
            };
        }
    }
}

impl Default for ScanoutState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "loom"))]
#[inline]
fn test_yield() {
    loom::thread::yield_now();
}

#[cfg(all(test, not(feature = "loom")))]
#[inline]
fn test_yield() {
    std::thread::yield_now();
}

#[cfg(not(test))]
#[inline]
fn test_yield() {}

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn scanout_state_struct_matches_declared_u32_len() {
        assert_eq!(
            core::mem::size_of::<ScanoutState>(),
            SCANOUT_STATE_U32_LEN * 4
        );
    }

    #[test]
    fn snapshot_is_coherent_across_concurrent_updates() {
        let state = Arc::new(ScanoutState::new());

        // Initialize to a non-zero state so the reader doesn't have to special-case the default.
        state.publish(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_WDDM,
            base_paddr_lo: 3,
            base_paddr_hi: 4,
            width: 0,
            height: 1,
            pitch_bytes: 2,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });

        let start = Arc::new(std::sync::Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));

        let writer_state = state.clone();
        let writer_start = start.clone();
        let writer_done = done.clone();
        let writer = thread::spawn(move || {
            writer_start.wait();

            for token in 0u32..10_000 {
                writer_state.publish(ScanoutStateUpdate {
                    source: SCANOUT_SOURCE_WDDM,
                    base_paddr_lo: token.wrapping_add(3),
                    base_paddr_hi: token.wrapping_add(4),
                    width: token,
                    height: token.wrapping_add(1),
                    pitch_bytes: token.wrapping_add(2),
                    format: SCANOUT_FORMAT_B8G8R8X8,
                });
            }

            writer_done.store(true, Ordering::SeqCst);
        });

        let reader_state = state.clone();
        let reader_start = start.clone();
        let reader_done = done.clone();
        let reader = thread::spawn(move || {
            reader_start.wait();

            while !reader_done.load(Ordering::SeqCst) {
                let snap = reader_state.snapshot();

                assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
                assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);

                let token = snap.width;
                assert_eq!(snap.height, token.wrapping_add(1));
                assert_eq!(snap.pitch_bytes, token.wrapping_add(2));
                assert_eq!(snap.base_paddr_lo, token.wrapping_add(3));
                assert_eq!(snap.base_paddr_hi, token.wrapping_add(4));
            }

            // One last snapshot after the writer has finished.
            let snap = reader_state.snapshot();
            assert_eq!(snap.source, SCANOUT_SOURCE_WDDM);
            assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}

