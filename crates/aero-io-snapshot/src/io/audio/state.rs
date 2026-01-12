use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaStreamState {
    pub ctl: u32,
    pub lpib: u32,
    pub cbl: u32,
    pub lvi: u16,
    pub fifow: u16,
    pub fifos: u16,
    pub fmt: u16,
    pub bdpl: u32,
    pub bdpu: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaStreamRuntimeState {
    pub bdl_index: u16,
    pub bdl_offset: u32,
    pub last_fmt_raw: u16,
    /// `f64` bits for the stream resampler source position.
    pub resampler_src_pos_bits: u64,
    /// Number of queued source frames in the stream resampler.
    pub resampler_queued_frames: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaCodecState {
    pub output_stream_id: u8,
    pub output_channel: u8,
    pub output_format: u16,
    pub amp_gain_left: u8,
    pub amp_gain_right: u8,
    pub amp_mute_left: bool,
    pub amp_mute_right: bool,
    pub pin_conn_select: u8,
    pub pin_ctl: u8,
    pub output_pin_power_state: u8,
    pub afg_power_state: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaCodecCaptureState {
    pub input_stream_id: u8,
    pub input_channel: u8,
    pub input_format: u16,
    pub mic_pin_conn_select: u8,
    pub mic_pin_ctl: u8,
    pub mic_pin_power_state: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioWorkletRingState {
    pub capacity_frames: u32,
    pub write_pos: u32,
    pub read_pos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaControllerState {
    pub gctl: u32,
    pub wakeen: u16,
    pub statests: u16,
    pub intctl: u32,
    pub intsts: u32,
    /// Host/output sample rate used by the HDA controller for time base + resampling.
    ///
    /// This is not guest-visible directly, but it affects guest-visible DMA/LPIB progression.
    /// Stored so restores can recreate deterministic stream resampler state without requiring the
    /// coordinator to set the rate before applying the snapshot.
    pub output_rate_hz: u32,
    /// Host/input sample rate used when pulling microphone samples for the capture stream.
    ///
    /// Like [`Self::output_rate_hz`], this is not guest-visible directly, but it affects how many
    /// host samples are consumed to synthesize guest capture frames (and therefore affects
    /// deterministic capture output when the capture source is deterministic).
    pub capture_sample_rate_hz: u32,

    pub dplbase: u32,
    pub dpubase: u32,

    pub corblbase: u32,
    pub corbubase: u32,
    pub corbwp: u16,
    pub corbrp: u16,
    pub corbctl: u8,
    pub corbsts: u8,
    pub corbsize: u8,

    pub rirblbase: u32,
    pub rirbubase: u32,
    pub rirbwp: u16,
    pub rirbctl: u8,
    pub rirbsts: u8,
    pub rirbsize: u8,
    pub rintcnt: u16,

    pub streams: Vec<HdaStreamState>,
    pub stream_runtime: Vec<HdaStreamRuntimeState>,
    pub stream_capture_frame_accum: Vec<u64>,
    pub codec: HdaCodecState,
    pub codec_capture: HdaCodecCaptureState,
    pub worklet_ring: AudioWorkletRingState,
}

impl Default for HdaControllerState {
    fn default() -> Self {
        Self {
            gctl: 0,
            wakeen: 0,
            statests: 0,
            intctl: 0,
            intsts: 0,
            output_rate_hz: 0,
            capture_sample_rate_hz: 0,
            dplbase: 0,
            dpubase: 0,
            corblbase: 0,
            corbubase: 0,
            corbwp: 0,
            corbrp: 0,
            corbctl: 0,
            corbsts: 0,
            corbsize: 0,
            rirblbase: 0,
            rirbubase: 0,
            rirbwp: 0,
            rirbctl: 0,
            rirbsts: 0,
            rirbsize: 0,
            rintcnt: 0,
            streams: Vec::new(),
            stream_runtime: Vec::new(),
            stream_capture_frame_accum: Vec::new(),
            codec: HdaCodecState {
                output_stream_id: 0,
                output_channel: 0,
                output_format: 0x0011,
                amp_gain_left: 0x7f,
                amp_gain_right: 0x7f,
                amp_mute_left: false,
                amp_mute_right: false,
                pin_conn_select: 0,
                pin_ctl: 0x40,
                output_pin_power_state: 0,
                afg_power_state: 0,
            },
            codec_capture: HdaCodecCaptureState {
                input_stream_id: 0,
                input_channel: 0,
                input_format: 0x0010,
                mic_pin_conn_select: 0,
                mic_pin_ctl: 0,
                mic_pin_power_state: 0,
            },
            worklet_ring: AudioWorkletRingState {
                capacity_frames: 0,
                write_pos: 0,
                read_pos: 0,
            },
        }
    }
}

impl IoSnapshot for HdaControllerState {
    const DEVICE_ID: [u8; 4] = *b"HDA0";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(2, 4);

    fn save_state(&self) -> Vec<u8> {
        const TAG_GCTL: u16 = 1;
        const TAG_STATESTS: u16 = 2;
        const TAG_INTCTL: u16 = 3;
        const TAG_INTSTS: u16 = 4;
        const TAG_DPLBASE: u16 = 5;
        const TAG_DPUBASE: u16 = 6;
        const TAG_WAKEEN: u16 = 7;
        const TAG_OUTPUT_RATE_HZ: u16 = 8;
        const TAG_CAPTURE_SAMPLE_RATE_HZ: u16 = 9;

        const TAG_CORBLBASE: u16 = 10;
        const TAG_CORBUBASE: u16 = 11;
        const TAG_CORBWP: u16 = 12;
        const TAG_CORBRP: u16 = 13;
        const TAG_CORBCTL: u16 = 14;
        const TAG_CORBSTS: u16 = 15;
        const TAG_CORBSIZE: u16 = 16;

        const TAG_RIRBLBASE: u16 = 20;
        const TAG_RIRBUBASE: u16 = 21;
        const TAG_RIRBWP: u16 = 22;
        const TAG_RINTCNT: u16 = 23;
        const TAG_RIRBCTL: u16 = 24;
        const TAG_RIRBSTS: u16 = 25;
        const TAG_RIRBSIZE: u16 = 26;

        const TAG_STREAMS: u16 = 30;
        const TAG_STREAM_RUNTIME: u16 = 31;
        const TAG_STREAM_CAPTURE_FRAME_ACCUM: u16 = 32;
        const TAG_STREAM_FIFOW: u16 = 33;

        const TAG_CODEC: u16 = 40;
        const TAG_CODEC_CAPTURE: u16 = 41;
        const TAG_CODEC_PIN_POWER: u16 = 42;
        const TAG_WORKLET_RING: u16 = 50;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_GCTL, self.gctl);
        w.field_u16(TAG_STATESTS, self.statests);
        w.field_u32(TAG_INTCTL, self.intctl);
        w.field_u32(TAG_INTSTS, self.intsts);
        w.field_u32(TAG_DPLBASE, self.dplbase);
        w.field_u32(TAG_DPUBASE, self.dpubase);
        w.field_u16(TAG_WAKEEN, self.wakeen);
        w.field_u32(TAG_OUTPUT_RATE_HZ, self.output_rate_hz);
        w.field_u32(TAG_CAPTURE_SAMPLE_RATE_HZ, self.capture_sample_rate_hz);

        w.field_u32(TAG_CORBLBASE, self.corblbase);
        w.field_u32(TAG_CORBUBASE, self.corbubase);
        w.field_u16(TAG_CORBWP, self.corbwp);
        w.field_u16(TAG_CORBRP, self.corbrp);
        w.field_u8(TAG_CORBCTL, self.corbctl);
        w.field_u8(TAG_CORBSTS, self.corbsts);
        w.field_u8(TAG_CORBSIZE, self.corbsize);

        w.field_u32(TAG_RIRBLBASE, self.rirblbase);
        w.field_u32(TAG_RIRBUBASE, self.rirbubase);
        w.field_u16(TAG_RIRBWP, self.rirbwp);
        w.field_u16(TAG_RINTCNT, self.rintcnt);
        w.field_u8(TAG_RIRBCTL, self.rirbctl);
        w.field_u8(TAG_RIRBSTS, self.rirbsts);
        w.field_u8(TAG_RIRBSIZE, self.rirbsize);

        let mut streams = Encoder::new().u32(self.streams.len() as u32);
        for s in &self.streams {
            streams = streams
                .u32(s.ctl)
                .u32(s.lpib)
                .u32(s.cbl)
                .u16(s.lvi)
                .u16(s.fifos)
                .u16(s.fmt)
                .u32(s.bdpl)
                .u32(s.bdpu);
        }
        w.field_bytes(TAG_STREAMS, streams.finish());

        let mut fifow = Encoder::new().u32(self.streams.len() as u32);
        for s in &self.streams {
            fifow = fifow.u16(s.fifow);
        }
        w.field_bytes(TAG_STREAM_FIFOW, fifow.finish());

        let mut rt = Encoder::new().u32(self.stream_runtime.len() as u32);
        for s in &self.stream_runtime {
            rt = rt
                .u16(s.bdl_index)
                .u32(s.bdl_offset)
                .u16(s.last_fmt_raw)
                .u64(s.resampler_src_pos_bits)
                .u32(s.resampler_queued_frames);
        }
        w.field_bytes(TAG_STREAM_RUNTIME, rt.finish());

        let mut capture_accum = Encoder::new().u32(self.stream_capture_frame_accum.len() as u32);
        for v in &self.stream_capture_frame_accum {
            capture_accum = capture_accum.u64(*v);
        }
        w.field_bytes(TAG_STREAM_CAPTURE_FRAME_ACCUM, capture_accum.finish());

        let codec = Encoder::new()
            .u8(self.codec.output_stream_id)
            .u8(self.codec.output_channel)
            .u16(self.codec.output_format)
            .u8(self.codec.amp_gain_left)
            .u8(self.codec.amp_gain_right)
            .bool(self.codec.amp_mute_left)
            .bool(self.codec.amp_mute_right)
            .u8(self.codec.pin_conn_select)
            .u8(self.codec.pin_ctl)
            .u8(self.codec.afg_power_state)
            .finish();
        w.field_bytes(TAG_CODEC, codec);

        let codec_capture = Encoder::new()
            .u8(self.codec_capture.input_stream_id)
            .u8(self.codec_capture.input_channel)
            .u16(self.codec_capture.input_format)
            .u8(self.codec_capture.mic_pin_conn_select)
            .u8(self.codec_capture.mic_pin_ctl)
            .finish();
        w.field_bytes(TAG_CODEC_CAPTURE, codec_capture);

        let codec_pins = Encoder::new()
            .u8(self.codec.output_pin_power_state)
            .u8(self.codec_capture.mic_pin_power_state)
            .finish();
        w.field_bytes(TAG_CODEC_PIN_POWER, codec_pins);

        let ring = Encoder::new()
            .u32(self.worklet_ring.capacity_frames)
            .u32(self.worklet_ring.write_pos)
            .u32(self.worklet_ring.read_pos)
            .finish();
        w.field_bytes(TAG_WORKLET_RING, ring);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        // Snapshot files may come from untrusted sources; keep bounded allocations when
        // restoring variable-length arrays.
        const MAX_STREAMS: usize = 64;
        const STREAM_STATE_BYTES: usize = 4 + 4 + 4 + 2 + 2 + 2 + 4 + 4;
        const STREAM_RUNTIME_BYTES: usize = 2 + 4 + 2 + 8 + 4;

        const TAG_GCTL: u16 = 1;
        const TAG_STATESTS: u16 = 2;
        const TAG_INTCTL: u16 = 3;
        const TAG_INTSTS: u16 = 4;
        const TAG_DPLBASE: u16 = 5;
        const TAG_DPUBASE: u16 = 6;
        const TAG_WAKEEN: u16 = 7;
        const TAG_OUTPUT_RATE_HZ: u16 = 8;
        const TAG_CAPTURE_SAMPLE_RATE_HZ: u16 = 9;

        const TAG_CORBLBASE: u16 = 10;
        const TAG_CORBUBASE: u16 = 11;
        const TAG_CORBWP: u16 = 12;
        const TAG_CORBRP: u16 = 13;
        const TAG_CORBCTL: u16 = 14;
        const TAG_CORBSTS: u16 = 15;
        const TAG_CORBSIZE: u16 = 16;

        const TAG_RIRBLBASE: u16 = 20;
        const TAG_RIRBUBASE: u16 = 21;
        const TAG_RIRBWP: u16 = 22;
        const TAG_RINTCNT: u16 = 23;
        const TAG_RIRBCTL: u16 = 24;
        const TAG_RIRBSTS: u16 = 25;
        const TAG_RIRBSIZE: u16 = 26;

        const TAG_STREAMS: u16 = 30;
        const TAG_STREAM_RUNTIME: u16 = 31;
        const TAG_STREAM_CAPTURE_FRAME_ACCUM: u16 = 32;
        const TAG_STREAM_FIFOW: u16 = 33;

        const TAG_CODEC: u16 = 40;
        const TAG_CODEC_CAPTURE: u16 = 41;
        const TAG_CODEC_PIN_POWER: u16 = 42;
        const TAG_WORKLET_RING: u16 = 50;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(v) = r.u32(TAG_GCTL)? {
            self.gctl = v;
        }
        if let Some(v) = r.u16(TAG_STATESTS)? {
            self.statests = v;
        }
        if let Some(v) = r.u32(TAG_INTCTL)? {
            self.intctl = v;
        }
        if let Some(v) = r.u32(TAG_INTSTS)? {
            self.intsts = v;
        }
        if let Some(v) = r.u32(TAG_DPLBASE)? {
            self.dplbase = v;
        }
        if let Some(v) = r.u32(TAG_DPUBASE)? {
            self.dpubase = v;
        }
        if let Some(v) = r.u16(TAG_WAKEEN)? {
            self.wakeen = v;
        }
        if let Some(v) = r.u32(TAG_OUTPUT_RATE_HZ)? {
            self.output_rate_hz = v;
        }
        if let Some(v) = r.u32(TAG_CAPTURE_SAMPLE_RATE_HZ)? {
            self.capture_sample_rate_hz = v;
        }

        if let Some(v) = r.u32(TAG_CORBLBASE)? {
            self.corblbase = v;
        }
        if let Some(v) = r.u32(TAG_CORBUBASE)? {
            self.corbubase = v;
        }
        if let Some(v) = r.u16(TAG_CORBWP)? {
            self.corbwp = v;
        }
        if let Some(v) = r.u16(TAG_CORBRP)? {
            self.corbrp = v;
        }
        if let Some(v) = r.u8(TAG_CORBCTL)? {
            self.corbctl = v;
        }
        if let Some(v) = r.u8(TAG_CORBSTS)? {
            self.corbsts = v;
        }
        if let Some(v) = r.u8(TAG_CORBSIZE)? {
            self.corbsize = v;
        }

        if let Some(v) = r.u32(TAG_RIRBLBASE)? {
            self.rirblbase = v;
        }
        if let Some(v) = r.u32(TAG_RIRBUBASE)? {
            self.rirbubase = v;
        }
        if let Some(v) = r.u16(TAG_RIRBWP)? {
            self.rirbwp = v;
        }
        if let Some(v) = r.u16(TAG_RINTCNT)? {
            self.rintcnt = v;
        }
        if let Some(v) = r.u8(TAG_RIRBCTL)? {
            self.rirbctl = v;
        }
        if let Some(v) = r.u8(TAG_RIRBSTS)? {
            self.rirbsts = v;
        }
        if let Some(v) = r.u8(TAG_RIRBSIZE)? {
            self.rirbsize = v;
        }

        self.streams.clear();
        if let Some(buf) = r.bytes(TAG_STREAMS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            let max_by_len = buf
                .len()
                .saturating_sub(4)
                .saturating_div(STREAM_STATE_BYTES);
            let count = count.min(max_by_len);
            let keep = count.min(MAX_STREAMS);
            self.streams.reserve(keep);
            for _ in 0..keep {
                self.streams.push(HdaStreamState {
                    ctl: d.u32()?,
                    lpib: d.u32()?,
                    cbl: d.u32()?,
                    lvi: d.u16()?,
                    fifow: 0,
                    fifos: d.u16()?,
                    fmt: d.u16()?,
                    bdpl: d.u32()?,
                    bdpu: d.u32()?,
                });
            }
            if count > keep {
                let skip = (count - keep).saturating_mul(STREAM_STATE_BYTES);
                d.bytes(skip)?;
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_STREAM_FIFOW) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            let max_by_len = buf.len().saturating_sub(4).saturating_div(2);
            let count = count.min(max_by_len);
            let apply = count.min(self.streams.len());
            for i in 0..apply {
                self.streams[i].fifow = d.u16()?;
            }
            if count > apply {
                d.bytes((count - apply).saturating_mul(2))?;
            }
            d.finish()?;
        }

        self.stream_runtime.clear();
        if let Some(buf) = r.bytes(TAG_STREAM_RUNTIME) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            let max_by_len = buf
                .len()
                .saturating_sub(4)
                .saturating_div(STREAM_RUNTIME_BYTES);
            let count = count.min(max_by_len);
            let keep = count.min(MAX_STREAMS);
            self.stream_runtime.reserve(keep);
            for _ in 0..keep {
                self.stream_runtime.push(HdaStreamRuntimeState {
                    bdl_index: d.u16()?,
                    bdl_offset: d.u32()?,
                    last_fmt_raw: d.u16()?,
                    resampler_src_pos_bits: d.u64()?,
                    resampler_queued_frames: d.u32()?,
                });
            }
            if count > keep {
                d.bytes((count - keep).saturating_mul(STREAM_RUNTIME_BYTES))?;
            }
            d.finish()?;
        }

        self.stream_capture_frame_accum.clear();
        if let Some(buf) = r.bytes(TAG_STREAM_CAPTURE_FRAME_ACCUM) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            let max_by_len = buf.len().saturating_sub(4).saturating_div(8);
            let count = count.min(max_by_len);
            let keep = count.min(MAX_STREAMS);
            self.stream_capture_frame_accum.reserve(keep);
            for _ in 0..keep {
                self.stream_capture_frame_accum.push(d.u64()?);
            }
            if count > keep {
                d.bytes((count - keep).saturating_mul(8))?;
            }
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_CODEC) {
            let mut d = Decoder::new(buf);
            self.codec.output_stream_id = d.u8()?;
            self.codec.output_channel = d.u8()?;
            self.codec.output_format = d.u16()?;
            self.codec.amp_gain_left = d.u8()?;
            self.codec.amp_gain_right = d.u8()?;
            self.codec.amp_mute_left = d.bool()?;
            self.codec.amp_mute_right = d.bool()?;
            self.codec.pin_conn_select = d.u8()?;
            self.codec.pin_ctl = d.u8()?;
            self.codec.afg_power_state = d.u8()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_CODEC_CAPTURE) {
            let mut d = Decoder::new(buf);
            self.codec_capture.input_stream_id = d.u8()?;
            self.codec_capture.input_channel = d.u8()?;
            self.codec_capture.input_format = d.u16()?;
            self.codec_capture.mic_pin_conn_select = d.u8()?;
            self.codec_capture.mic_pin_ctl = d.u8()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_CODEC_PIN_POWER) {
            let mut d = Decoder::new(buf);
            self.codec.output_pin_power_state = d.u8()?;
            self.codec_capture.mic_pin_power_state = d.u8()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_WORKLET_RING) {
            let mut d = Decoder::new(buf);
            self.worklet_ring.capacity_frames = d.u32()?;
            self.worklet_ring.write_pos = d.u32()?;
            self.worklet_ring.read_pos = d.u32()?;
            d.finish()?;
        }

        Ok(())
    }
}

// -------------------------------------------------------------------------------------------------
// virtio-snd (virtio-pci) snapshot state
// -------------------------------------------------------------------------------------------------

/// Serializable PCM parameters for a virtio-snd stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioSndPcmParamsState {
    pub buffer_bytes: u32,
    pub period_bytes: u32,
    pub channels: u8,
    pub format: u8,
    pub rate: u8,
}

/// Serializable per-stream state for virtio-snd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioSndStreamState {
    /// Stream state encoded as:
    /// - 0: Idle
    /// - 1: ParamsSet
    /// - 2: Prepared
    /// - 3: Running
    pub state: u8,
    pub params: Option<VirtioSndPcmParamsState>,
}

