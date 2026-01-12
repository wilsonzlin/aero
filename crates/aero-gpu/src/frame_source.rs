use core::fmt;

use crate::dirty_rect::Rect;
use aero_shared::shared_framebuffer::{
    dirty_tiles_to_rects, DirtyRect, FramebufferFormat, LayoutError, SharedFramebuffer,
    SharedFramebufferError, SharedFramebufferHeader, SharedFramebufferHeaderSnapshot,
    SharedFramebufferLayout, SHARED_FRAMEBUFFER_MAGIC, SHARED_FRAMEBUFFER_VERSION,
};
use std::sync::atomic::Ordering;

#[derive(Debug)]
pub enum FrameSourceError {
    SharedFramebuffer(SharedFramebufferError),
    InvalidLayout(LayoutError),
    BadMagic {
        found: u32,
    },
    BadVersion {
        found: u32,
    },
    UnsupportedFormat {
        found: u32,
    },
    DirtyWordsMismatch {
        expected: u32,
        found: u32,
    },
    TilesMismatch {
        expected_x: u32,
        expected_y: u32,
        found_x: u32,
        found_y: u32,
    },
}

impl fmt::Display for FrameSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameSourceError::SharedFramebuffer(err) => write!(f, "{err}"),
            FrameSourceError::InvalidLayout(err) => write!(f, "{err}"),
            FrameSourceError::BadMagic { found } => {
                write!(f, "bad shared framebuffer magic 0x{found:08x}")
            }
            FrameSourceError::BadVersion { found } => {
                write!(f, "unsupported shared framebuffer version {found}")
            }
            FrameSourceError::UnsupportedFormat { found } => {
                write!(f, "unsupported framebuffer format {found}")
            }
            FrameSourceError::DirtyWordsMismatch { expected, found } => write!(
                f,
                "dirty_words_per_buffer mismatch: expected {expected}, header has {found}"
            ),
            FrameSourceError::TilesMismatch {
                expected_x,
                expected_y,
                found_x,
                found_y,
            } => write!(
                f,
                "tiles mismatch: expected {expected_x}x{expected_y}, header has {found_x}x{found_y}"
            ),
        }
    }
}

pub struct FrameSource {
    shared: SharedFramebuffer,
    last_seq: u32,
}

impl FrameSource {
    /// Create a [`FrameSource`] by locating a shared framebuffer region inside a larger shared memory.
    ///
    /// # Safety
    /// `memory_base` must be a valid pointer to the beginning of a shared memory region (e.g. the
    /// start of WASM linear memory), and `framebuffer_offset_bytes` must point at a
    /// [`SharedFramebufferHeader`] followed by the framebuffer slots as defined by the header.
    pub unsafe fn from_shared_memory(
        memory_base: *mut u8,
        framebuffer_offset_bytes: usize,
    ) -> Result<Self, FrameSourceError> {
        let region_base = memory_base.add(framebuffer_offset_bytes);
        let header = &*(region_base as *const SharedFramebufferHeader);

        let magic = header.magic.load(Ordering::SeqCst);
        if magic != SHARED_FRAMEBUFFER_MAGIC {
            return Err(FrameSourceError::BadMagic { found: magic });
        }

        let version = header.version.load(Ordering::SeqCst);
        if version != SHARED_FRAMEBUFFER_VERSION {
            return Err(FrameSourceError::BadVersion { found: version });
        }

        let width = header.width.load(Ordering::SeqCst);
        let height = header.height.load(Ordering::SeqCst);
        let stride_bytes = header.stride_bytes.load(Ordering::SeqCst);

        let format_raw = header.format.load(Ordering::SeqCst);
        let format = FramebufferFormat::try_from(format_raw)
            .map_err(|_| FrameSourceError::UnsupportedFormat { found: format_raw })?;

        let tile_size = header.tile_size.load(Ordering::SeqCst);

        let layout = SharedFramebufferLayout::new(width, height, stride_bytes, format, tile_size)
            .map_err(FrameSourceError::InvalidLayout)?;

        let dirty_words = header.dirty_words_per_buffer.load(Ordering::SeqCst);
        if dirty_words != layout.dirty_words_per_buffer {
            return Err(FrameSourceError::DirtyWordsMismatch {
                expected: layout.dirty_words_per_buffer,
                found: dirty_words,
            });
        }

        let tiles_x = header.tiles_x.load(Ordering::SeqCst);
        let tiles_y = header.tiles_y.load(Ordering::SeqCst);
        if tiles_x != layout.tiles_x || tiles_y != layout.tiles_y {
            return Err(FrameSourceError::TilesMismatch {
                expected_x: layout.tiles_x,
                expected_y: layout.tiles_y,
                found_x: tiles_x,
                found_y: tiles_y,
            });
        }

        let shared = SharedFramebuffer::from_raw_parts(region_base, layout)
            .map_err(FrameSourceError::SharedFramebuffer)?;
        let last_seq = header.frame_seq.load(Ordering::SeqCst);

        Ok(Self { shared, last_seq })
    }

