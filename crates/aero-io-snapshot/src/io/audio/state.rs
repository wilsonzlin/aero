use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaStreamState {
    pub ctl: u32,
    pub lpib: u32,
    pub cbl: u32,
    pub lvi: u16,
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
    pub afg_power_state: u8,
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
    pub statests: u16,
    pub intctl: u32,
    pub intsts: u32,

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
    pub codec: HdaCodecState,
    pub worklet_ring: AudioWorkletRingState,
}

impl Default for HdaControllerState {
    fn default() -> Self {
        Self {
            gctl: 0,
            statests: 0,
            intctl: 0,
            intsts: 0,
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
            codec: HdaCodecState {
                output_stream_id: 0,
                output_channel: 0,
                output_format: 0,
                amp_gain_left: 0,
                amp_gain_right: 0,
                amp_mute_left: false,
                amp_mute_right: false,
                pin_conn_select: 0,
                pin_ctl: 0,
                afg_power_state: 0,
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
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(2, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_GCTL: u16 = 1;
        const TAG_STATESTS: u16 = 2;
        const TAG_INTCTL: u16 = 3;
        const TAG_INTSTS: u16 = 4;
        const TAG_DPLBASE: u16 = 5;
        const TAG_DPUBASE: u16 = 6;

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

        const TAG_CODEC: u16 = 40;
        const TAG_WORKLET_RING: u16 = 50;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_GCTL, self.gctl);
        w.field_u16(TAG_STATESTS, self.statests);
        w.field_u32(TAG_INTCTL, self.intctl);
        w.field_u32(TAG_INTSTS, self.intsts);
        w.field_u32(TAG_DPLBASE, self.dplbase);
        w.field_u32(TAG_DPUBASE, self.dpubase);

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

        let ring = Encoder::new()
            .u32(self.worklet_ring.capacity_frames)
            .u32(self.worklet_ring.write_pos)
            .u32(self.worklet_ring.read_pos)
            .finish();
        w.field_bytes(TAG_WORKLET_RING, ring);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_GCTL: u16 = 1;
        const TAG_STATESTS: u16 = 2;
        const TAG_INTCTL: u16 = 3;
        const TAG_INTSTS: u16 = 4;
        const TAG_DPLBASE: u16 = 5;
        const TAG_DPUBASE: u16 = 6;

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

        const TAG_CODEC: u16 = 40;
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
            self.streams.reserve(count);
            for _ in 0..count {
                self.streams.push(HdaStreamState {
                    ctl: d.u32()?,
                    lpib: d.u32()?,
                    cbl: d.u32()?,
                    lvi: d.u16()?,
                    fifos: d.u16()?,
                    fmt: d.u16()?,
                    bdpl: d.u32()?,
                    bdpu: d.u32()?,
                });
            }
            d.finish()?;
        }

        self.stream_runtime.clear();
        if let Some(buf) = r.bytes(TAG_STREAM_RUNTIME) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            self.stream_runtime.reserve(count);
            for _ in 0..count {
                self.stream_runtime.push(HdaStreamRuntimeState {
                    bdl_index: d.u16()?,
                    bdl_offset: d.u32()?,
                    last_fmt_raw: d.u16()?,
                    resampler_src_pos_bits: d.u64()?,
                    resampler_queued_frames: d.u32()?,
                });
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
