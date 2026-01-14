//! Lock-free scanout descriptor shared between multiple workers.
//!
//! This structure is designed to be shared with JavaScript via
//! `SharedArrayBuffer` + `Int32Array`, using atomic operations only.
//!
//! ## Publish protocol
//!
//! A naive "write fields, then increment generation" scheme is not sufficient because a reader
//! could observe a mix of old/new fields while `generation` is still unchanged.
//!
//! Instead we use a seqlock-style scheme where the high bit of `generation` is treated as a
//! "busy" marker:
//! - The writer sets [`SCANOUT_STATE_GENERATION_BUSY_BIT`] before writing fields.
//! - The writer stores the new committed generation (busy bit cleared) as the last step.
//! - Readers spin/retry if the busy bit is set or if the generation changes mid-snapshot.

#[cfg(all(feature = "loom", test))]
use loom::sync::atomic::AtomicU32;
#[cfg(not(all(feature = "loom", test)))]
use std::sync::atomic::AtomicU32;

use std::sync::atomic::Ordering;

use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

pub const SCANOUT_SOURCE_LEGACY_TEXT: u32 = 0;
pub const SCANOUT_SOURCE_LEGACY_VBE_LFB: u32 = 1;
pub const SCANOUT_SOURCE_WDDM: u32 = 2;

/// Scanout format values use the AeroGPU `AerogpuFormat` (`u32`) discriminants.
///
/// Semantics (from the AeroGPU protocol):
/// - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha. When converting
///   to RGBA (e.g. for scanout presentation/cursor blending), treat alpha as
///   fully opaque (`0xFF`) and ignore the stored `X` byte.
/// - `*_SRGB` variants are layout-identical to their UNORM counterparts; only
///   the color space interpretation differs. Presenters must avoid
///   double-applying gamma when handling sRGB scanout formats.
///
/// This must stay in sync with `aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat`.
pub const SCANOUT_FORMAT_B8G8R8X8: u32 = AerogpuFormat::B8G8R8X8Unorm as u32;
pub const SCANOUT_FORMAT_B8G8R8A8: u32 = AerogpuFormat::B8G8R8A8Unorm as u32;
pub const SCANOUT_FORMAT_R8G8B8A8: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
pub const SCANOUT_FORMAT_R8G8B8X8: u32 = AerogpuFormat::R8G8B8X8Unorm as u32;
pub const SCANOUT_FORMAT_B8G8R8X8_SRGB: u32 = AerogpuFormat::B8G8R8X8UnormSrgb as u32;
pub const SCANOUT_FORMAT_B8G8R8A8_SRGB: u32 = AerogpuFormat::B8G8R8A8UnormSrgb as u32;
pub const SCANOUT_FORMAT_R8G8B8A8_SRGB: u32 = AerogpuFormat::R8G8B8A8UnormSrgb as u32;
pub const SCANOUT_FORMAT_R8G8B8X8_SRGB: u32 = AerogpuFormat::R8G8B8X8UnormSrgb as u32;

/// Internal bit used to mark `generation` as "being updated".
///
/// The published generation value (the one returned from [`ScanoutState::snapshot`]) never has
/// this bit set and increments by 1 per completed update.
pub const SCANOUT_STATE_GENERATION_BUSY_BIT: u32 = 1 << 31;