/// Host-side microphone capture telemetry counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VirtioSndCaptureTelemetryState {
    pub dropped_samples: u64,
    pub underrun_samples: u64,
    pub underrun_responses: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioSndState {
    pub playback: VirtioSndStreamState,
    pub capture: VirtioSndStreamState,
    pub capture_telemetry: VirtioSndCaptureTelemetryState,
    /// Host/output sample rate used by the virtio-snd playback path.
    pub host_sample_rate_hz: u32,
    /// Host/input sample rate used by the virtio-snd capture path.
    pub capture_sample_rate_hz: u32,
}

impl Default for VirtioSndState {
    fn default() -> Self {
        Self {
            playback: VirtioSndStreamState {
                state: 0,
                params: None,
            },
            capture: VirtioSndStreamState {
                state: 0,
                params: None,
            },
            capture_telemetry: VirtioSndCaptureTelemetryState::default(),
            host_sample_rate_hz: 0,
            capture_sample_rate_hz: 0,
        }
    }
}

/// Full virtio-snd PCI function state as stored in `aero-io-snapshot`.
///
/// This includes:
/// - virtio-pci transport state (`DEVICE_ID = VPCI`) as opaque bytes
/// - virtio-snd internal stream state
/// - AudioWorklet output ring indices (but not ring contents)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioSndPciState {
    pub virtio_pci: Vec<u8>,
    pub snd: VirtioSndState,
    pub worklet_ring: AudioWorkletRingState,
}

