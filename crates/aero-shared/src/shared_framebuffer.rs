use core::fmt;
use core::ptr::NonNull;
use core::slice;

#[cfg(all(feature = "loom", test))]
use loom::sync::atomic::AtomicU32;
#[cfg(not(all(feature = "loom", test)))]
use std::sync::atomic::AtomicU32;

use std::sync::atomic::Ordering;

/// Magic value stored in [`SharedFramebufferHeader::magic`].
///
/// Written as `0xA3F0_FB01` to be unlikely to collide with uninitialized memory.
pub const SHARED_FRAMEBUFFER_MAGIC: u32 = 0xA3F0_FB01;
pub const SHARED_FRAMEBUFFER_VERSION: u32 = 1;

/// Double-buffered: slot 0 and slot 1.
pub const SHARED_FRAMEBUFFER_SLOTS: usize = 2;

/// Header is a fixed array of 32-bit atomics so both Rust (WASM) and JS can
/// access it via `AtomicU32` / `Int32Array + Atomics`.
pub const SHARED_FRAMEBUFFER_HEADER_U32_LEN: usize = 16;
pub const SHARED_FRAMEBUFFER_HEADER_BYTE_LEN: usize = SHARED_FRAMEBUFFER_HEADER_U32_LEN * 4;

/// All byte offsets are aligned to this to keep the framebuffer payload aligned
/// for fast host-side copies and GPU texture uploads.
pub const SHARED_FRAMEBUFFER_ALIGNMENT: usize = 64;

pub mod header_index {
    //! Indices into the header when viewed as a `u32[]` / `Int32Array`.

    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 1;
    pub const WIDTH: usize = 2;
    pub const HEIGHT: usize = 3;
    pub const STRIDE_BYTES: usize = 4;
    pub const FORMAT: usize = 5;
    pub const ACTIVE_INDEX: usize = 6;
    pub const FRAME_SEQ: usize = 7;
    pub const FRAME_DIRTY: usize = 8;
    pub const TILE_SIZE: usize = 9;
    pub const TILES_X: usize = 10;
    pub const TILES_Y: usize = 11;
    pub const DIRTY_WORDS_PER_BUFFER: usize = 12;
    pub const BUF0_FRAME_SEQ: usize = 13;
    pub const BUF1_FRAME_SEQ: usize = 14;
    pub const FLAGS: usize = 15;
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramebufferFormat {
    /// 8-bit per component RGBA, byte order `[R, G, B, A]`.
    Rgba8 = 0,
}

impl FramebufferFormat {
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            FramebufferFormat::Rgba8 => 4,
        }
    }
}

impl TryFrom<u32> for FramebufferFormat {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(FramebufferFormat::Rgba8),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedFramebufferLayout {
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: FramebufferFormat,

    /// Tile edge length in pixels. If `0`, dirty tracking is disabled.
    pub tile_size: u32,
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub dirty_words_per_buffer: u32,

    header_bytes: usize,
    buffer_bytes: usize,
    framebuffer_offsets: [usize; SHARED_FRAMEBUFFER_SLOTS],
    dirty_offsets: [usize; SHARED_FRAMEBUFFER_SLOTS],
    total_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    ZeroSized,
    StrideTooSmall {
        stride_bytes: u32,
        min_stride_bytes: u32,
    },
    TileSizeNotPowerOfTwo(u32),
    SizeOverflow(&'static str),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LayoutError::ZeroSized => write!(f, "framebuffer width/height must be non-zero"),
            LayoutError::StrideTooSmall {
                stride_bytes,
                min_stride_bytes,
            } => write!(
                f,
                "stride_bytes ({stride_bytes}) is smaller than minimum ({min_stride_bytes})"
            ),
            LayoutError::TileSizeNotPowerOfTwo(tile_size) => write!(
                f,
                "tile_size ({tile_size}) must be 0 (disabled) or a power-of-two"
            ),
            LayoutError::SizeOverflow(what) => write!(f, "size overflow while computing {what}"),
        }
    }
}

impl SharedFramebufferLayout {
    pub fn new_rgba8(width: u32, height: u32, tile_size: u32) -> Result<Self, LayoutError> {
        let stride_bytes = width.saturating_mul(FramebufferFormat::Rgba8.bytes_per_pixel());
        Self::new(
            width,
            height,
            stride_bytes,
            FramebufferFormat::Rgba8,
            tile_size,
        )
    }