/// The scanout state is an array of 32-bit words to keep it trivially shareable
/// with JS as an `Int32Array`.
pub const SCANOUT_STATE_U32_LEN: usize = 8;
pub const SCANOUT_STATE_BYTE_LEN: usize = SCANOUT_STATE_U32_LEN * 4;

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
    /// Pixel format stored as an AeroGPU `AerogpuFormat` (`u32`) discriminant.
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
    /// Pixel format stored as an AeroGPU `AerogpuFormat` (`u32`) discriminant.
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
    /// The high bit ([`SCANOUT_STATE_GENERATION_BUSY_BIT`]) is used internally to mark an
    /// in-progress update; published generations always have the bit cleared.
    pub generation: AtomicU32,

    pub source: AtomicU32,
    pub base_paddr_lo: AtomicU32,
    pub base_paddr_hi: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub pitch_bytes: AtomicU32,
    /// Pixel format stored as an AeroGPU `AerogpuFormat` (`u32`) discriminant.
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
    /// 1) Mark the state as "in progress" by setting [`SCANOUT_STATE_GENERATION_BUSY_BIT`].
    /// 2) Store all non-generation fields.
    /// 3) Increment `generation` (busy bit cleared) as the final publish step.
    pub fn publish(&self, update: ScanoutStateUpdate) -> u32 {
        // Acquire the write lock by setting the busy bit.
        let mut start = self.generation.load(Ordering::SeqCst);
        loop {
            if start & SCANOUT_STATE_GENERATION_BUSY_BIT != 0 {
                std::hint::spin_loop();
                start = self.generation.load(Ordering::SeqCst);
                continue;
            }

            match self.generation.compare_exchange_weak(
                start,
                start | SCANOUT_STATE_GENERATION_BUSY_BIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => start = actual,
            }
        }

        test_yield();

        self.source.store(update.source, Ordering::SeqCst);
        test_yield();
        self.base_paddr_lo
            .store(update.base_paddr_lo, Ordering::SeqCst);
        test_yield();
        self.base_paddr_hi
            .store(update.base_paddr_hi, Ordering::SeqCst);
        test_yield();
        self.width.store(update.width, Ordering::SeqCst);
        test_yield();
        self.height.store(update.height, Ordering::SeqCst);
        test_yield();
        self.pitch_bytes.store(update.pitch_bytes, Ordering::SeqCst);
        test_yield();
        self.format.store(update.format, Ordering::SeqCst);

        test_yield();

        // Final publish step: increment generation and clear the busy bit.
        let new_generation = start.wrapping_add(1) & !SCANOUT_STATE_GENERATION_BUSY_BIT;
        self.generation.store(new_generation, Ordering::SeqCst);
        new_generation
    }

    pub fn snapshot(&self) -> ScanoutStateSnapshot {
        loop {
            let gen0 = self.generation.load(Ordering::SeqCst);
            if gen0 & SCANOUT_STATE_GENERATION_BUSY_BIT != 0 {
                // Writer in progress.
                std::hint::spin_loop();
                test_yield();
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
                test_yield();
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
    fn scanout_state_defaults_match_protocol() {
        let state = ScanoutState::new();
        let snap = state.snapshot();
        assert_eq!(snap.generation, 0);
        assert_eq!(snap.source, SCANOUT_SOURCE_LEGACY_TEXT);
        assert_eq!(snap.format, SCANOUT_FORMAT_B8G8R8X8);
        assert_eq!(SCANOUT_FORMAT_B8G8R8X8, AerogpuFormat::B8G8R8X8Unorm as u32);
    }

    #[test]
    fn scanout_format_default_matches_aerogpu_protocol() {
        assert_eq!(SCANOUT_FORMAT_B8G8R8X8, AerogpuFormat::B8G8R8X8Unorm as u32);
    }

    #[test]
    fn scanout_state_struct_matches_declared_u32_len() {
        assert_eq!(core::mem::size_of::<ScanoutState>(), SCANOUT_STATE_BYTE_LEN);
    }

    #[test]
    fn header_indices_match_struct_layout() {
        let state = ScanoutState::new();
        let base = core::ptr::addr_of!(state) as usize;

        let field_offset = |ptr: *const AtomicU32| ptr as usize - base;

        assert_eq!(
            field_offset(core::ptr::addr_of!(state.generation)),
            header_index::GENERATION * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.source)),
            header_index::SOURCE * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.base_paddr_lo)),
            header_index::BASE_PADDR_LO * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.base_paddr_hi)),
            header_index::BASE_PADDR_HI * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.width)),
            header_index::WIDTH * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.height)),
            header_index::HEIGHT * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.pitch_bytes)),
            header_index::PITCH_BYTES * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.format)),
            header_index::FORMAT * 4
        );
    }

    #[test]
    fn scanout_format_constants_match_aerogpu_format_discriminants() {
        // 0 is reserved by the AeroGPU protocol for "Invalid", so scanout format values must not
        // use custom numbering.
        assert_eq!(AerogpuFormat::Invalid as u32, 0);
        assert_eq!(SCANOUT_FORMAT_B8G8R8X8, AerogpuFormat::B8G8R8X8Unorm as u32);
        assert_eq!(SCANOUT_FORMAT_B8G8R8A8, AerogpuFormat::B8G8R8A8Unorm as u32);
        assert_eq!(SCANOUT_FORMAT_R8G8B8A8, AerogpuFormat::R8G8B8A8Unorm as u32);
        assert_eq!(SCANOUT_FORMAT_R8G8B8X8, AerogpuFormat::R8G8B8X8Unorm as u32);
        assert_eq!(
            SCANOUT_FORMAT_B8G8R8X8_SRGB,
            AerogpuFormat::B8G8R8X8UnormSrgb as u32
        );
        assert_eq!(
            SCANOUT_FORMAT_B8G8R8A8_SRGB,
            AerogpuFormat::B8G8R8A8UnormSrgb as u32
        );
        assert_eq!(
            SCANOUT_FORMAT_R8G8B8A8_SRGB,
            AerogpuFormat::R8G8B8A8UnormSrgb as u32
        );
        assert_eq!(
            SCANOUT_FORMAT_R8G8B8X8_SRGB,
            AerogpuFormat::R8G8B8X8UnormSrgb as u32
        );
    }

    #[test]
    fn generation_increments_by_one_per_completed_publish() {
        let state = ScanoutState::new();

        let g0 = state.snapshot().generation;
        state.publish(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 1,
            height: 2,
            pitch_bytes: 3,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });
        let g1 = state.snapshot().generation;
        state.publish(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 4,
            height: 5,
            pitch_bytes: 6,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });
        let g2 = state.snapshot().generation;

        assert_eq!(g1, g0.wrapping_add(1));
        assert_eq!(g2, g1.wrapping_add(1));
    }

    #[test]
    fn generation_wraps_without_leaking_busy_bit() {
        let state = ScanoutState::new();

        // Force the generation near the busy-bit boundary (high bit is reserved).
        state.generation.store(0x7fff_fffe, Ordering::SeqCst);

        let g0 = state.snapshot().generation;
        assert_eq!(g0, 0x7fff_fffe);

        let g1 = state.publish(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });
        assert_eq!(g1, 0x7fff_ffff);
        assert_eq!(g1 & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);

        let g2 = state.publish(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });
        assert_eq!(g2, 0x0000_0000);
        assert_eq!(g2 & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);
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