impl Default for VirtioSndPciState {
    fn default() -> Self {
        Self {
            virtio_pci: Vec::new(),
            snd: VirtioSndState::default(),
            worklet_ring: AudioWorkletRingState {
                capacity_frames: 0,
                write_pos: 0,
                read_pos: 0,
            },
        }
    }
}

impl IoSnapshot for VirtioSndPciState {
    const DEVICE_ID: [u8; 4] = *b"VSND";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_VIRTIO_PCI: u16 = 1;
        const TAG_HOST_SAMPLE_RATE_HZ: u16 = 2;
        const TAG_CAPTURE_SAMPLE_RATE_HZ: u16 = 3;

        const TAG_PLAYBACK_STREAM: u16 = 10;
        const TAG_CAPTURE_STREAM: u16 = 11;
        const TAG_CAPTURE_TELEMETRY: u16 = 12;

        const TAG_WORKLET_RING: u16 = 20;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_bytes(TAG_VIRTIO_PCI, self.virtio_pci.clone());
        w.field_u32(TAG_HOST_SAMPLE_RATE_HZ, self.snd.host_sample_rate_hz);
        w.field_u32(TAG_CAPTURE_SAMPLE_RATE_HZ, self.snd.capture_sample_rate_hz);

        let encode_stream = |s: &VirtioSndStreamState| -> Vec<u8> {
            let mut enc = Encoder::new().u8(s.state).bool(s.params.is_some());
            if let Some(p) = s.params.as_ref() {
                enc = enc
                    .u32(p.buffer_bytes)
                    .u32(p.period_bytes)
                    .u8(p.channels)
                    .u8(p.format)
                    .u8(p.rate);
            }
            enc.finish()
        };
        w.field_bytes(TAG_PLAYBACK_STREAM, encode_stream(&self.snd.playback));
        w.field_bytes(TAG_CAPTURE_STREAM, encode_stream(&self.snd.capture));