    pub fn shared(&self) -> SharedFramebuffer {
        self.shared
    }

    pub fn poll_frame(&mut self) -> Option<FrameView<'_>> {
        let header = self.shared.header();
        let seq = header.frame_seq.load(Ordering::SeqCst);
        if seq == self.last_seq {
            return None;
        }
        self.last_seq = seq;

        let active_index = header.active_index.load(Ordering::SeqCst) as usize;
        let active_buf_seq = header.buffer_frame_seq(active_index).load(Ordering::SeqCst);

        // Clear the "new frame" flag. The CPU sets this to 1 on publish; clearing it
        // is optional but allows basic monitoring of consumer liveness.
        header.frame_dirty.store(0, Ordering::SeqCst);

        let pixels = self.shared.framebuffer(active_index);
        let layout = self.shared.layout();

        let dirty = if layout.dirty_words_per_buffer == 0 {
            DirtyHint::FullFrame
        } else {
            let words = self.shared.dirty_words(active_index).unwrap_or_default();
            if words.iter().all(|w| *w == 0) {
                DirtyHint::FullFrame
            } else {
                DirtyHint::Tiles(DirtyTilesView { layout, words })
            }
        };

        Some(FrameView {
            seq,
            active_index,
            active_buf_seq,
            width: layout.width,
            height: layout.height,
            stride_bytes: layout.stride_bytes,
            format: layout.format,
            pixels,
            dirty,
        })
    }
}

pub struct FrameView<'a> {
    pub seq: u32,
    pub active_index: usize,
    /// Diagnostic: the per-buffer sequence value stored alongside the payload.
    pub active_buf_seq: u32,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub format: FramebufferFormat,
    pub pixels: &'a [u8],
    pub dirty: DirtyHint<'a>,
}

impl FrameView<'_> {
    /// Convert dirty tracking information into `aero-gpu`'s [`Rect`] type for use with
    /// [`crate::Presenter`].
    ///
    /// Returns `None` to indicate "treat as full frame" (i.e. pass `dirty=None` to
    /// `Presenter::present`), which is both the fallback when dirty tracking is disabled
    /// and the most efficient path when the entire frame is dirty.
    pub fn dirty_rects_for_presenter(&self) -> Option<Vec<Rect>> {
        match &self.dirty {
            DirtyHint::FullFrame => None,
            DirtyHint::Tiles(tiles) => {
                let rects: Vec<DirtyRect> = tiles.rects();
                if rects.len() == 1
                    && rects[0].x == 0
                    && rects[0].y == 0
                    && rects[0].width == self.width
                    && rects[0].height == self.height
                {
                    // We can't see the layout here, so we treat a single rect at origin as a
                    // likely full-frame update and fall back to the full-frame path.
                    //
                    // Callers can still override this if they want to always upload via rects.
                    return None;
                }

                let mut out = Vec::new();
                if out.try_reserve_exact(rects.len()).is_err() {
                    // Conservative fallback: if we can't allocate the rect list, upload the full
                    // frame instead.
                    return None;
                }
                for r in rects {
                    out.push(Rect::new(r.x, r.y, r.width, r.height));
                }
                Some(out)
            }
        }
    }
}