#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    // Loom concurrency tests.
    //
    // Run with:
    // `cargo test -p aero-shared --features loom`
    use super::*;

    use loom::sync::Arc;
    use loom::thread;

    #[test]
    fn publish_snapshot_protocol_holds_under_contention() {
        const GEN_MASK: u32 = !SCANOUT_STATE_GENERATION_BUSY_BIT;
        const WRITER_PUBLISHES: usize = 2;
        const SNAPSHOTS_PER_READER: usize = 2;

        fn update_for_generation(generation: u32) -> ScanoutStateUpdate {
            // Encode the published generation into every field so that readers can detect:
            // - observing a "new" generation with "old" fields (generation published too early),
            // - observing a mix of fields from multiple publishes.
            //
            // Use wrapping arithmetic so the test remains valid across the 31-bit wrap boundary.
            ScanoutStateUpdate {
                source: generation.wrapping_add(10),
                base_paddr_lo: generation.wrapping_add(11),
                base_paddr_hi: generation.wrapping_add(12),
                width: generation.wrapping_add(13),
                height: generation.wrapping_add(14),
                pitch_bytes: generation.wrapping_add(15),
                format: generation.wrapping_add(16),
            }
        }

        fn assert_snapshot_is_self_consistent(snap: ScanoutStateSnapshot) {
            assert_eq!(snap.generation & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);
            let g = snap.generation;

            assert_eq!(snap.source, g.wrapping_add(10));
            assert_eq!(snap.base_paddr_lo, g.wrapping_add(11));
            assert_eq!(snap.base_paddr_hi, g.wrapping_add(12));
            assert_eq!(snap.width, g.wrapping_add(13));
            assert_eq!(snap.height, g.wrapping_add(14));
            assert_eq!(snap.pitch_bytes, g.wrapping_add(15));
            assert_eq!(snap.format, g.wrapping_add(16));
        }

        fn assert_generation_monotonic(prev: u32, curr: u32, max_delta: u32) {
            assert_eq!(prev & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);
            assert_eq!(curr & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);

            // Treat `generation` as a 31-bit wrapping counter (high bit reserved as busy bit).
            let delta = (curr.wrapping_sub(prev)) & GEN_MASK;
            assert!(
                delta <= max_delta,
                "generation went backwards: prev={prev:#010x}, curr={curr:#010x}, delta={delta:#010x}"
            );
        }

        // Keep the model small, but ensure we still explore interleavings where:
        // - the writer is mid-publish while readers snapshot,
        // - the counter wraps (31-bit counter; bit 31 is reserved).
        // This code intentionally uses spin-wait loops (both in `publish` and `snapshot`).
        // Raise Loom's branch limit so the model checker can explore enough schedules to
        // validate the protocol without aborting early.
        let mut builder = loom::model::Builder::new();
        builder.max_branches = 100_000;
        builder.max_permutations = Some(10_000);
        builder.check(|| {
            let state = Arc::new(ScanoutState::new());

            // Initialize to a coherent state up-front (before any concurrent access).
            //
            // We start near the wrap boundary so this short model crosses it. The high bit is
            // reserved as the busy bit.
            let initial_gen = 0x7fff_fffe;
            let init = update_for_generation(initial_gen);
            state.source.store(init.source, Ordering::SeqCst);
            state
                .base_paddr_lo
                .store(init.base_paddr_lo, Ordering::SeqCst);
            state
                .base_paddr_hi
                .store(init.base_paddr_hi, Ordering::SeqCst);
            state.width.store(init.width, Ordering::SeqCst);
            state.height.store(init.height, Ordering::SeqCst);
            state.pitch_bytes.store(init.pitch_bytes, Ordering::SeqCst);
            state.format.store(init.format, Ordering::SeqCst);
            state.generation.store(initial_gen, Ordering::SeqCst);

            let writer_state = state.clone();
            let writer = thread::spawn(move || {
                for _ in 0..WRITER_PUBLISHES {
                    let cur = writer_state.generation.load(Ordering::SeqCst) & GEN_MASK;
                    let next = cur.wrapping_add(1) & GEN_MASK;
                    let published = writer_state.publish(update_for_generation(next));
                    assert_eq!(published, next);
                    assert_eq!(published & SCANOUT_STATE_GENERATION_BUSY_BIT, 0);
                }
            });

            let reader_state = state.clone();
            let reader = thread::spawn(move || {
                let mut last_gen: Option<u32> = None;
                for _ in 0..SNAPSHOTS_PER_READER {
                    let snap = reader_state.snapshot();
                    assert_snapshot_is_self_consistent(snap);

                    if let Some(prev) = last_gen {
                        assert_generation_monotonic(prev, snap.generation, WRITER_PUBLISHES as u32);
                    }
                    last_gen = Some(snap.generation);
                }
            });

            writer.join().unwrap();
            reader.join().unwrap();

            // Final snapshot should still be coherent.
            assert_snapshot_is_self_consistent(state.snapshot());
        });
    }
}
