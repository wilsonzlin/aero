//! Lock-free hardware cursor descriptor shared between multiple workers.
//!
//! This structure is designed to be shared with JavaScript via
//! `SharedArrayBuffer` + `Int32Array`, using atomic operations only.
//!
//! ## Publish protocol
//!
//! This uses the same seqlock-style scheme as [`crate::scanout_state::ScanoutState`]:
//! the high bit of `generation` is treated as a "busy" marker:
//! - The writer sets [`CURSOR_STATE_GENERATION_BUSY_BIT`] before writing fields.
//! - The writer stores the new committed generation (busy bit cleared) as the last step.
//! - Readers spin/retry if the busy bit is set or if the generation changes mid-snapshot.

#[cfg(all(feature = "loom", test))]
use loom::sync::atomic::AtomicU32;
#[cfg(not(all(feature = "loom", test)))]
use std::sync::atomic::AtomicU32;

use std::sync::atomic::Ordering;

use aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat;

/// Cursor format values use the AeroGPU `AerogpuFormat` (`u32`) discriminants.
///
/// Semantics (from the AeroGPU protocol):
/// - `*X8*` formats (`B8G8R8X8*`, `R8G8B8X8*`) do not carry alpha. When converting
///   to RGBA (e.g. for cursor blending), treat alpha as fully opaque (`0xFF`) and
///   ignore the stored `X` byte.
/// - `*_SRGB` variants are layout-identical to their UNORM counterparts; only
///   the color space interpretation differs. Presenters must avoid
///   double-applying gamma when handling sRGB cursor formats.
///
/// This must stay in sync with `aero_protocol::aerogpu::aerogpu_pci::AerogpuFormat`.
pub const CURSOR_FORMAT_B8G8R8A8: u32 = AerogpuFormat::B8G8R8A8Unorm as u32;
pub const CURSOR_FORMAT_B8G8R8X8: u32 = AerogpuFormat::B8G8R8X8Unorm as u32;
pub const CURSOR_FORMAT_R8G8B8A8: u32 = AerogpuFormat::R8G8B8A8Unorm as u32;
pub const CURSOR_FORMAT_R8G8B8X8: u32 = AerogpuFormat::R8G8B8X8Unorm as u32;

/// Internal bit used to mark `generation` as "being updated".
///
/// The published generation value (the one returned from [`CursorState::snapshot`]) never has
/// this bit set and increments by 1 per completed update.
pub const CURSOR_STATE_GENERATION_BUSY_BIT: u32 = 1 << 31;

/// The cursor state is an array of 32-bit words to keep it trivially shareable
/// with JS as an `Int32Array`.
pub const CURSOR_STATE_U32_LEN: usize = 12;
pub const CURSOR_STATE_BYTE_LEN: usize = CURSOR_STATE_U32_LEN * 4;

pub mod header_index {
    //! Indices into the cursor state when viewed as a `u32[]` / `Int32Array`.