pub enum DirtyHint<'a> {
    FullFrame,
    Tiles(DirtyTilesView<'a>),
}

pub struct DirtyTilesView<'a> {
    layout: SharedFramebufferLayout,
    words: &'a [u32],
}

impl DirtyTilesView<'_> {
    pub fn rects(&self) -> Vec<DirtyRect> {
        dirty_tiles_to_rects(self.layout, self.words)
    }

    pub fn words(&self) -> &[u32] {
        self.words
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameInspectorSnapshot {
    pub header: SharedFramebufferHeaderSnapshot,
    pub buffer_hashes: [u64; 2],
    pub publish_order_ok: bool,
    pub problems: Vec<FrameInspectorProblem>,
    pub buffers: Option<[Vec<u8>; 2]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameInspectorProblem {
    BadMagic {
        found: u32,
    },
    BadVersion {
        found: u32,
    },
    ActiveIndexOutOfRange {
        found: u32,
    },
    PublishOrderMismatch {
        active_index: u32,
        frame_seq: u32,
        active_buffer_seq: u32,
    },
}

pub struct FrameInspector {
    shared: SharedFramebuffer,
}

impl FrameInspector {
    pub fn new(source: &FrameSource) -> Self {
        Self {
            shared: source.shared,
        }
    }

    pub fn inspect(&self, snapshot_buffers: bool) -> FrameInspectorSnapshot {
        let header = self.shared.header().snapshot();

        let buffer_hashes = [
            fnv1a64(self.shared.framebuffer(0)),
            fnv1a64(self.shared.framebuffer(1)),
        ];

        let mut problems = Vec::new();

        if header.magic != SHARED_FRAMEBUFFER_MAGIC {
            problems.push(FrameInspectorProblem::BadMagic {
                found: header.magic,
            });
        }

        if header.version != SHARED_FRAMEBUFFER_VERSION {
            problems.push(FrameInspectorProblem::BadVersion {
                found: header.version,
            });
        }

        let active_buffer_seq = match header.active_index {
            0 => Some(header.buf0_frame_seq),
            1 => Some(header.buf1_frame_seq),
            _ => {
                problems.push(FrameInspectorProblem::ActiveIndexOutOfRange {
                    found: header.active_index,
                });
                None
            }
        };

        let publish_order_ok = active_buffer_seq.is_some_and(|seq| seq == header.frame_seq);
        if let Some(active_buffer_seq) = active_buffer_seq {
            if active_buffer_seq != header.frame_seq {
                problems.push(FrameInspectorProblem::PublishOrderMismatch {
                    active_index: header.active_index,
                    frame_seq: header.frame_seq,
                    active_buffer_seq,
                });
            }
        }

        let buffers = if snapshot_buffers {
            Some([
                self.shared.framebuffer(0).to_vec(),
                self.shared.framebuffer(1).to_vec(),
            ])
        } else {
            None
        };

        FrameInspectorSnapshot {
            header,
            buffer_hashes,
            publish_order_ok,
            problems,
            buffers,
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_shared::shared_framebuffer::{
        SharedFramebufferWriter, SHARED_FRAMEBUFFER_HEADER_BYTE_LEN,
    };

    #[test]
    fn from_shared_memory_reads_frames_and_dirty_tiles() {
        let layout = SharedFramebufferLayout::new_rgba8(32, 32, 32).unwrap();
        let word_len = layout.total_byte_len().div_ceil(4);
        let mut words = vec![0u32; word_len];

        let shared = unsafe {
            SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout).unwrap()
        };
        shared.header().init(layout);

        // Sanity: header starts at byte 0 in this backing store.
        assert_eq!(SHARED_FRAMEBUFFER_HEADER_BYTE_LEN, 64);

        let mut source =
            unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();

        assert!(source.poll_frame().is_none());

        let writer = SharedFramebufferWriter::new(shared);
        writer.write_frame(|buf, dirty, layout| {
            buf.fill(0x11);
            if let Some(words) = dirty {
                let tile_count = layout.tile_count();
                for idx in 0..tile_count {
                    let word = idx / 32;
                    let bit = idx % 32;
                    words[word] |= 1u32 << bit;
                }
            }
        });

        let frame = source.poll_frame().expect("new frame");
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.pixels[0], 0x11);
        assert_eq!(frame.active_buf_seq, 1);

        // For a full-frame dirty bitset, the presenter path should take the full-frame
        // upload shortcut (`dirty=None`) rather than emitting an explicit rect list.
        assert_eq!(frame.dirty_rects_for_presenter(), None);

        match frame.dirty {
            DirtyHint::FullFrame => panic!("expected tile dirty hint"),
            DirtyHint::Tiles(ref tiles) => {
                let rects = tiles.rects();
                assert_eq!(rects.len(), 1);
                assert_eq!(rects[0].width, 32);
                assert_eq!(rects[0].height, 32);
            }
        }

        let inspector = FrameInspector::new(&source);
        let snapshot = inspector.inspect(false);
        assert_eq!(snapshot.header.magic, SHARED_FRAMEBUFFER_MAGIC);
        assert!(snapshot.publish_order_ok);
        assert!(snapshot.problems.is_empty());
    }

    #[test]
    fn dirty_rects_for_presenter_returns_rects_for_partial_updates() {
        let layout = SharedFramebufferLayout::new_rgba8(64, 32, 32).unwrap();
        let word_len = layout.total_byte_len().div_ceil(4);
        let mut words = vec![0u32; word_len];

        let shared = unsafe {
            SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout).unwrap()
        };
        shared.header().init(layout);

        let mut source =
            unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();

        let writer = SharedFramebufferWriter::new(shared);
        writer.write_frame(|buf, dirty, layout| {
            buf.fill(0x22);
            if let Some(words) = dirty {
                // Mark only the first tile dirty (top-left).
                let tile_count = layout.tile_count();
                assert!(tile_count >= 1);
                words[0] = 1;
            }
        });

        let frame = source.poll_frame().expect("new frame");
        let rects = frame
            .dirty_rects_for_presenter()
            .expect("expected partial rects");
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].x, 0);
        assert_eq!(rects[0].y, 0);
        assert_eq!(rects[0].w, 32);
        assert_eq!(rects[0].h, 32);
    }

    #[test]
    fn inspector_detects_publish_order_mismatch() {
        let layout = SharedFramebufferLayout::new_rgba8(16, 16, 0).unwrap();
        let word_len = layout.total_byte_len().div_ceil(4);
        let mut words = vec![0u32; word_len];
        let shared = unsafe {
            SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout).unwrap()
        };
        shared.header().init(layout);

        // Simulate a broken publish: global seq says 1 but the active buffer seq does not.
        let header = shared.header();
        header.active_index.store(1, Ordering::SeqCst);
        header.frame_seq.store(1, Ordering::SeqCst);
        header.buf1_frame_seq.store(0, Ordering::SeqCst);

        let source =
            unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();
        let inspector = FrameInspector::new(&source);
        let snapshot = inspector.inspect(false);
        assert!(!snapshot.publish_order_ok);
        assert_eq!(snapshot.problems.len(), 1);
        assert!(matches!(
            snapshot.problems[0],
            FrameInspectorProblem::PublishOrderMismatch { .. }
        ));
    }
}