    pub fn new(
        width: u32,
        height: u32,
        stride_bytes: u32,
        format: FramebufferFormat,
        tile_size: u32,
    ) -> Result<Self, LayoutError> {
        if width == 0 || height == 0 {
            return Err(LayoutError::ZeroSized);
        }

        let min_stride_bytes = width.saturating_mul(format.bytes_per_pixel());
        if stride_bytes < min_stride_bytes {
            return Err(LayoutError::StrideTooSmall {
                stride_bytes,
                min_stride_bytes,
            });
        }

        if tile_size != 0 && !tile_size.is_power_of_two() {
            return Err(LayoutError::TileSizeNotPowerOfTwo(tile_size));
        }

        let buffer_bytes = (stride_bytes as usize)
            .checked_mul(height as usize)
            .ok_or(LayoutError::SizeOverflow("framebuffer byte length"))?;

        let (tiles_x, tiles_y, dirty_words_per_buffer) = if tile_size == 0 {
            (0, 0, 0)
        } else {
            let tiles_x = width.div_ceil(tile_size);
            let tiles_y = height.div_ceil(tile_size);
            let tile_count = (tiles_x as usize)
                .checked_mul(tiles_y as usize)
                .ok_or(LayoutError::SizeOverflow("tile_count"))?;
            let dirty_words = u32::try_from(tile_count.div_ceil(32))
                .map_err(|_| LayoutError::SizeOverflow("dirty_words_per_buffer"))?;
            (tiles_x, tiles_y, dirty_words)
        };

        let header_bytes = SHARED_FRAMEBUFFER_HEADER_BYTE_LEN;

        let mut cursor = align_up(header_bytes, SHARED_FRAMEBUFFER_ALIGNMENT)?;

        let slot0_fb = cursor;
        cursor = align_up(
            slot0_fb
                .checked_add(buffer_bytes)
                .ok_or(LayoutError::SizeOverflow("slot0_fb end"))?,
            4,
        )?;
        let slot0_dirty = cursor;
        cursor = align_up(
            slot0_dirty
                .checked_add(
                    (dirty_words_per_buffer as usize)
                        .checked_mul(4)
                        .ok_or(LayoutError::SizeOverflow("dirty_words_per_buffer bytes"))?,
                )
                .ok_or(LayoutError::SizeOverflow("slot0 dirty end"))?,
            SHARED_FRAMEBUFFER_ALIGNMENT,
        )?;

        let slot1_fb = cursor;
        cursor = align_up(
            slot1_fb
                .checked_add(buffer_bytes)
                .ok_or(LayoutError::SizeOverflow("slot1_fb end"))?,
            4,
        )?;
        let slot1_dirty = cursor;
        cursor = align_up(
            slot1_dirty
                .checked_add(
                    (dirty_words_per_buffer as usize)
                        .checked_mul(4)
                        .ok_or(LayoutError::SizeOverflow("dirty_words_per_buffer bytes"))?,
                )
                .ok_or(LayoutError::SizeOverflow("slot1 dirty end"))?,
            SHARED_FRAMEBUFFER_ALIGNMENT,
        )?;

        Ok(Self {
            width,
            height,
            stride_bytes,
            format,
            tile_size,
            tiles_x,
            tiles_y,
            dirty_words_per_buffer,
            header_bytes,
            buffer_bytes,
            framebuffer_offsets: [slot0_fb, slot1_fb],
            dirty_offsets: [slot0_dirty, slot1_dirty],
            total_bytes: cursor,
        })
    }

    pub fn header_byte_len(self) -> usize {
        self.header_bytes
    }

    pub fn buffer_byte_len(self) -> usize {
        self.buffer_bytes
    }

    pub fn total_byte_len(self) -> usize {
        self.total_bytes
    }

    pub fn framebuffer_offset_bytes(self, index: usize) -> usize {
        self.framebuffer_offsets[index]
    }

    pub fn dirty_offset_bytes(self, index: usize) -> Option<usize> {
        if self.dirty_words_per_buffer == 0 {
            None
        } else {
            Some(self.dirty_offsets[index])
        }
    }

    pub fn tile_count(self) -> usize {
        self.tiles_x as usize * self.tiles_y as usize
    }
}

