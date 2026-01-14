use crate::frame_source::{FrameSource, FrameSourceError};
use aero_shared::shared_framebuffer::{
    FramebufferFormat, SharedFramebuffer, SharedFramebufferError, SharedFramebufferLayout,
    SharedFramebufferWriter, SHARED_FRAMEBUFFER_MAGIC, SHARED_FRAMEBUFFER_VERSION,
};
use std::sync::atomic::Ordering;

fn alloc_words(layout: SharedFramebufferLayout) -> Vec<u32> {
    // `SharedFramebufferLayout::total_byte_len()` is always aligned to 64 bytes, so the u32 count is
    // exact (no rounding required).
    let word_len = layout.total_byte_len().div_ceil(4);
    vec![0u32; word_len]
}

#[test]
fn frame_source_from_shared_memory_rejects_null_base_ptr() {
    let err = unsafe { FrameSource::from_shared_memory(core::ptr::null_mut(), 0) }
        .err()
        .expect("null base pointer must be rejected");
    assert!(matches!(
        err,
        FrameSourceError::SharedFramebuffer(SharedFramebufferError::NullBasePtr)
    ));
}

#[test]
fn frame_source_from_shared_memory_rejects_unaligned_base_ptr() {
    // Pass an intentionally unaligned base pointer (`ptr + 1`) while keeping the offset at 0.
    let mut bytes = vec![0u8; 128];
    let base = unsafe { bytes.as_mut_ptr().add(1) };
    let err = unsafe { FrameSource::from_shared_memory(base, 0) }
        .err()
        .expect("unaligned base pointer must be rejected");

    assert!(matches!(
        err,
        FrameSourceError::SharedFramebuffer(SharedFramebufferError::UnalignedBasePtr { .. })
    ));
}

#[test]
fn frame_source_from_shared_memory_rejects_bad_magic() {
    let layout = SharedFramebufferLayout::new_rgba8(16, 16, /*tile_size=*/ 0).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let bad_magic = SHARED_FRAMEBUFFER_MAGIC ^ 0xFFFF_FFFF;
    shared.header().magic.store(bad_magic, Ordering::SeqCst);

    let err = unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }
        .err()
        .expect("bad magic must be rejected");
    assert!(matches!(err, FrameSourceError::BadMagic { found } if found == bad_magic));
}

#[test]
fn frame_source_from_shared_memory_rejects_bad_version() {
    let layout = SharedFramebufferLayout::new_rgba8(16, 16, /*tile_size=*/ 0).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let bad_version = SHARED_FRAMEBUFFER_VERSION + 1;
    shared.header().version.store(bad_version, Ordering::SeqCst);

    let err = unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }
        .err()
        .expect("bad version must be rejected");
    assert!(matches!(err, FrameSourceError::BadVersion { found } if found == bad_version));
}

#[test]
fn frame_source_from_shared_memory_rejects_unsupported_format() {
    let layout = SharedFramebufferLayout::new_rgba8(16, 16, /*tile_size=*/ 0).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let bad_format = 999u32;
    shared.header().format.store(bad_format, Ordering::SeqCst);

    let err = unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }
        .err()
        .expect("unsupported format must be rejected");
    assert!(matches!(err, FrameSourceError::UnsupportedFormat { found } if found == bad_format));
}

#[test]
fn frame_source_from_shared_memory_rejects_dirty_words_per_buffer_mismatch() {
    let layout = SharedFramebufferLayout::new_rgba8(64, 32, /*tile_size=*/ 32).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let bad_dirty_words = layout.dirty_words_per_buffer + 1;
    shared
        .header()
        .dirty_words_per_buffer
        .store(bad_dirty_words, Ordering::SeqCst);

    let err = unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }
        .err()
        .expect("dirty_words_per_buffer mismatch must be rejected");
    assert!(matches!(
        err,
        FrameSourceError::DirtyWordsMismatch { expected, found }
            if expected == layout.dirty_words_per_buffer && found == bad_dirty_words
    ));
}

#[test]
fn frame_source_from_shared_memory_rejects_tile_grid_mismatch() {
    let layout = SharedFramebufferLayout::new_rgba8(64, 32, /*tile_size=*/ 32).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let bad_tiles_x = layout.tiles_x + 1;
    shared.header().tiles_x.store(bad_tiles_x, Ordering::SeqCst);

    let err = unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }
        .err()
        .expect("tile grid mismatch must be rejected");
    assert!(matches!(
        err,
        FrameSourceError::TilesMismatch {
            expected_x,
            expected_y,
            found_x,
            found_y,
        } if expected_x == layout.tiles_x
            && expected_y == layout.tiles_y
            && found_x == bad_tiles_x
            && found_y == layout.tiles_y
    ));
}