        let telemetry = Encoder::new()
            .u64(self.snd.capture_telemetry.dropped_samples)
            .u64(self.snd.capture_telemetry.underrun_samples)
            .u64(self.snd.capture_telemetry.underrun_responses)
            .finish();
        w.field_bytes(TAG_CAPTURE_TELEMETRY, telemetry);

        let ring = Encoder::new()
            .u32(self.worklet_ring.capacity_frames)
            .u32(self.worklet_ring.write_pos)
            .u32(self.worklet_ring.read_pos)
            .finish();
        w.field_bytes(TAG_WORKLET_RING, ring);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        // Defensive bounds: snapshot files may be untrusted.
        const MAX_VIRTIO_PCI_BYTES: usize = 64 * 1024;

        const TAG_VIRTIO_PCI: u16 = 1;
        const TAG_HOST_SAMPLE_RATE_HZ: u16 = 2;
        const TAG_CAPTURE_SAMPLE_RATE_HZ: u16 = 3;

        const TAG_PLAYBACK_STREAM: u16 = 10;
        const TAG_CAPTURE_STREAM: u16 = 11;
        const TAG_CAPTURE_TELEMETRY: u16 = 12;

        const TAG_WORKLET_RING: u16 = 20;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let Some(virtio_pci) = r.bytes(TAG_VIRTIO_PCI) else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing virtio-pci state",
            ));
        };
        if virtio_pci.len() > MAX_VIRTIO_PCI_BYTES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "virtio-pci state too large",
            ));
        }
        self.virtio_pci.clear();
        self.virtio_pci.extend_from_slice(virtio_pci);

        if let Some(v) = r.u32(TAG_HOST_SAMPLE_RATE_HZ)? {
            self.snd.host_sample_rate_hz = v;
        }
        if let Some(v) = r.u32(TAG_CAPTURE_SAMPLE_RATE_HZ)? {
            self.snd.capture_sample_rate_hz = v;
        }

        let decode_stream = |buf: &[u8]| -> SnapshotResult<VirtioSndStreamState> {
            let mut d = Decoder::new(buf);
            let state = d.u8()?;
            let has_params = d.bool()?;
            let params = if has_params {
                Some(VirtioSndPcmParamsState {
                    buffer_bytes: d.u32()?,
                    period_bytes: d.u32()?,
                    channels: d.u8()?,
                    format: d.u8()?,
                    rate: d.u8()?,
                })
            } else {
                None
            };
            d.finish()?;
            Ok(VirtioSndStreamState { state, params })
        };

        if let Some(buf) = r.bytes(TAG_PLAYBACK_STREAM) {
            self.snd.playback = decode_stream(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_CAPTURE_STREAM) {
            self.snd.capture = decode_stream(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_CAPTURE_TELEMETRY) {
            let mut d = Decoder::new(buf);
            self.snd.capture_telemetry.dropped_samples = d.u64()?;
            self.snd.capture_telemetry.underrun_samples = d.u64()?;
            self.snd.capture_telemetry.underrun_responses = d.u64()?;
            d.finish()?;
        }

        if let Some(buf) = r.bytes(TAG_WORKLET_RING) {
            let mut d = Decoder::new(buf);
            self.worklet_ring.capacity_frames = d.u32()?;
            self.worklet_ring.write_pos = d.u32()?;
            self.worklet_ring.read_pos = d.u32()?;
            d.finish()?;
        }

        Ok(())
    }
}