    pub const GENERATION: usize = 0;
    pub const ENABLE: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const HOT_X: usize = 4;
    pub const HOT_Y: usize = 5;
    pub const WIDTH: usize = 6;
    pub const HEIGHT: usize = 7;
    pub const PITCH_BYTES: usize = 8;
    pub const FORMAT: usize = 9;
    pub const BASE_PADDR_LO: usize = 10;
    pub const BASE_PADDR_HI: usize = 11;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStateUpdate {
    pub enable: u32,
    /// Cursor position in the scanout coordinate space (top-left origin).
    ///
    /// Stored as an i32 bit-pattern in a u32 word.
    pub x: i32,
    /// Cursor position in the scanout coordinate space (top-left origin).
    ///
    /// Stored as an i32 bit-pattern in a u32 word.
    pub y: i32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: u32,
    pub base_paddr_lo: u32,
    pub base_paddr_hi: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStateSnapshot {
    pub generation: u32,
    pub enable: u32,
    pub x: i32,
    pub y: i32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub pitch_bytes: u32,
    pub format: u32,
    pub base_paddr_lo: u32,
    pub base_paddr_hi: u32,
}

impl CursorStateSnapshot {
    pub fn base_paddr(self) -> u64 {
        (self.base_paddr_hi as u64) << 32 | self.base_paddr_lo as u64
    }
}

#[repr(C)]
pub struct CursorState {
    /// Sequence counter used to publish updates.
    ///
    /// The high bit ([`CURSOR_STATE_GENERATION_BUSY_BIT`]) is used internally to mark an
    /// in-progress update; published generations always have the bit cleared.
    pub generation: AtomicU32,

    pub enable: AtomicU32,
    pub x: AtomicU32,
    pub y: AtomicU32,
    pub hot_x: AtomicU32,
    pub hot_y: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub pitch_bytes: AtomicU32,
    pub format: AtomicU32,
    pub base_paddr_lo: AtomicU32,
    pub base_paddr_hi: AtomicU32,
}

impl CursorState {
    pub fn new() -> Self {
        Self {
            generation: AtomicU32::new(0),
            enable: AtomicU32::new(0),
            x: AtomicU32::new(0),
            y: AtomicU32::new(0),
            hot_x: AtomicU32::new(0),
            hot_y: AtomicU32::new(0),
            width: AtomicU32::new(0),
            height: AtomicU32::new(0),
            pitch_bytes: AtomicU32::new(0),
            format: AtomicU32::new(CURSOR_FORMAT_B8G8R8A8),
            base_paddr_lo: AtomicU32::new(0),
            base_paddr_hi: AtomicU32::new(0),
        }
    }

    /// Publish a complete cursor update.
    ///
    /// Protocol:
    /// 1) Mark the state as "in progress" by setting [`CURSOR_STATE_GENERATION_BUSY_BIT`].
    /// 2) Store all non-generation fields.
    /// 3) Increment `generation` (busy bit cleared) as the final publish step.
    pub fn publish(&self, update: CursorStateUpdate) -> u32 {
        // Acquire the write lock by setting the busy bit.
        let mut start = self.generation.load(Ordering::SeqCst);
        loop {
            if start & CURSOR_STATE_GENERATION_BUSY_BIT != 0 {
                std::hint::spin_loop();
                start = self.generation.load(Ordering::SeqCst);
                continue;
            }

            match self.generation.compare_exchange_weak(
                start,
                start | CURSOR_STATE_GENERATION_BUSY_BIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => start = actual,
            }
        }

        test_yield();

        self.enable.store(update.enable, Ordering::SeqCst);
        test_yield();
        self.x.store(update.x as u32, Ordering::SeqCst);
        test_yield();
        self.y.store(update.y as u32, Ordering::SeqCst);
        test_yield();
        self.hot_x.store(update.hot_x, Ordering::SeqCst);
        test_yield();
        self.hot_y.store(update.hot_y, Ordering::SeqCst);
        test_yield();
        self.width.store(update.width, Ordering::SeqCst);
        test_yield();
        self.height.store(update.height, Ordering::SeqCst);
        test_yield();
        self.pitch_bytes.store(update.pitch_bytes, Ordering::SeqCst);
        test_yield();
        self.format.store(update.format, Ordering::SeqCst);
        test_yield();
        self.base_paddr_lo
            .store(update.base_paddr_lo, Ordering::SeqCst);
        test_yield();
        self.base_paddr_hi
            .store(update.base_paddr_hi, Ordering::SeqCst);

        test_yield();

        // Final publish step: increment generation and clear the busy bit.
        let new_generation = start.wrapping_add(1) & !CURSOR_STATE_GENERATION_BUSY_BIT;
        self.generation.store(new_generation, Ordering::SeqCst);
        new_generation
    }

    pub fn snapshot(&self) -> CursorStateSnapshot {
        loop {
            let gen0 = self.generation.load(Ordering::SeqCst);
            if gen0 & CURSOR_STATE_GENERATION_BUSY_BIT != 0 {
                // Writer in progress.
                std::hint::spin_loop();
                test_yield();
                continue;
            }

            let enable = self.enable.load(Ordering::SeqCst);
            let x = self.x.load(Ordering::SeqCst) as i32;
            let y = self.y.load(Ordering::SeqCst) as i32;
            let hot_x = self.hot_x.load(Ordering::SeqCst);
            let hot_y = self.hot_y.load(Ordering::SeqCst);
            let width = self.width.load(Ordering::SeqCst);
            let height = self.height.load(Ordering::SeqCst);
            let pitch_bytes = self.pitch_bytes.load(Ordering::SeqCst);
            let format = self.format.load(Ordering::SeqCst);
            let base_paddr_lo = self.base_paddr_lo.load(Ordering::SeqCst);
            let base_paddr_hi = self.base_paddr_hi.load(Ordering::SeqCst);

            let gen1 = self.generation.load(Ordering::SeqCst);
            if gen0 != gen1 {
                test_yield();
                continue;
            }

            return CursorStateSnapshot {
                generation: gen0,
                enable,
                x,
                y,
                hot_x,
                hot_y,
                width,
                height,
                pitch_bytes,
                format,
                base_paddr_lo,
                base_paddr_hi,
            };
        }
    }
}

impl Default for CursorState {
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
    fn cursor_format_constants_match_aerogpu_format_discriminants() {
        assert_eq!(CURSOR_FORMAT_B8G8R8A8, AerogpuFormat::B8G8R8A8Unorm as u32);
        assert_eq!(CURSOR_FORMAT_B8G8R8X8, AerogpuFormat::B8G8R8X8Unorm as u32);
        assert_eq!(CURSOR_FORMAT_R8G8B8A8, AerogpuFormat::R8G8B8A8Unorm as u32);
        assert_eq!(CURSOR_FORMAT_R8G8B8X8, AerogpuFormat::R8G8B8X8Unorm as u32);
    }

    #[test]
    fn cursor_state_struct_matches_declared_u32_len() {
        assert_eq!(core::mem::size_of::<CursorState>(), CURSOR_STATE_BYTE_LEN);
    }

    #[test]
    fn header_indices_match_struct_layout() {
        let state = CursorState::new();
        let base = core::ptr::addr_of!(state) as usize;

        let field_offset = |ptr: *const AtomicU32| ptr as usize - base;

        assert_eq!(
            field_offset(core::ptr::addr_of!(state.generation)),
            header_index::GENERATION * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.enable)),
            header_index::ENABLE * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.x)),
            header_index::X * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.y)),
            header_index::Y * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.hot_x)),
            header_index::HOT_X * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.hot_y)),
            header_index::HOT_Y * 4
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
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.base_paddr_lo)),
            header_index::BASE_PADDR_LO * 4
        );
        assert_eq!(
            field_offset(core::ptr::addr_of!(state.base_paddr_hi)),
            header_index::BASE_PADDR_HI * 4
        );
    }

    #[test]
    fn generation_increments_by_one_per_completed_publish() {
        let state = CursorState::new();

        let g0 = state.snapshot().generation;
        state.publish(CursorStateUpdate {
            enable: 1,
            x: 1,
            y: 2,
            hot_x: 3,
            hot_y: 4,
            width: 5,
            height: 6,
            pitch_bytes: 7,
            format: CURSOR_FORMAT_B8G8R8A8,
            base_paddr_lo: 8,
            base_paddr_hi: 9,
        });
        let g1 = state.snapshot().generation;
        state.publish(CursorStateUpdate {
            enable: 0,
            x: 10,
            y: 11,
            hot_x: 12,
            hot_y: 13,
            width: 14,
            height: 15,
            pitch_bytes: 16,
            format: CURSOR_FORMAT_B8G8R8X8,
            base_paddr_lo: 17,
            base_paddr_hi: 18,
        });
        let g2 = state.snapshot().generation;

        assert_eq!(g1, g0.wrapping_add(1));
        assert_eq!(g2, g1.wrapping_add(1));
    }

    #[test]
    fn generation_wraps_without_leaking_busy_bit() {
        let state = CursorState::new();

        // Force the generation near the busy-bit boundary (high bit is reserved).
        state.generation.store(0x7fff_fffe, Ordering::SeqCst);

        let g0 = state.snapshot().generation;
        assert_eq!(g0, 0x7fff_fffe);

        let g1 = state.publish(CursorStateUpdate {
            enable: 0,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: CURSOR_FORMAT_B8G8R8A8,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
        });
        assert_eq!(g1, 0x7fff_ffff);
        assert_eq!(g1 & CURSOR_STATE_GENERATION_BUSY_BIT, 0);

        let g2 = state.publish(CursorStateUpdate {
            enable: 0,
            x: 0,
            y: 0,
            hot_x: 0,
            hot_y: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: CURSOR_FORMAT_B8G8R8A8,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
        });
        assert_eq!(g2, 0x0000_0000);
        assert_eq!(g2 & CURSOR_STATE_GENERATION_BUSY_BIT, 0);
    }

    #[test]
    fn snapshot_is_coherent_across_concurrent_updates() {
        let state = Arc::new(CursorState::new());

        // Initialize to a non-zero state so the reader doesn't have to special-case the default.
        state.publish(CursorStateUpdate {
            enable: 1,
            x: 0,
            y: 1,
            hot_x: 2,
            hot_y: 3,
            width: 4,
            height: 5,
            pitch_bytes: 6,
            format: CURSOR_FORMAT_B8G8R8A8,
            base_paddr_lo: 7,
            base_paddr_hi: 8,
        });

        let start = Arc::new(std::sync::Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));

        let writer_state = state.clone();
        let writer_start = start.clone();
        let writer_done = done.clone();
        let writer = thread::spawn(move || {
            writer_start.wait();

            for token in 0u32..10_000 {
                writer_state.publish(CursorStateUpdate {
                    enable: 1,
                    x: token as i32,
                    y: token.wrapping_add(1) as i32,
                    hot_x: token.wrapping_add(2),
                    hot_y: token.wrapping_add(3),
                    width: token.wrapping_add(4),
                    height: token.wrapping_add(5),
                    pitch_bytes: token.wrapping_add(6),
                    format: CURSOR_FORMAT_B8G8R8A8,
                    base_paddr_lo: token.wrapping_add(7),
                    base_paddr_hi: token.wrapping_add(8),
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

                assert_eq!(snap.format, CURSOR_FORMAT_B8G8R8A8);
                assert_eq!(snap.enable, 1);

                let token = snap.width.wrapping_sub(4);
                assert_eq!(snap.x, token as i32);
                assert_eq!(snap.y, token.wrapping_add(1) as i32);
                assert_eq!(snap.hot_x, token.wrapping_add(2));
                assert_eq!(snap.hot_y, token.wrapping_add(3));
                assert_eq!(snap.width, token.wrapping_add(4));
                assert_eq!(snap.height, token.wrapping_add(5));
                assert_eq!(snap.pitch_bytes, token.wrapping_add(6));
                assert_eq!(snap.base_paddr_lo, token.wrapping_add(7));
                assert_eq!(snap.base_paddr_hi, token.wrapping_add(8));
            }

            // One last snapshot after the writer has finished.
            let snap = reader_state.snapshot();
            assert_eq!(snap.format, CURSOR_FORMAT_B8G8R8A8);
            assert_eq!(snap.enable, 1);
        });

        writer.join().unwrap();
        reader.join().unwrap();
    }
}