#[test]
fn frame_source_happy_path_poll_and_dirty_rects() {
    let layout = SharedFramebufferLayout::new_rgba8(64, 32, /*tile_size=*/ 32).unwrap();
    let mut words = alloc_words(layout);

    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let mut source =
        unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();

    // Nothing has been published yet.
    assert!(source.poll_frame().is_none());

    // Publish a full-frame update (all dirty bits set).
    let writer = SharedFramebufferWriter::new(shared);
    writer.write_frame(|buf, dirty, layout| {
        buf.fill(0x11);
        if let Some(words) = dirty {
            for idx in 0..layout.tile_count() {
                let word = idx / 32;
                let bit = idx % 32;
                words[word] |= 1u32 << bit;
            }
        }
    });

    let frame = source.poll_frame().expect("new frame must be visible");
    assert_eq!(frame.width, layout.width);
    assert_eq!(frame.height, layout.height);
    assert_eq!(frame.stride_bytes, layout.stride_bytes);
    assert_eq!(frame.format, FramebufferFormat::Rgba8);
    assert_eq!(frame.active_buf_seq, frame.seq);
    assert_eq!(frame.pixels[0], 0x11);
    // Full-frame update should take the efficient presenter path.
    assert_eq!(frame.dirty_rects_for_presenter(), None);

    // Publish a partial update: mark only the first tile dirty.
    writer.write_frame(|buf, dirty, _layout| {
        buf.fill(0x22);
        if let Some(words) = dirty {
            words[0] = 1;
        }
    });

    let frame = source.poll_frame().expect("new frame must be visible");
    assert_eq!(frame.active_buf_seq, frame.seq);
    assert_eq!(frame.pixels[0], 0x22);
    let rects = frame
        .dirty_rects_for_presenter()
        .expect("partial update should return rects");
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0].x, 0);
    assert_eq!(rects[0].y, 0);
    assert_eq!(rects[0].w, 32);
    assert_eq!(rects[0].h, 32);
}

#[test]
fn frame_source_poll_frame_clamps_out_of_range_active_index() {
    let layout = SharedFramebufferLayout::new_rgba8(32, 32, /*tile_size=*/ 32).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    // Force the first publish to target buffer 0, so clamping an invalid active_index to 0 still
    // reads the freshly written frame.
    shared.header().active_index.store(1, Ordering::SeqCst);

    let mut source =
        unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();
    assert!(source.poll_frame().is_none());

    let writer = SharedFramebufferWriter::new(shared);
    writer.write_frame(|buf, _dirty, _layout| {
        buf.fill(0x33);
    });

    // Corrupt the header: active_index must be 0 or 1, but the reader should tolerate other values
    // by clamping.
    shared.header().active_index.store(2, Ordering::SeqCst);

    let frame = source.poll_frame().expect("new frame");
    assert_eq!(frame.active_index, 0);
    assert_eq!(frame.active_buf_seq, frame.seq);
    assert_eq!(frame.pixels[0], 0x33);
}

#[test]
fn frame_source_does_not_clear_frame_dirty_until_acked() {
    let layout = SharedFramebufferLayout::new_rgba8(16, 16, /*tile_size=*/ 0).unwrap();
    let mut words = alloc_words(layout);
    let shared = unsafe {
        SharedFramebuffer::from_raw_parts(words.as_mut_ptr() as *mut u8, layout)
            .expect("backing store must be aligned")
    };
    shared.header().init(layout);

    let mut source =
        unsafe { FrameSource::from_shared_memory(words.as_mut_ptr() as *mut u8, 0) }.unwrap();

    let writer = SharedFramebufferWriter::new(shared);
    writer.write_frame(|buf, _dirty, _layout| buf.fill(0x11));
    assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 1);

    let seq1 = {
        let frame = source.poll_frame().expect("new frame must be visible");
        assert_eq!(frame.seq, 1);

        // Polling must not clear `frame_dirty` since the frame buffer is still exposed by
        // reference.
        assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 1);
        frame.seq
    };
    source.ack_frame(seq1);
    assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 0);

    // Publishing a new frame sets `frame_dirty` again.
    writer.write_frame(|buf, _dirty, _layout| buf.fill(0x22));
    assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 1);

    // ACKing an older frame sequence must not clear a newer frame.
    source.ack_frame(seq1);
    assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 1);

    let seq2 = {
        let frame2 = source.poll_frame().expect("new frame must be visible");
        frame2.seq
    };
    source.ack_frame(seq2);
    assert_eq!(shared.header().frame_dirty.load(Ordering::SeqCst), 0);
}