#[repr(C)]
pub struct SharedFramebufferHeader {
    pub magic: AtomicU32,
    pub version: AtomicU32,
    pub width: AtomicU32,
    pub height: AtomicU32,
    pub stride_bytes: AtomicU32,
    pub format: AtomicU32,
    pub active_index: AtomicU32,
    pub frame_seq: AtomicU32,
    pub frame_dirty: AtomicU32,
    pub tile_size: AtomicU32,
    pub tiles_x: AtomicU32,
    pub tiles_y: AtomicU32,
    pub dirty_words_per_buffer: AtomicU32,
    /// For diagnostics: the last `frame_seq` written into buffer slot 0.
    pub buf0_frame_seq: AtomicU32,
    /// For diagnostics: the last `frame_seq` written into buffer slot 1.
    pub buf1_frame_seq: AtomicU32,
    pub flags: AtomicU32,
}

impl SharedFramebufferHeader {
    pub fn init(&self, layout: SharedFramebufferLayout) {
        self.magic.store(SHARED_FRAMEBUFFER_MAGIC, Ordering::SeqCst);
        self.version
            .store(SHARED_FRAMEBUFFER_VERSION, Ordering::SeqCst);
        self.width.store(layout.width, Ordering::SeqCst);
        self.height.store(layout.height, Ordering::SeqCst);
        self.stride_bytes
            .store(layout.stride_bytes, Ordering::SeqCst);
        self.format.store(layout.format as u32, Ordering::SeqCst);
        self.active_index.store(0, Ordering::SeqCst);
        self.frame_seq.store(0, Ordering::SeqCst);
        self.frame_dirty.store(0, Ordering::SeqCst);
        self.tile_size.store(layout.tile_size, Ordering::SeqCst);
        self.tiles_x.store(layout.tiles_x, Ordering::SeqCst);
        self.tiles_y.store(layout.tiles_y, Ordering::SeqCst);
        self.dirty_words_per_buffer
            .store(layout.dirty_words_per_buffer, Ordering::SeqCst);
        self.buf0_frame_seq.store(0, Ordering::SeqCst);
        self.buf1_frame_seq.store(0, Ordering::SeqCst);
        self.flags.store(0, Ordering::SeqCst);
    }

    pub fn snapshot(&self) -> SharedFramebufferHeaderSnapshot {
        SharedFramebufferHeaderSnapshot {
            magic: self.magic.load(Ordering::SeqCst),
            version: self.version.load(Ordering::SeqCst),
            width: self.width.load(Ordering::SeqCst),
            height: self.height.load(Ordering::SeqCst),
            stride_bytes: self.stride_bytes.load(Ordering::SeqCst),
            format: self.format.load(Ordering::SeqCst),
            active_index: self.active_index.load(Ordering::SeqCst),
            frame_seq: self.frame_seq.load(Ordering::SeqCst),
            frame_dirty: self.frame_dirty.load(Ordering::SeqCst),
            tile_size: self.tile_size.load(Ordering::SeqCst),
            tiles_x: self.tiles_x.load(Ordering::SeqCst),
            tiles_y: self.tiles_y.load(Ordering::SeqCst),
            dirty_words_per_buffer: self.dirty_words_per_buffer.load(Ordering::SeqCst),
            buf0_frame_seq: self.buf0_frame_seq.load(Ordering::SeqCst),
            buf1_frame_seq: self.buf1_frame_seq.load(Ordering::SeqCst),
            flags: self.flags.load(Ordering::SeqCst),
        }
    }