#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    use super::*;

    use loom::sync::Arc;
    use loom::thread;

    #[test]
    fn snapshot_never_observes_partial_publish() {
        loom::model(|| {
            let state = Arc::new(CursorState::new());

            // Initialize to a coherent state using the same invariant relation as the update.
            state.publish(CursorStateUpdate {
                enable: 1,
                x: 0,
                y: 1,
                hot_x: 2,
                hot_y: 3,
                width: 4,
                height: 5,
                pitch_bytes: 6,
                format: CURSOR_FORMAT_B8G8R8A8,
                base_paddr_lo: 7,
                base_paddr_hi: 8,
            });

            let writer_state = state.clone();
            let reader_state = state.clone();

            let writer = thread::spawn(move || {
                writer_state.publish(CursorStateUpdate {
                    enable: 1,
                    x: 1,
                    y: 2,
                    hot_x: 3,
                    hot_y: 4,
                    width: 5,
                    height: 6,
                    pitch_bytes: 7,
                    format: CURSOR_FORMAT_B8G8R8A8,
                    base_paddr_lo: 8,
                    base_paddr_hi: 9,
                });
            });

            let reader = thread::spawn(move || {
                let snap = reader_state.snapshot();

                assert_eq!(snap.format, CURSOR_FORMAT_B8G8R8A8);
                assert_eq!(snap.enable, 1);

                let token = snap.width.wrapping_sub(4);
                assert_eq!(snap.x, token as i32);
                assert_eq!(snap.y, token.wrapping_add(1) as i32);
                assert_eq!(snap.hot_x, token.wrapping_add(2));
                assert_eq!(snap.hot_y, token.wrapping_add(3));
                assert_eq!(snap.width, token.wrapping_add(4));
                assert_eq!(snap.height, token.wrapping_add(5));
                assert_eq!(snap.pitch_bytes, token.wrapping_add(6));
                assert_eq!(snap.base_paddr_lo, token.wrapping_add(7));
                assert_eq!(snap.base_paddr_hi, token.wrapping_add(8));
            });

            writer.join().unwrap();
            reader.join().unwrap();
        });
    }
}
