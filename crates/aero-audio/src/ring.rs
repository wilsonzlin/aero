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
    channels: usize,
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
        assert!(capacity_frames > 0);
        let capacity_frames = capacity_frames.min(MAX_CAPACITY_FRAMES);
        let channels = 2;
        Self {
            channels,
            capacity_frames,
            data: vec![0.0; capacity_frames * channels],
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
    pub fn push_interleaved_stereo(&mut self, samples: &[f32]) {
        assert!(samples.len().is_multiple_of(self.channels));
        let frames = samples.len() / self.channels;
        if frames == 0 {
            return;
        }

        let free_frames = self.capacity_frames - self.len_frames;
        let frames_to_write = frames.min(free_frames);
        let dropped = frames - frames_to_write;
        if dropped > 0 {
            self.overrun_frames += dropped as u64;
        }

        for frame_idx in 0..frames_to_write {
            let src = frame_idx * self.channels;
            let dst_frame = self.write_frame;
            let dst = dst_frame * self.channels;
            self.data[dst..dst + self.channels].copy_from_slice(&samples[src..src + self.channels]);
            self.write_frame = (self.write_frame + 1) % self.capacity_frames;
        }
        self.len_frames += frames_to_write;
    }

    /// Pop `frames` frames as interleaved stereo.
    ///
    /// If the buffer does not contain enough audio, the remaining frames will be
    /// filled with silence.
    pub fn pop_interleaved_stereo(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * self.channels];

        let available = self.len_frames.min(frames);
        for frame_idx in 0..available {
            let src_frame = self.read_frame;
            let src = src_frame * self.channels;
            let dst = frame_idx * self.channels;
            out[dst..dst + self.channels].copy_from_slice(&self.data[src..src + self.channels]);
            self.read_frame = (self.read_frame + 1) % self.capacity_frames;
        }

        self.len_frames -= available;
        if available < frames {
            self.underrun_frames += (frames - available) as u64;
        }

        out
    }
}