    pub fn buffer_frame_seq(&self, buffer_index: usize) -> &AtomicU32 {
        match buffer_index {
            0 => &self.buf0_frame_seq,
            1 => &self.buf1_frame_seq,
            _ => unreachable!("buffer_index must be 0 or 1"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SharedFramebufferHeaderSnapshot {
    pub magic: u32,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: u32,
    pub active_index: u32,
    pub frame_seq: u32,
    pub frame_dirty: u32,
    pub tile_size: u32,
    pub tiles_x: u32,
    pub tiles_y: u32,
    pub dirty_words_per_buffer: u32,
    pub buf0_frame_seq: u32,
    pub buf1_frame_seq: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedFramebufferError {
    NullBasePtr,
    UnalignedBasePtr { addr: usize },
}

impl fmt::Display for SharedFramebufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SharedFramebufferError::NullBasePtr => {
                write!(f, "shared framebuffer base pointer is null")
            }
            SharedFramebufferError::UnalignedBasePtr { addr } => {
                write!(
                    f,
                    "shared framebuffer base pointer (0x{addr:x}) is not 4-byte aligned"
                )
            }
        }
    }
}

/// A view over a shared-memory framebuffer region.
///
/// This intentionally uses raw pointers internally because the underlying
/// memory is mutated concurrently by other threads/workers. Safety relies on
/// following the publish protocol (write to the back buffer, then publish by
/// atomically updating `active_index` + `frame_seq`).
#[derive(Clone, Copy)]
pub struct SharedFramebuffer {
    base: NonNull<u8>,
    layout: SharedFramebufferLayout,
}

// `SharedFramebuffer` is a handle to shared memory and may be sent between
// threads/workers as long as the producer/consumer follow the atomic publish
// protocol. The underlying pointer does not imply ownership.
unsafe impl Send for SharedFramebuffer {}
unsafe impl Sync for SharedFramebuffer {}

impl SharedFramebuffer {
    /// # Safety
    /// `base` must point to a region of memory that is at least
    /// `layout.total_byte_len()` bytes long and is shared between producer and
    /// consumer.
    pub unsafe fn from_raw_parts(
        base: *mut u8,
        layout: SharedFramebufferLayout,
    ) -> Result<Self, SharedFramebufferError> {
        let Some(base) = NonNull::new(base) else {
            return Err(SharedFramebufferError::NullBasePtr);
        };
        let addr = base.as_ptr() as usize;
        if !addr.is_multiple_of(4) {
            return Err(SharedFramebufferError::UnalignedBasePtr { addr });
        }

        Ok(Self { base, layout })
    }

    pub fn layout(self) -> SharedFramebufferLayout {
        self.layout
    }

    pub fn header(&self) -> &SharedFramebufferHeader {
        // Safety: caller validated `base` points at a shared framebuffer header.
        unsafe { &*(self.base.as_ptr() as *const SharedFramebufferHeader) }
    }

    pub fn framebuffer_ptr(&self, index: usize) -> *mut u8 {
        assert!(index < SHARED_FRAMEBUFFER_SLOTS);
        // Safety: offset is within the region by construction.
        unsafe {
            self.base
                .as_ptr()
                .add(self.layout.framebuffer_offset_bytes(index))
        }
    }

    pub fn framebuffer(&self, index: usize) -> &[u8] {
        // Safety: region is shared; reading is safe if producer does not write to the active buffer.
        unsafe { slice::from_raw_parts(self.framebuffer_ptr(index), self.layout.buffer_byte_len()) }
    }

    /// # Safety
    /// The caller must ensure no other thread/worker is concurrently reading
    /// from the returned buffer (i.e. only write to the back buffer).
    pub unsafe fn framebuffer_mut(&self, index: usize) -> *mut [u8] {
        core::ptr::slice_from_raw_parts_mut(
            self.framebuffer_ptr(index),
            self.layout.buffer_byte_len(),
        )
    }

    pub fn dirty_words_ptr(&self, index: usize) -> Option<*mut u32> {
        let offset = self.layout.dirty_offset_bytes(index)?;
        let ptr = unsafe { self.base.as_ptr().add(offset) } as *mut u32;
        Some(ptr)
    }

    pub fn dirty_words(&self, index: usize) -> Option<&[u32]> {
        let ptr = self.dirty_words_ptr(index)?;
        let len = self.layout.dirty_words_per_buffer as usize;
        // Safety: slice points into the shared region and is 4-byte aligned by construction.
        Some(unsafe { slice::from_raw_parts(ptr as *const u32, len) })
    }

    /// # Safety
    /// The caller must ensure no other thread/worker is concurrently reading
    /// from the returned buffer (i.e. only write dirty state for the back buffer).
    pub unsafe fn dirty_words_mut(&self, index: usize) -> Option<*mut [u32]> {
        let ptr = self.dirty_words_ptr(index)?;
        let len = self.layout.dirty_words_per_buffer as usize;
        Some(core::ptr::slice_from_raw_parts_mut(ptr, len))
    }
}

pub struct SharedFramebufferWriter {
    shared: SharedFramebuffer,
}

impl SharedFramebufferWriter {
    pub fn new(shared: SharedFramebuffer) -> Self {
        Self { shared }
    }

    /// Write into the back buffer, then publish it as the active buffer.
    ///
    /// This performs the publish step in the order required by the JS `Atomics`
    /// consumer:
    /// 1. `bufN_frame_seq = new_seq`
    /// 2. `active_index = N`
    /// 3. `frame_seq = new_seq` (last; used as the `Atomics.wait` address)
    /// 4. `frame_dirty = 1` (consumer may clear after it is finished copying/presenting)
    ///
    /// Note: double buffering alone does not prevent producer/consumer overlap if the producer
    /// publishes faster than the consumer can process, because the buffer the consumer is reading
    /// becomes the *next* back buffer. When the backing store is shared across threads (e.g. a
    /// threaded wasm build using `WebAssembly.Memory` + `atomics`), that overlap can become a Rust
    /// data race (UB). Producers may avoid this by treating `frame_dirty` as a best-effort consumer
    /// acknowledgement (ACK) and throttling publishing until it is cleared.
    pub fn write_frame<F>(&self, f: F) -> u32
    where
        F: FnOnce(&mut [u8], Option<&mut [u32]>, SharedFramebufferLayout),
    {
        let header = self.shared.header();

        // The header lives in shared memory and can be corrupted. Clamp to a valid slot so we
        // don't panic when computing the back buffer.
        let active = (header.active_index.load(Ordering::SeqCst) & 1) as usize;
        let back = active ^ 1;

        // Safety: producer only writes to the back buffer.
        let back_buffer = unsafe { &mut *self.shared.framebuffer_mut(back) };
        let mut dirty_words = unsafe { self.shared.dirty_words_mut(back).map(|ptr| &mut *ptr) };

        if let Some(words) = dirty_words.as_deref_mut() {
            words.fill(0);
        }

        f(back_buffer, dirty_words, self.shared.layout());

        let new_seq = header.frame_seq.load(Ordering::SeqCst).wrapping_add(1);

        header
            .buffer_frame_seq(back)
            .store(new_seq, Ordering::SeqCst);
        header.active_index.store(back as u32, Ordering::SeqCst);
        header.frame_seq.store(new_seq, Ordering::SeqCst);
        header.frame_dirty.store(1, Ordering::SeqCst);

        new_seq
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirtyRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Convert a per-tile dirty bitset into pixel rects, merging runs on the X axis.
pub fn dirty_tiles_to_rects(
    layout: SharedFramebufferLayout,
    dirty_words: &[u32],
) -> Vec<DirtyRect> {
    const MAX_DIRTY_RECTS: usize = 65_536;

    if layout.tile_size == 0 || layout.dirty_words_per_buffer == 0 {
        return vec![DirtyRect {
            x: 0,
            y: 0,
            width: layout.width,
            height: layout.height,
        }];
    }

    let tile_count = layout.tile_count();
    if tile_count == 0 {
        return Vec::new();
    }

    if dirty_words_cover_all_tiles(tile_count, dirty_words) {
        return vec![DirtyRect {
            x: 0,
            y: 0,
            width: layout.width,
            height: layout.height,
        }];
    }

    let mut rects = Vec::new();

    let tiles_x = layout.tiles_x as usize;
    let tiles_y = layout.tiles_y as usize;
    let tile_size = layout.tile_size;

    for ty in 0..tiles_y {
        let y = (ty as u64) * u64::from(tile_size);
        let framebuffer_h = u64::from(layout.height);
        if y >= framebuffer_h {
            break;
        }

        let mut tx = 0usize;
        while tx < tiles_x {
            let tile_index = ty * tiles_x + tx;
            if !dirty_bit_is_set(dirty_words, tile_index) {
                tx += 1;
                continue;
            }

            let start_tx = tx;
            tx += 1;
            while tx < tiles_x {
                let next_index = ty * tiles_x + tx;
                if !dirty_bit_is_set(dirty_words, next_index) {
                    break;
                }
                tx += 1;
            }

            let x = (start_tx as u64) * u64::from(tile_size);
            let framebuffer_w = u64::from(layout.width);
            let mut width = (tx - start_tx) as u64 * u64::from(tile_size);
            if x.saturating_add(width) > framebuffer_w {
                width = framebuffer_w.saturating_sub(x);
            }

            let mut height = u64::from(tile_size);
            if y.saturating_add(height) > framebuffer_h {
                height = framebuffer_h.saturating_sub(y);
            }

            if rects.len() >= MAX_DIRTY_RECTS || rects.try_reserve(1).is_err() {
                return vec![DirtyRect {
                    x: 0,
                    y: 0,
                    width: layout.width,
                    height: layout.height,
                }];
            }

            rects.push(DirtyRect {
                x: x as u32,
                y: y as u32,
                width: width as u32,
                height: height as u32,
            });
        }
    }

    rects
}

fn dirty_words_cover_all_tiles(tile_count: usize, dirty_words: &[u32]) -> bool {
    if tile_count == 0 {
        return true;
    }

    let full_words = tile_count / 32;
    let remaining = tile_count % 32;

    if dirty_words.len() < full_words + if remaining == 0 { 0 } else { 1 } {
        return false;
    }

    for word in dirty_words.iter().take(full_words) {
        if *word != u32::MAX {
            return false;
        }
    }

    if remaining == 0 {
        return true;
    }

    let mask = (1u32 << remaining) - 1;
    (dirty_words[full_words] & mask) == mask
}

fn dirty_bit_is_set(words: &[u32], tile_index: usize) -> bool {
    let word = tile_index / 32;
    let bit = tile_index % 32;
    (words.get(word).copied().unwrap_or(0) & (1u32 << bit)) != 0
}

fn align_up(value: usize, align: usize) -> Result<usize, LayoutError> {
    debug_assert!(align.is_power_of_two());
    value
        .checked_add(align - 1)
        .map(|v| v & !(align - 1))
        .ok_or(LayoutError::SizeOverflow("align_up"))
}

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    struct Backing {
        layout: SharedFramebufferLayout,
        words: Vec<u32>,
    }

    impl Backing {
        fn new(layout: SharedFramebufferLayout) -> Self {
            let word_len = layout.total_byte_len().div_ceil(4);
            Self {
                layout,
                words: vec![0; word_len],
            }
        }

        fn shared(&mut self) -> SharedFramebuffer {
            // Safety: backing store is 4-byte aligned due to `Vec<u32>`.
            unsafe {
                SharedFramebuffer::from_raw_parts(self.words.as_mut_ptr() as *mut u8, self.layout)
                    .unwrap()
            }
        }
    }

    #[test]
    fn header_struct_matches_declared_u32_len() {
        assert_eq!(
            core::mem::size_of::<SharedFramebufferHeader>(),
            SHARED_FRAMEBUFFER_HEADER_BYTE_LEN
        );
    }

    #[test]
    fn layout_is_stable_and_aligned() {
        let layout = SharedFramebufferLayout::new_rgba8(640, 480, 32).unwrap();
        assert_eq!(layout.header_byte_len(), SHARED_FRAMEBUFFER_HEADER_BYTE_LEN);
        assert_eq!(layout.buffer_byte_len(), 640 * 4 * 480);

        assert_eq!(
            layout.framebuffer_offset_bytes(0) % SHARED_FRAMEBUFFER_ALIGNMENT,
            0
        );
        assert_eq!(
            layout.framebuffer_offset_bytes(1) % SHARED_FRAMEBUFFER_ALIGNMENT,
            0
        );
        assert!(layout
            .total_byte_len()
            .is_multiple_of(SHARED_FRAMEBUFFER_ALIGNMENT));

        // Dirty words should be enough to cover all tiles.
        let tile_count = layout.tile_count();
        let expected_words = tile_count.div_ceil(32) as u32;
        assert_eq!(layout.dirty_words_per_buffer, expected_words);
    }

    #[test]
    fn layout_rejects_total_size_overflow() {
        // Two framebuffer slots can overflow `usize` even though the individual dimensions fit in
        // `u32`.
        let err = SharedFramebufferLayout::new(
            u32::MAX,
            u32::MAX,
            u32::MAX,
            FramebufferFormat::Rgba8,
            /*tile_size=*/ 0,
        )
        .unwrap_err();
        assert!(matches!(err, LayoutError::SizeOverflow(_)));
    }

    #[test]
    fn layout_rejects_dirty_word_count_overflow() {
        let err = SharedFramebufferLayout::new(
            u32::MAX,
            u32::MAX,
            u32::MAX,
            FramebufferFormat::Rgba8,
            /*tile_size=*/ 1,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LayoutError::SizeOverflow("dirty_words_per_buffer")
        ));
    }

    #[test]
    fn publish_protocol_never_reads_back_buffer() {
        let layout = SharedFramebufferLayout::new_rgba8(32, 32, 32).unwrap();
        let mut backing = Backing::new(layout);
        let shared = backing.shared();
        shared.header().init(layout);

        let writer = SharedFramebufferWriter::new(shared);
        let shared_for_reader = shared;

        let ready = Arc::new(std::sync::Barrier::new(2));
        let ready_reader = ready.clone();

        let handle = thread::spawn(move || {
            ready_reader.wait();
            let header = shared_for_reader.header();

            for seq in 1..=8u32 {
                while header.frame_dirty.load(Ordering::SeqCst) == 0 {
                    thread::yield_now();
                }

                let observed_seq = header.frame_seq.load(Ordering::SeqCst);
                assert_eq!(observed_seq, seq);

                let active = header.active_index.load(Ordering::SeqCst) as usize;
                assert_eq!(active, (seq as usize) & 1);

                let buf_seq = header.buffer_frame_seq(active).load(Ordering::SeqCst);
                assert_eq!(buf_seq, seq);

                let framebuffer = shared_for_reader.framebuffer(active);
                assert_eq!(framebuffer[0], seq as u8);

                header.frame_dirty.store(0, Ordering::SeqCst);
            }
        });

        ready.wait();

        let header = shared.header();
        for seq in 1..=8u32 {
            writer.write_frame(|buf, dirty, layout| {
                buf.fill(seq as u8);
                if let Some(words) = dirty {
                    // Mark everything dirty.
                    let tile_count = layout.tile_count();
                    for idx in 0..tile_count {
                        let word = idx / 32;
                        let bit = idx % 32;
                        words[word] |= 1u32 << bit;
                    }
                }
            });

            while header.frame_dirty.load(Ordering::SeqCst) != 0 {
                thread::yield_now();
            }
        }

        handle.join().unwrap();
    }

    #[test]
    fn dirty_tile_rects_merge_runs_horizontally() {
        let layout = SharedFramebufferLayout::new_rgba8(64, 64, 32).unwrap();
        assert_eq!(layout.tiles_x, 2);
        assert_eq!(layout.tiles_y, 2);
        assert_eq!(layout.dirty_words_per_buffer, 1);

        // Mark top row tiles dirty (tiles 0 and 1).
        let dirty_words = [0b11u32];
        let rects = dirty_tiles_to_rects(layout, &dirty_words);

        assert_eq!(
            rects,
            vec![DirtyRect {
                x: 0,
                y: 0,
                width: 64,
                height: 32
            }]
        );
    }

    #[test]
    fn dirty_tile_rects_clamp_width_when_sum_exceeds_u32_max() {
        // Regression test: `x + width` can overflow `u32` when the framebuffer width is near
        // `u32::MAX` and the tile grid rounds up.
        let tile_size = 1u32 << 30;
        let layout = SharedFramebufferLayout::new(
            u32::MAX,
            1,
            u32::MAX,
            FramebufferFormat::Rgba8,
            tile_size,
        )
        .unwrap();
        assert_eq!(layout.tiles_x, 4);
        assert_eq!(layout.tiles_y, 1);

        // Mark only the last tile dirty.
        let dirty_words = [0b1000u32];
        let rects = dirty_tiles_to_rects(layout, &dirty_words);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 3 * tile_size);
        assert_eq!(rects[0].width, tile_size - 1);
        assert_eq!(rects[0].height, 1);
    }

    #[test]
    fn dirty_tile_rects_clamp_height_when_sum_exceeds_u32_max() {
        let tile_size = 1u32 << 30;
        let layout =
            SharedFramebufferLayout::new(1, u32::MAX, 4, FramebufferFormat::Rgba8, tile_size)
                .unwrap();
        assert_eq!(layout.tiles_x, 1);
        assert_eq!(layout.tiles_y, 4);

        // Mark only the last row tile dirty (tile_index=3).
        let dirty_words = [0b1000u32];
        let rects = dirty_tiles_to_rects(layout, &dirty_words);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].y, 3 * tile_size);
        assert_eq!(rects[0].height, tile_size - 1);
        assert_eq!(rects[0].width, 1);
    }

    #[test]
    fn dirty_tile_rects_fall_back_to_full_frame_when_too_many_rects() {
        // Alternating dirty tiles can create a very large rect list (one rect per dirty tile per
        // row). When the list would grow too large, we conservatively fall back to a single
        // full-frame dirty rect.
        let width = 1025;
        let height = 128;
        let stride_bytes = width * 4;
        let layout = SharedFramebufferLayout::new(
            width,
            height,
            stride_bytes,
            FramebufferFormat::Rgba8,
            /*tile_size=*/ 1,
        )
        .unwrap();

        let tiles_x = layout.tiles_x as usize;
        let tiles_y = layout.tiles_y as usize;

        let mut dirty_words = vec![0u32; layout.dirty_words_per_buffer as usize];
        for ty in 0..tiles_y {
            for tx in (0..tiles_x).step_by(2) {
                let tile_index = ty * tiles_x + tx;
                let word = tile_index / 32;
                let bit = tile_index % 32;
                dirty_words[word] |= 1u32 << bit;
            }
        }

        let rects = dirty_tiles_to_rects(layout, &dirty_words);
        assert_eq!(
            rects,
            vec![DirtyRect {
                x: 0,
                y: 0,
                width,
                height,
            }]
        );
    }

    #[test]
    fn writer_clamps_active_index_when_corrupted() {
        let layout = SharedFramebufferLayout::new_rgba8(32, 32, 32).unwrap();
        let mut backing = Backing::new(layout);
        let shared = backing.shared();
        shared.header().init(layout);

        // Corrupt the header: active_index must be 0 or 1, but the writer should tolerate other
        // values by clamping to a valid slot.
        shared.header().active_index.store(2, Ordering::SeqCst);

        let writer = SharedFramebufferWriter::new(shared);
        writer.write_frame(|buf, _dirty, _layout| {
            buf.fill(0xAA);
        });

        // active_index=2 -> clamped active=0 -> back=1.
        assert_eq!(shared.header().active_index.load(Ordering::SeqCst), 1);
        assert_eq!(shared.framebuffer(1)[0], 0xAA);
    }
}

