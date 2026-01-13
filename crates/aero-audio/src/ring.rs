/// Stereo audio ring buffer used to bridge the emulator output to a Web Audio
/// `AudioWorkletProcessor`.
///
/// In the browser build this will be backed by a `SharedArrayBuffer`; for unit
/// tests we keep the implementation pure Rust while matching the same semantics:
///
/// - Producer writes interleaved stereo `f32` frames.
/// - Consumer reads a fixed number of frames per render quantum.
/// - On underrun, the consumer receives silence and `underrun_frames` increments
///   by the number of missing frames.
/// - On overrun (buffer full), the producer drops *new* frames (writes are
///   truncated) and an overrun counter increments.
///
/// This "drop-new" policy matches the `SharedArrayBuffer` output ring buffer
/// used by the AudioWorklet bridge: the consumer owns the read index, so the
/// producer must not advance it to make room.
#[derive(Debug, Clone)]
pub struct AudioRingBuffer {
    capacity_frames: usize,
    data: Vec<f32>,
    read_frame: usize,
    write_frame: usize,
    len_frames: usize,
    underrun_frames: u64,
    overrun_frames: u64,
}

/// Maximum ring buffer capacity supported by [`AudioRingBuffer`].
///
/// This crate is used in both native and wasm environments. A hostile/misconfigured caller could
/// otherwise request an arbitrarily large capacity and trigger a multi-gigabyte allocation.
///
/// `2^20` frames is ~21s at 48kHz; at stereo f32 this is ~8MiB of sample storage.
const MAX_CAPACITY_FRAMES: usize = 1_048_576;
const CHANNELS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingBufferTelemetry {
    pub capacity_frames: usize,
    pub available_frames: usize,
    pub underrun_frames: u64,
    /// Frames dropped because the producer attempted to write into a full
    /// buffer.
    pub overrun_frames: u64,
}

