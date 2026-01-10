use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaStreamState {
    pub ctl: u32,
    pub lpib: u32,
    pub cbl: u32,
    pub lvi: u16,
    pub fmt: u16,
    pub bdpl: u32,
    pub bdpu: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioWorkletRingState {
    pub capacity_frames: u32,
    pub write_pos: u32,
    pub read_pos: u32,
}

/// Placeholder snapshot-able HDA device state.
///
/// The actual HDA device model and the WebAudio pipeline will live in separate
/// components; this struct defines the snapshot contract between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdaControllerState {
    pub gctl: u32,
    pub intctl: u32,
    pub intsts: u32,

    pub corbwp: u16,
    pub corbrp: u16,
    pub corbctl: u8,
    pub rirbwp: u16,
    pub rirbctl: u8,
    pub rintcnt: u16,

    pub streams: Vec<HdaStreamState>,
    pub worklet_ring: AudioWorkletRingState,
}

impl Default for HdaControllerState {
    fn default() -> Self {
        Self {
            gctl: 0,
            intctl: 0,
            intsts: 0,
            corbwp: 0,
            corbrp: 0,
            corbctl: 0,
            rirbwp: 0,
            rirbctl: 0,
            rintcnt: 0,
            streams: Vec::new(),
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
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_REGS: u16 = 1;
        const TAG_STREAMS: u16 = 2;
        const TAG_WORKLET_RING: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        let regs = Encoder::new()
            .u32(self.gctl)
            .u32(self.intctl)
            .u32(self.intsts)
            .u16(self.corbwp)
            .u16(self.corbrp)
            .u8(self.corbctl)
            .u16(self.rirbwp)
            .u8(self.rirbctl)
            .u16(self.rintcnt)
            .finish();
        w.field_bytes(TAG_REGS, regs);

        let mut streams = Encoder::new().u32(self.streams.len() as u32);
        for s in &self.streams {
            streams = streams
                .u32(s.ctl)
                .u32(s.lpib)
                .u32(s.cbl)
                .u16(s.lvi)
                .u16(s.fmt)
                .u32(s.bdpl)
                .u32(s.bdpu);
        }
        w.field_bytes(TAG_STREAMS, streams.finish());

        let ring = Encoder::new()
            .u32(self.worklet_ring.capacity_frames)
            .u32(self.worklet_ring.write_pos)
            .u32(self.worklet_ring.read_pos)
            .finish();
        w.field_bytes(TAG_WORKLET_RING, ring);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_REGS: u16 = 1;
        const TAG_STREAMS: u16 = 2;
        const TAG_WORKLET_RING: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_REGS) {
            let mut d = Decoder::new(buf);
            self.gctl = d.u32()?;
            self.intctl = d.u32()?;
            self.intsts = d.u32()?;
            self.corbwp = d.u16()?;
            self.corbrp = d.u16()?;
            self.corbctl = d.u8()?;
            self.rirbwp = d.u16()?;
            self.rirbctl = d.u8()?;
            self.rintcnt = d.u16()?;
            d.finish()?;
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
                    fmt: d.u16()?,
                    bdpl: d.u32()?,
                    bdpu: d.u32()?,
                });
            }
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