#[cfg(all(test, feature = "loom"))]
mod loom_tests {
    use super::*;

    use loom::sync::atomic::AtomicU32;
    use loom::sync::Arc;
    use loom::thread;

    struct Model {
        active_index: AtomicU32,
        frame_seq: AtomicU32,
        buf0_frame_seq: AtomicU32,
        buf1_frame_seq: AtomicU32,
        pixel0: AtomicU32,
        pixel1: AtomicU32,
    }

    #[test]
    fn publish_makes_back_buffer_visible() {
        loom::model(|| {
            let model = Arc::new(Model {
                active_index: AtomicU32::new(0),
                frame_seq: AtomicU32::new(0),
                buf0_frame_seq: AtomicU32::new(0),
                buf1_frame_seq: AtomicU32::new(0),
                pixel0: AtomicU32::new(0),
                pixel1: AtomicU32::new(0),
            });

            // Start with buffer 0 active and seq 0.
            model.active_index.store(0, Ordering::SeqCst);
            model.frame_seq.store(0, Ordering::SeqCst);

            let writer_model = model.clone();
            let reader_model = model.clone();

            let writer = thread::spawn(move || {
                let active = writer_model.active_index.load(Ordering::SeqCst) as usize;
                let back = active ^ 1;

                let new_seq = writer_model
                    .frame_seq
                    .load(Ordering::SeqCst)
                    .wrapping_add(1);
                match back {
                    0 => {
                        writer_model.pixel0.store(new_seq, Ordering::SeqCst);
                        writer_model.buf0_frame_seq.store(new_seq, Ordering::SeqCst);
                    }
                    1 => {
                        writer_model.pixel1.store(new_seq, Ordering::SeqCst);
                        writer_model.buf1_frame_seq.store(new_seq, Ordering::SeqCst);
                    }
                    _ => unreachable!(),
                }

                writer_model
                    .active_index
                    .store(back as u32, Ordering::SeqCst);
                writer_model.frame_seq.store(new_seq, Ordering::SeqCst);
            });

            let reader = thread::spawn(move || {
                while reader_model.frame_seq.load(Ordering::SeqCst) == 0 {
                    thread::yield_now();
                }

                let seq = reader_model.frame_seq.load(Ordering::SeqCst);
                let active = reader_model.active_index.load(Ordering::SeqCst) as usize;

                let (buf_seq, pixel) = match active {
                    0 => (
                        reader_model.buf0_frame_seq.load(Ordering::SeqCst),
                        reader_model.pixel0.load(Ordering::SeqCst),
                    ),
                    1 => (
                        reader_model.buf1_frame_seq.load(Ordering::SeqCst),
                        reader_model.pixel1.load(Ordering::SeqCst),
                    ),
                    _ => return,
                };

                assert_eq!(buf_seq, seq);
                assert_eq!(pixel, seq);
            });

            writer.join().unwrap();
            reader.join().unwrap();
        });
    }
}