impl AudioRingBuffer {
    pub fn new_stereo(capacity_frames: usize) -> Self {
        // Treat capacity as untrusted (e.g. from host config); clamp to avoid panics and
        // multi-gigabyte allocations.
        let capacity_frames = capacity_frames.clamp(1, MAX_CAPACITY_FRAMES);
        Self {
            capacity_frames,
            data: vec![0.0; capacity_frames * CHANNELS],
            read_frame: 0,
            write_frame: 0,
            len_frames: 0,
            underrun_frames: 0,
            overrun_frames: 0,
        }
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    #[inline]
    pub fn available_frames(&self) -> usize {
        self.len_frames
    }

    pub fn telemetry(&self) -> RingBufferTelemetry {
        RingBufferTelemetry {
            capacity_frames: self.capacity_frames,
            available_frames: self.len_frames,
            underrun_frames: self.underrun_frames,
            overrun_frames: self.overrun_frames,
        }
    }

    pub fn clear(&mut self) {
        self.read_frame = 0;
        self.write_frame = 0;
        self.len_frames = 0;
        self.underrun_frames = 0;
        self.overrun_frames = 0;
        self.data.fill(0.0);
    }

    /// Push interleaved stereo samples.
    #[inline]
    pub fn push_interleaved_stereo(&mut self, samples: &[f32]) {
        // Treat inputs as untrusted; ignore any trailing partial frame.
        let frames = samples.len() / CHANNELS;
        if frames == 0 {
            return;
        }

        let free_frames = self.capacity_frames.saturating_sub(self.len_frames);
        let frames_to_write = frames.min(free_frames);
        let dropped = frames - frames_to_write;
        if dropped > 0 {
            self.overrun_frames = self.overrun_frames.saturating_add(dropped as u64);
        }
        if frames_to_write == 0 {
            return;
        }

        // Copy in at most two segments: one contiguous chunk to the end of the
        // backing storage, then (if we wrapped) a second chunk at the start.
        //
        // This avoids per-frame copying which shows up in profiles when the ring is
        // used as an `AudioSink` in unit tests / native builds.
        let write_pos = self.write_frame;
        let first_frames = frames_to_write.min(self.capacity_frames - write_pos);
        let second_frames = frames_to_write - first_frames;

        let first_samples = first_frames * CHANNELS;
        let write_sample_pos = write_pos * CHANNELS;
        self.data[write_sample_pos..write_sample_pos + first_samples]
            .copy_from_slice(&samples[..first_samples]);

        if second_frames > 0 {
            let second_samples = second_frames * CHANNELS;
            self.data[..second_samples]
                .copy_from_slice(&samples[first_samples..first_samples + second_samples]);
        }

        // Avoid `% capacity_frames` here: `frames_to_write <= capacity_frames`, so
        // the sum is always < 2*capacity_frames and can be wrapped with a single
        // conditional subtract (cheaper than a division/modulo).
        let mut write_frame = self.write_frame + frames_to_write;
        if write_frame >= self.capacity_frames {
            write_frame -= self.capacity_frames;
        }
        self.write_frame = write_frame;
        self.len_frames = self
            .len_frames
            .saturating_add(frames_to_write)
            .min(self.capacity_frames);
    }

    /// Pop `frames` frames as interleaved stereo.
    ///
    /// If the buffer does not contain enough audio, the remaining frames will be
    /// filled with silence.
    #[inline]
    pub fn pop_interleaved_stereo(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::new();
        self.pop_interleaved_stereo_into(frames, &mut out);
        out
    }

    /// Pop `frames` frames as interleaved stereo into a caller-provided buffer.
    ///
    /// This is a convenience for native/unit-test code that wants to avoid
    /// per-call allocations by reusing an output `Vec`.
    ///
    /// On underrun, the remaining frames are filled with silence and
    /// `underrun_frames` is incremented by the number of missing frames.
    ///
    /// If `out` cannot be resized (allocation failure), this leaves the ring
    /// buffer state unchanged and clears `out`.
    #[inline]
    pub fn pop_interleaved_stereo_into(&mut self, frames: usize, out: &mut Vec<f32>) {
        // Bound output allocation/work: callers may treat `frames` as untrusted.
        let frames = frames.min(MAX_CAPACITY_FRAMES);
        if frames == 0 {
            out.clear();
            return;
        }

        let out_len = frames.saturating_mul(CHANNELS);
        // `try_reserve_exact` reserves *additional* elements beyond the current
        // length. Avoid reserving `out_len` extra when the caller is reusing a
        // buffer that already has the right size.
        let additional = out_len.saturating_sub(out.len());
        if out.try_reserve_exact(additional).is_err() {
            out.clear();
            return;
        }
        out.resize(out_len, 0.0f32);

        let available = self.len_frames.min(frames);
        if available > 0 {
            // Copy in at most two segments: one contiguous chunk from the current
            // read cursor to the end of the storage, and (if we wrapped) a second
            // chunk from the start.
            let read_pos = self.read_frame;
            let first_frames = available.min(self.capacity_frames - read_pos);
            let second_frames = available - first_frames;

            let first_samples = first_frames * CHANNELS;
            let read_sample_pos = read_pos * CHANNELS;
            out[..first_samples]
                .copy_from_slice(&self.data[read_sample_pos..read_sample_pos + first_samples]);

            if second_frames > 0 {
                let second_samples = second_frames * CHANNELS;
                out[first_samples..first_samples + second_samples]
                    .copy_from_slice(&self.data[..second_samples]);
            }

            // Avoid `% capacity_frames` here: `available <= capacity_frames`, so
            // the sum is always < 2*capacity_frames and can be wrapped with a
            // single conditional subtract.
            let mut read_frame = self.read_frame + available;
            if read_frame >= self.capacity_frames {
                read_frame -= self.capacity_frames;
            }
            self.read_frame = read_frame;
        }

        let available_samples = available * CHANNELS;
        if available_samples < out_len {
            // Underrun: ensure the remainder is zeroed. (The caller may be
            // reusing an output buffer that contains old samples.)
            out[available_samples..].fill(0.0);
        }

        self.len_frames = self.len_frames.saturating_sub(available);
        if available < frames {
            self.underrun_frames = self
                .underrun_frames
                .saturating_add((frames - available) as u64);
        }
    }
}
