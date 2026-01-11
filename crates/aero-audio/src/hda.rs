use crate::capture::{AudioCaptureSource, SilenceCaptureSource};
use crate::clock::AudioFrameClock;
use crate::mem::MemoryAccess;
use crate::pcm::{decode_pcm_to_stereo_f32, encode_mono_f32_to_pcm, LinearResampler, StreamFormat};
use crate::ring::AudioRingBuffer;
use crate::sink::AudioSink;

/// Size of the HDA MMIO region.
pub const HDA_MMIO_SIZE: usize = 0x4000;

// Global register offsets (subset).
const REG_GCAP: u64 = 0x00;
const REG_VMIN: u64 = 0x02;
const REG_VMAJ: u64 = 0x03;
const REG_GCTL: u64 = 0x08;
const REG_STATESTS: u64 = 0x0e;
const REG_INTCTL: u64 = 0x20;
const REG_INTSTS: u64 = 0x24;

// CORB register offsets.
const REG_CORBLBASE: u64 = 0x40;
const REG_CORBUBASE: u64 = 0x44;
const REG_CORBWP: u64 = 0x48;
const REG_CORBRP: u64 = 0x4a;
const REG_CORBCTL: u64 = 0x4c;
const REG_CORBSTS: u64 = 0x4d;
const REG_CORBSIZE: u64 = 0x4e;

// RIRB register offsets.
const REG_RIRBLBASE: u64 = 0x50;
const REG_RIRBUBASE: u64 = 0x54;
const REG_RIRBWP: u64 = 0x58;
const REG_RINTCNT: u64 = 0x5a;
const REG_RIRBCTL: u64 = 0x5c;
const REG_RIRBSTS: u64 = 0x5d;
const REG_RIRBSIZE: u64 = 0x5e;

// DMA position buffer registers.
const REG_DPLBASE: u64 = 0x70;
const REG_DPUBASE: u64 = 0x74;

const REG_SD_BASE: u64 = 0x80;
const SD_STRIDE: u64 = 0x20;

// Stream descriptor register offsets within the SD block.
const SD_REG_CTL: u64 = 0x00;
const SD_REG_LPIB: u64 = 0x04;
const SD_REG_CBL: u64 = 0x08;
const SD_REG_LVI: u64 = 0x0c;
const SD_REG_FIFOS: u64 = 0x10;
const SD_REG_FMT: u64 = 0x12;
const SD_REG_BDPL: u64 = 0x18;
const SD_REG_BDPU: u64 = 0x1c;

// Bit definitions (subset).
const GCTL_CRST: u32 = 1 << 0;

const INTCTL_GIE: u32 = 1 << 31;
const INTSTS_CIS: u32 = 1 << 30;
const INTSTS_GIS: u32 = 1 << 31;

// CORBCTL/RIRBCTL bit positions follow the Intel HDA spec:
// - bit 1: DMA engine run
// - bit 0 (RIRBCTL only): response interrupt enable
const CORBCTL_RUN: u8 = 1 << 1;
const RIRBCTL_RUN: u8 = 1 << 1;
const RIRBCTL_RINTCTL: u8 = 1 << 0;

// CORBSIZE/RIRBSIZE capability bits (RO) as defined by the Intel HDA spec.
const RING_SIZE_CAP_2: u8 = 1 << 4;
const RING_SIZE_CAP_16: u8 = 1 << 5;
const RING_SIZE_CAP_256: u8 = 1 << 6;

const DPLBASE_ENABLE: u32 = 1 << 0;
const DPLBASE_ADDR_MASK: u32 = !0x7f;

/// Stream reset (SRST) bit in SDnCTL.
///
/// The Intel HDA spec defines this bit as active-high: when 0, the stream is held in reset.
const SD_CTL_SRST: u32 = 1 << 0;
const SD_CTL_RUN: u32 = 1 << 1;
const SD_CTL_IOCE: u32 = 1 << 2;
const SD_CTL_STRM_SHIFT: u32 = 20;
const SD_CTL_STRM_MASK: u32 = 0xF << SD_CTL_STRM_SHIFT;

const SD_STS_BCIS: u32 = 1 << 2; // Buffer Completion Interrupt Status (in SDSTS byte)

/// One HDA stream descriptor worth of registers.
#[derive(Debug, Clone)]
pub struct StreamDescriptor {
    pub ctl: u32,
    pub lpib: u32,
    pub cbl: u32,
    pub lvi: u16,
    pub fifos: u16,
    pub fmt: u16,
    pub bdpl: u32,
    pub bdpu: u32,
}

impl Default for StreamDescriptor {
    fn default() -> Self {
        Self {
            ctl: 0,
            lpib: 0,
            cbl: 0,
            lvi: 0,
            fifos: 0,
            fmt: 0,
            bdpl: 0,
            bdpu: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct StreamRuntime {
    bdl_index: u16,
    bdl_offset: u32,
    resampler: LinearResampler,
    last_fmt_raw: u16,
    capture_frame_accum: u64,
}

impl StreamRuntime {
    fn new(output_rate_hz: u32) -> Self {
        Self {
            bdl_index: 0,
            bdl_offset: 0,
            resampler: LinearResampler::new(output_rate_hz, output_rate_hz),
            last_fmt_raw: 0,
            capture_frame_accum: 0,
        }
    }

    fn reset(&mut self, output_rate_hz: u32) {
        self.bdl_index = 0;
        self.bdl_offset = 0;
        self.resampler.reset_rates(output_rate_hz, output_rate_hz);
        self.last_fmt_raw = 0;
        self.capture_frame_accum = 0;
    }
}

#[derive(Debug, Clone, Copy)]
struct BdlEntry {
    addr: u64,
    len: u32,
    ioc: bool,
}

fn read_bdl_entry(mem: &dyn MemoryAccess, base: u64, index: usize) -> BdlEntry {
    let addr = base + index as u64 * 16;
    let buf_addr = mem.read_u64(addr);
    let len = mem.read_u32(addr + 8);
    let flags = mem.read_u32(addr + 12);
    BdlEntry {
        addr: buf_addr,
        len,
        ioc: (flags & 1) != 0,
    }
}

fn supported_pcm_caps() -> u32 {
    // PCM Size, Rate capabilities (HDA spec). Advertise a minimal, common subset:
    // - 16-bit samples
    // - 44.1kHz and 48kHz
    (1 << 1) | (1 << 13) | (1 << 14)
}

/// Minimal Intel HDA codec model.
///
/// This is not intended to be feature complete; it only targets the verbs and
/// widget topology needed for Windows' inbox HDAudio stack to configure a basic
/// output path.
#[derive(Debug, Clone)]
pub struct HdaCodec {
    vendor_id: u32,
    subsystem_id: u32,
    revision_id: u32,

    output: CodecOutputWidget,
    output_pin: CodecPinWidget,
    input: CodecInputWidget,
    mic_pin: CodecPinWidget,
    afg_power_state: u8,
}

#[derive(Debug, Clone)]
struct CodecOutputWidget {
    stream_id: u8,
    channel: u8,
    format: u16,
    amp_gain_left: u8,
    amp_gain_right: u8,
    amp_mute_left: bool,
    amp_mute_right: bool,
}

#[derive(Debug, Clone)]
struct CodecInputWidget {
    stream_id: u8,
    channel: u8,
    format: u16,
}

#[derive(Debug, Clone)]
struct CodecPinWidget {
    conn_select: u8,
    pin_ctl: u8,
    config_default: u32,
}

impl CodecOutputWidget {
    /// Compute output amplifier gains as per-channel scalars in `[0.0, 1.0]`.
    ///
    /// Pragmatic mapping:
    /// - `0x00` => silence
    /// - `0x7f` => unity gain
    fn gain_scalars(&self) -> [f32; 2] {
        fn scalar(gain: u8, mute: bool) -> f32 {
            if mute {
                0.0
            } else {
                gain as f32 / 0x7f as f32
            }
        }

        [
            scalar(self.amp_gain_left, self.amp_mute_left),
            scalar(self.amp_gain_right, self.amp_mute_right),
        ]
    }
}

impl HdaCodec {
    pub fn new() -> Self {
        // Pick a plausible, but not particularly meaningful, codec identity.
        // Windows matches function group type, not vendor ID, for its generic
        // driver path.
        Self {
            vendor_id: 0x1af4_1620,
            subsystem_id: 0x1af4_0001,
            revision_id: 0x0001_0000,
            output: CodecOutputWidget {
                stream_id: 0,
                channel: 0,
                // Default to the common initial format: 48kHz, 16-bit, stereo.
                format: 0x0011,
                amp_gain_left: 0x7f,
                amp_gain_right: 0x7f,
                amp_mute_left: false,
                amp_mute_right: false,
            },
            output_pin: CodecPinWidget {
                conn_select: 0,
                // Pin Widget Control (PWCTL). Real codecs typically default to having the
                // line-out pin enabled; keep the model usable without requiring the guest
                // to explicitly unmute the pin.
                pin_ctl: 0x40,
                // Default config: line out, rear, green, 1/8" jack, association 1, sequence 0.
                config_default: 0x0101_0000,
            },
            input: CodecInputWidget {
                stream_id: 0,
                channel: 0,
                // Default to 48kHz, 16-bit, mono.
                format: 0x0010,
            },
            mic_pin: CodecPinWidget {
                conn_select: 0,
                pin_ctl: 0x00,
                // Default config: microphone input, rear, 1/8" jack.
                config_default: 0x01A1_0000,
            },
            afg_power_state: 0, // D0
        }
    }

    pub fn output_stream_id(&self) -> u8 {
        self.output.stream_id
    }

    fn output_gain_scalars(&self) -> [f32; 2] {
        // Treat anything other than D0 as powered down.
        if self.afg_power_state != 0 {
            return [0.0, 0.0];
        }
        // For the minimal model, treat pin_ctl==0 as disabled and non-zero as enabled.
        if self.output_pin.pin_ctl == 0 {
            return [0.0, 0.0];
        }
        self.output.gain_scalars()
    }

    pub fn input_stream_id(&self) -> u8 {
        self.input.stream_id
    }

    pub fn execute_verb(&mut self, nid: u8, verb_20: u32) -> u32 {
        let verb_id = ((verb_20 >> 8) & 0x0fff) as u16;
        let payload8 = (verb_20 & 0xff) as u8;
        let payload16 = (verb_20 & 0xffff) as u16;

        match nid {
            0 => self.handle_root_verb(verb_id, payload8),
            1 => self.handle_afg_verb(verb_id, payload8),
            2 => self.handle_output_verb(verb_id, payload8, payload16),
            3 => self.handle_output_pin_verb(verb_id, payload8),
            4 => self.handle_input_verb(verb_id, payload8, payload16),
            5 => self.handle_mic_pin_verb(verb_id, payload8),
            _ => 0,
        }
    }

    fn handle_root_verb(&mut self, verb_id: u16, payload8: u8) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_root(payload8),
            _ => 0,
        }
    }

    fn handle_afg_verb(&mut self, verb_id: u16, payload8: u8) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_afg(payload8),
            0x705 => {
                // SET_POWER_STATE
                self.afg_power_state = payload8 & 0x3;
                0
            }
            0xF05 => {
                // GET_POWER_STATE
                self.afg_power_state as u32
            }
            _ => 0,
        }
    }

    fn handle_output_verb(&mut self, verb_id: u16, payload8: u8, payload16: u16) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_output(payload8),
            0xF06 => ((self.output.stream_id as u32) << 4) | (self.output.channel as u32),
            0x706 => {
                self.output.stream_id = payload8 >> 4;
                self.output.channel = payload8 & 0x0f;
                0
            }
            0xA00 => self.output.format as u32,
            0x200..=0x2ff => {
                // SET_CONVERTER_FORMAT (4-bit verb encoded in low 16 bits)
                self.output.format = payload16;
                0
            }
            0xB00..=0xBff => self.get_amp_gain_mute(payload16),
            0x300..=0x3ff => {
                self.set_amp_gain_mute(payload16);
                0
            }
            _ => 0,
        }
    }

    fn handle_input_verb(&mut self, verb_id: u16, payload8: u8, payload16: u16) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_input(payload8),
            0xF06 => ((self.input.stream_id as u32) << 4) | (self.input.channel as u32),
            0x706 => {
                self.input.stream_id = payload8 >> 4;
                self.input.channel = payload8 & 0x0f;
                0
            }
            0xA00 => self.input.format as u32,
            0x200..=0x2ff => {
                // SET_CONVERTER_FORMAT (4-bit verb encoded in low 16 bits)
                self.input.format = payload16;
                0
            }
            _ => 0,
        }
    }

    fn handle_output_pin_verb(&mut self, verb_id: u16, payload8: u8) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_output_pin(payload8),
            0xF01 => self.output_pin.conn_select as u32,
            0x701 => {
                self.output_pin.conn_select = payload8;
                0
            }
            0xF02 => self.get_output_connection_list_entry(payload8),
            0xF07 => self.output_pin.pin_ctl as u32,
            0x707 => {
                self.output_pin.pin_ctl = payload8;
                0
            }
            0xF09 => {
                // GET_PIN_SENSE: report presence detect (bit31).
                1 << 31
            }
            0xF1C => self.output_pin.config_default,
            _ => 0,
        }
    }

    fn handle_mic_pin_verb(&mut self, verb_id: u16, payload8: u8) -> u32 {
        match verb_id {
            0xF00 => self.get_parameter_mic_pin(payload8),
            0xF01 => self.mic_pin.conn_select as u32,
            0x701 => {
                self.mic_pin.conn_select = payload8;
                0
            }
            0xF02 => self.get_mic_connection_list_entry(payload8),
            0xF07 => self.mic_pin.pin_ctl as u32,
            0x707 => {
                self.mic_pin.pin_ctl = payload8;
                0
            }
            0xF09 => 1 << 31,
            0xF1C => self.mic_pin.config_default,
            _ => 0,
        }
    }

    fn get_parameter_root(&self, param_id: u8) -> u32 {
        match param_id {
            0x00 => self.vendor_id,
            0x01 => self.subsystem_id,
            0x02 => self.revision_id,
            0x04 => (1u32 << 16) | 1u32, // start NID 1, one function group
            _ => 0,
        }
    }

    fn get_parameter_afg(&self, param_id: u8) -> u32 {
        match param_id {
            0x04 => (2u32 << 16) | 4u32, // widgets start at 2, count 4
            0x05 => 0x01,                // audio function group
            0x08 => 0,                   // audio FG caps (minimal)
            _ => 0,
        }
    }

    fn get_parameter_output(&self, param_id: u8) -> u32 {
        match param_id {
            0x09 => {
                // Audio widget capabilities: type=audio output (0), stereo, out amp present, format override.
                (0x0u32) | (1 << 4) | (1 << 6) | (1 << 8)
            }
            0x0A => {
                supported_pcm_caps()
            }
            0x0B => {
                // Supported stream formats. Returning non-zero tends to keep drivers happy.
                1
            }
            0x12 => {
                // AMP_OUT_CAP: 0..0x7f steps, 0 offset, 7-bit, mute supported.
                (0x7f) | (1 << 31)
            }
            _ => 0,
        }
    }

    fn get_parameter_input(&self, param_id: u8) -> u32 {
        match param_id {
            0x09 => {
                // Audio widget capabilities: type=audio input (1), stereo, in amp present, format override.
                (0x1u32) | (1 << 4) | (1 << 5) | (1 << 8)
            }
            0x0A => supported_pcm_caps(),
            0x0B => 1,
            _ => 0,
        }
    }

    fn get_parameter_output_pin(&self, param_id: u8) -> u32 {
        match param_id {
            0x09 => {
                // Audio widget capabilities: type=pin complex (0x4), connection list, power control.
                (0x4u32) | (1 << 12) | (1 << 10)
            }
            0x0A => supported_pcm_caps(),
            0x0B => 1,
            0x0C => {
                // PIN_CAP: output capable.
                (1 << 4) | (1 << 2)
            }
            0x0E => 1, // connection list length
            _ => 0,
        }
    }

    fn get_parameter_mic_pin(&self, param_id: u8) -> u32 {
        match param_id {
            0x09 => (0x4u32) | (1 << 12) | (1 << 10),
            0x0A => supported_pcm_caps(),
            0x0B => 1,
            0x0C => {
                // PIN_CAP: input capable.
                (1 << 5) | (1 << 2)
            }
            0x0E => 1,
            _ => 0,
        }
    }

    fn get_output_connection_list_entry(&self, index: u8) -> u32 {
        // One-entry connection list (index 0) to the output converter (NID 2).
        if index == 0 {
            2u32
        } else {
            0
        }
    }

    fn get_mic_connection_list_entry(&self, index: u8) -> u32 {
        // One-entry connection list (index 0) to the input converter (NID 4).
        if index == 0 {
            4u32
        } else {
            0
        }
    }

    fn set_amp_gain_mute(&mut self, payload: u16) {
        // Payload matches the HDA spec:
        // [15] direction (0=out,1=in) - ignore in amps
        // [13] left, [12] right (if neither set, apply to both)
        // [11:8] index (we only support index 0)
        // [7] mute, [6:0] gain
        if (payload & (1 << 15)) != 0 {
            return;
        }
        if ((payload >> 8) & 0x0f) != 0 {
            return;
        }

        let mute = (payload & (1 << 7)) != 0;
        let gain = (payload & 0x7f) as u8;
        let left = (payload & (1 << 13)) != 0;
        let right = (payload & (1 << 12)) != 0;

        match (left, right) {
            (false, false) => {
                self.output.amp_mute_left = mute;
                self.output.amp_mute_right = mute;
                self.output.amp_gain_left = gain;
                self.output.amp_gain_right = gain;
            }
            (true, false) => {
                self.output.amp_mute_left = mute;
                self.output.amp_gain_left = gain;
            }
            (false, true) => {
                self.output.amp_mute_right = mute;
                self.output.amp_gain_right = gain;
            }
            (true, true) => {
                self.output.amp_mute_left = mute;
                self.output.amp_mute_right = mute;
                self.output.amp_gain_left = gain;
                self.output.amp_gain_right = gain;
            }
        }
    }

    fn get_amp_gain_mute(&self, payload: u16) -> u32 {
        if (payload & (1 << 15)) != 0 {
            return 0;
        }
        if ((payload >> 8) & 0x0f) != 0 {
            return 0;
        }
        let left = (payload & (1 << 13)) != 0;
        let right = (payload & (1 << 12)) != 0;

        // If neither side specified, return left.
        let (mute, gain) = if right && !left {
            (self.output.amp_mute_right, self.output.amp_gain_right)
        } else {
            (self.output.amp_mute_left, self.output.amp_gain_left)
        };

        ((mute as u32) << 7) | gain as u32
    }
}

/// Minimal Intel HD Audio controller emulation.
#[derive(Debug, Clone)]
pub struct HdaController {
    gcap: u16,
    vmin: u8,
    vmaj: u8,
    gctl: u32,
    statests: u16,
    intctl: u32,
    intsts: u32,

    dplbase: u32,
    dpubase: u32,

    corblbase: u32,
    corbubase: u32,
    corbwp: u16,
    corbrp: u16,
    corbctl: u8,
    corbsts: u8,
    corbsize: u8,

    rirblbase: u32,
    rirbubase: u32,
    rirbwp: u16,
    rintcnt: u16,
    rirbctl: u8,
    rirbsts: u8,
    rirbsize: u8,

    streams: Vec<StreamDescriptor>,
    stream_rt: Vec<StreamRuntime>,

    codec: HdaCodec,

    pub audio_out: AudioRingBuffer,

    irq_pending: bool,
    output_rate_hz: u32,
}

impl HdaController {
    pub fn new() -> Self {
        let output_rate_hz = 48_000;
        let num_output_streams: usize = 1;
        let num_input_streams: usize = 1;
        let num_bidir_streams: usize = 0;
        let num_streams = num_output_streams + num_input_streams + num_bidir_streams;
        Self {
            // GCAP: OSS=1, ISS=1, BSS=0, NSDO=1.
            gcap: ((num_output_streams as u16) & 0xF)
                | (((num_input_streams as u16) & 0xF) << 4)
                | (((num_bidir_streams as u16) & 0xF) << 8)
                | (1u16 << 12),
            vmin: 0x00,
            vmaj: 0x01,
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
            corbsize: RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256 | 0x2, // 256 entries

            rirblbase: 0,
            rirbubase: 0,
            rirbwp: 0,
            rintcnt: 0,
            rirbctl: 0,
            rirbsts: 0,
            rirbsize: RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256 | 0x2, // 256 entries

            streams: vec![StreamDescriptor::default(); num_streams],
            stream_rt: (0..num_streams)
                .map(|_| StreamRuntime::new(output_rate_hz))
                .collect(),

            codec: HdaCodec::new(),
            audio_out: AudioRingBuffer::new_stereo(48_000 / 10), // ~100ms

            irq_pending: false,
            output_rate_hz,
        }
    }

    pub fn codec(&self) -> &HdaCodec {
        &self.codec
    }

    pub fn codec_mut(&mut self) -> &mut HdaCodec {
        &mut self.codec
    }

    pub fn stream_mut(&mut self, index: usize) -> &mut StreamDescriptor {
        &mut self.streams[index]
    }

    pub fn take_irq(&mut self) -> bool {
        let pending = self.irq_pending;
        self.irq_pending = false;
        pending
    }

    /// Advance the HDA device by `output_frames` worth of host time.
    pub fn process(&mut self, mem: &mut dyn MemoryAccess, output_frames: usize) {
        let mut silence = SilenceCaptureSource;
        self.process_inner(mem, output_frames, None, &mut silence);
    }

    /// Advance the HDA device by `output_frames` worth of host time, writing any produced audio
    /// directly into `sink`.
    pub fn process_into(
        &mut self,
        mem: &mut dyn MemoryAccess,
        output_frames: usize,
        sink: &mut dyn AudioSink,
    ) {
        let mut silence = SilenceCaptureSource;
        self.process_inner(mem, output_frames, Some(sink), &mut silence);
    }

    /// Advance the HDA device by `output_frames` worth of host time, pulling microphone samples
    /// from `capture` and DMA'ing them into the guest capture stream.
    pub fn process_with_capture(
        &mut self,
        mem: &mut dyn MemoryAccess,
        output_frames: usize,
        capture: &mut dyn AudioCaptureSource,
    ) {
        self.process_inner(mem, output_frames, None, capture);
    }

    /// Like [`Self::process_into`], but also services the capture stream using `capture`.
    pub fn process_into_with_capture(
        &mut self,
        mem: &mut dyn MemoryAccess,
        output_frames: usize,
        sink: &mut dyn AudioSink,
        capture: &mut dyn AudioCaptureSource,
    ) {
        self.process_inner(mem, output_frames, Some(sink), capture);
    }

    fn process_inner(
        &mut self,
        mem: &mut dyn MemoryAccess,
        output_frames: usize,
        mut sink: Option<&mut dyn AudioSink>,
        capture: &mut dyn AudioCaptureSource,
    ) {
        self.process_corb(mem);

        let out_samples = self.process_output_stream(mem, 0, output_frames);
        if !out_samples.is_empty() {
            if let Some(ref mut sink) = sink {
                sink.push_interleaved_f32(&out_samples);
            } else {
                self.audio_out.push_interleaved_stereo(&out_samples);
            }
        }

        // Capture stream (stream descriptor 1).
        if self.streams.len() > 1 {
            self.process_capture_stream(mem, 1, output_frames, capture);
        }

        self.update_position_buffer(mem);
    }

    /// Convenience helper that derives `output_frames` from an [`AudioFrameClock`].
    ///
    /// This allows callers to drive the device model from a monotonic clock without needing to
    /// re-implement time→frames conversion (and avoids cumulative rounding drift).
    pub fn process_with_clock(
        &mut self,
        mem: &mut dyn MemoryAccess,
        clock: &mut AudioFrameClock,
        now_ns: u64,
        sink: &mut dyn AudioSink,
    ) {
        debug_assert_eq!(
            clock.sample_rate_hz, self.output_rate_hz,
            "AudioFrameClock sample rate must match HDA output rate"
        );
        let output_frames = clock.advance_to(now_ns);
        self.process_into(mem, output_frames, sink);
    }

    /// Like [`Self::process_with_clock`], but also services the capture stream using `capture`.
    pub fn process_with_clock_and_capture(
        &mut self,
        mem: &mut dyn MemoryAccess,
        clock: &mut AudioFrameClock,
        now_ns: u64,
        sink: &mut dyn AudioSink,
        capture: &mut dyn AudioCaptureSource,
    ) {
        debug_assert_eq!(
            clock.sample_rate_hz, self.output_rate_hz,
            "AudioFrameClock sample rate must match HDA output rate"
        );
        let output_frames = clock.advance_to(now_ns);
        self.process_into_with_capture(mem, output_frames, sink, capture);
    }

    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let end = match offset.checked_add(size as u64) {
            Some(end) => end,
            None => return 0,
        };
        if end > HDA_MMIO_SIZE as u64 {
            return 0;
        }

        match (offset, size) {
            (REG_GCAP, 2) => self.gcap as u64,
            (REG_VMIN, 1) => self.vmin as u64,
            (REG_VMAJ, 1) => self.vmaj as u64,
            (REG_GCTL, 4) => self.gctl as u64,
            (REG_STATESTS, 2) => self.statests as u64,
            (REG_INTCTL, 4) => self.intctl as u64,
            (REG_INTSTS, 4) => self.intsts as u64,

            (REG_CORBLBASE, 4) => self.corblbase as u64,
            (REG_CORBUBASE, 4) => self.corbubase as u64,
            (REG_CORBWP, 2) => self.corbwp as u64,
            (REG_CORBRP, 2) => self.corbrp as u64,
            (REG_CORBCTL, 1) => self.corbctl as u64,
            (REG_CORBSTS, 1) => self.corbsts as u64,
            (REG_CORBSIZE, 1) => self.corbsize as u64,

            (REG_RIRBLBASE, 4) => self.rirblbase as u64,
            (REG_RIRBUBASE, 4) => self.rirbubase as u64,
            (REG_RIRBWP, 2) => self.rirbwp as u64,
            (REG_RINTCNT, 2) => self.rintcnt as u64,
            (REG_RIRBCTL, 1) => self.rirbctl as u64,
            (REG_RIRBSTS, 1) => self.rirbsts as u64,
            (REG_RIRBSIZE, 1) => self.rirbsize as u64,

            (REG_DPLBASE, 4) => self.dplbase as u64,
            (REG_DPUBASE, 4) => self.dpubase as u64,

            _ if offset >= REG_SD_BASE
                && offset < REG_SD_BASE + SD_STRIDE * self.streams.len() as u64 =>
            {
                let stream = ((offset - REG_SD_BASE) / SD_STRIDE) as usize;
                let reg = (offset - REG_SD_BASE) % SD_STRIDE;
                let sd = &self.streams[stream];
                if reg < SD_REG_LPIB {
                    let start = (reg - SD_REG_CTL) as usize;
                    if size > 4 || start.saturating_add(size) > 4 {
                        return 0;
                    }
                    let bytes = sd.ctl.to_le_bytes();
                    let mut out = 0u64;
                    for i in 0..size {
                        out |= (bytes[start + i] as u64) << (8 * i);
                    }
                    return out;
                }
                match (reg, size) {
                    (SD_REG_LPIB, 4) => sd.lpib as u64,
                    (SD_REG_CBL, 4) => sd.cbl as u64,
                    (SD_REG_LVI, 2) => sd.lvi as u64,
                    (SD_REG_FIFOS, 2) => sd.fifos as u64,
                    (SD_REG_FMT, 2) => sd.fmt as u64,
                    (SD_REG_BDPL, 4) => sd.bdpl as u64,
                    (SD_REG_BDPU, 4) => sd.bdpu as u64,
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let end = match offset.checked_add(size as u64) {
            Some(end) => end,
            None => return,
        };
        if end > HDA_MMIO_SIZE as u64 {
            return;
        }

        match (offset, size) {
            (REG_GCTL, 4) => {
                let v = value as u32;
                let prev = self.gctl;
                self.gctl = v;
                let prev_crst = (prev & GCTL_CRST) != 0;
                let new_crst = (v & GCTL_CRST) != 0;
                if prev_crst && !new_crst {
                    self.reset();
                } else if !prev_crst && new_crst {
                    // Leaving reset: report codec 0 presence.
                    self.statests |= 1;
                }
            }
            (REG_STATESTS, 2) => {
                // RW1C
                self.statests &= !(value as u16);
            }
            (REG_INTCTL, 4) => {
                self.intctl = value as u32;
                self.update_irq_line();
            }
            (REG_INTSTS, 4) => {
                // RW1C
                self.intsts &= !(value as u32);
                if (self.intsts & (INTSTS_CIS | 0x3fff_ffff)) == 0 {
                    self.intsts &= !INTSTS_GIS;
                }
                self.update_irq_line();
            }

            (REG_CORBLBASE, 4) => self.corblbase = value as u32,
            (REG_CORBUBASE, 4) => self.corbubase = value as u32,
            (REG_CORBWP, 2) => self.corbwp = (value as u16) & 0x00ff,
            (REG_CORBRP, 2) => {
                let v = value as u16;
                if (v & 0x8000) != 0 {
                    self.corbrp = 0;
                } else {
                    self.corbrp = v & 0x00ff;
                }
            }
            (REG_CORBCTL, 1) => self.corbctl = value as u8,
            (REG_CORBSTS, 1) => {
                self.corbsts &= !(value as u8);
            }
            (REG_CORBSIZE, 1) => {
                // Only the size selection bits (1:0) are writable; capability bits are RO.
                self.corbsize = (self.corbsize & !0x3) | (value as u8 & 0x3);
            }

            (REG_RIRBLBASE, 4) => self.rirblbase = value as u32,
            (REG_RIRBUBASE, 4) => self.rirbubase = value as u32,
            (REG_RIRBWP, 2) => {
                let v = value as u16;
                if (v & 0x8000) != 0 {
                    self.rirbwp = 0;
                } else {
                    self.rirbwp = v & 0x00ff;
                }
            }
            (REG_RINTCNT, 2) => self.rintcnt = value as u16,
            (REG_RIRBCTL, 1) => self.rirbctl = value as u8,
            (REG_RIRBSTS, 1) => self.rirbsts &= !(value as u8),
            (REG_RIRBSIZE, 1) => {
                self.rirbsize = (self.rirbsize & !0x3) | (value as u8 & 0x3);
            }

            (REG_DPLBASE, 4) => {
                let v = value as u32;
                // Bits 6:1 are reserved and must read as 0; the base is 128-byte aligned.
                self.dplbase = (v & DPLBASE_ENABLE) | (v & DPLBASE_ADDR_MASK);
            }
            (REG_DPUBASE, 4) => self.dpubase = value as u32,

            _ if offset >= REG_SD_BASE
                && offset < REG_SD_BASE + SD_STRIDE * self.streams.len() as u64 =>
            {
                let stream = ((offset - REG_SD_BASE) / SD_STRIDE) as usize;
                let reg = (offset - REG_SD_BASE) % SD_STRIDE;
                if reg < SD_REG_LPIB {
                    if reg == SD_REG_CTL + 3 && size == 1 {
                        // SDSTS is RW1C.
                        self.clear_stream_status(stream, value as u8);
                        return;
                    }

                    if reg == SD_REG_CTL && size == 4 {
                        // Combined SDnCTL/SDnSTS dword write.
                        //
                        // Real hardware exposes SDnSTS as a separate byte register at offset 0x03.
                        // Some callers may still issue a dword write where the upper byte is used
                        // as a RW1C clear mask. To keep the model robust, treat the upper byte as
                        // status clear, then apply the low 24-bit control update.
                        let v = value as u32;
                        let sts_clear = (v >> 24) as u8;
                        if sts_clear != 0 {
                            self.clear_stream_status(stream, sts_clear);
                        }

                        let (prev, now) = {
                            let sd = &mut self.streams[stream];
                            let prev = sd.ctl;
                            let prev_ctl = prev & 0x00ff_ffff;
                            let status = sd.ctl & 0xff00_0000;
                            let write_ctl = v & 0x00ff_ffff;
                            let new_ctl = if write_ctl == 0 && sts_clear != 0 {
                                // Heuristic: status-only write should not stop the stream.
                                prev_ctl
                            } else {
                                write_ctl
                            };
                            sd.ctl = status | new_ctl;
                            (prev, sd.ctl)
                        };

                        // SRST cleared -> stream enters reset.
                        if (prev & SD_CTL_SRST) != 0 && (now & SD_CTL_SRST) == 0 {
                            self.reset_stream_engine(stream);
                        }
                        return;
                    }

                    // We only model writes fully contained in the SDnCTL bytes (0..2). Writes
                    // touching the SDnSTS byte must use the 1-byte RW1C path above.
                    let start = (reg - SD_REG_CTL) as usize;
                    if size > 4 || start.saturating_add(size) > 3 {
                        return;
                    }

                    let (prev, now) = {
                        let sd = &mut self.streams[stream];
                        let mut bytes = sd.ctl.to_le_bytes();
                        for i in 0..size {
                            bytes[start + i] = ((value >> (8 * i)) & 0xff) as u8;
                        }
                        let prev = sd.ctl;
                        sd.ctl = u32::from_le_bytes(bytes);
                        (prev, sd.ctl)
                    };

                    if (prev & SD_CTL_SRST) != 0 && (now & SD_CTL_SRST) == 0 {
                        self.reset_stream_engine(stream);
                    }
                    return;
                }

                let sd = &mut self.streams[stream];
                match (reg, size) {
                    (SD_REG_LPIB, 4) => {
                        // Read-only in hardware.
                    }
                    (SD_REG_CBL, 4) => sd.cbl = value as u32,
                    (SD_REG_LVI, 2) => sd.lvi = value as u16,
                    (SD_REG_FIFOS, 2) => sd.fifos = value as u16,
                    (SD_REG_FMT, 2) => sd.fmt = value as u16,
                    (SD_REG_BDPL, 4) => sd.bdpl = value as u32,
                    (SD_REG_BDPU, 4) => sd.bdpu = value as u32,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.gctl = 0;
        self.statests = 0;
        self.intctl = 0;
        self.intsts = 0;
        self.irq_pending = false;

        self.dplbase = 0;
        self.dpubase = 0;

        self.corbwp = 0;
        self.corbrp = 0;
        self.corbctl = 0;
        self.corbsts = 0;
        self.corbsize = RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256 | 0x2;

        self.rirbwp = 0;
        self.rintcnt = 0;
        self.rirbctl = 0;
        self.rirbsts = 0;
        self.rirbsize = RING_SIZE_CAP_2 | RING_SIZE_CAP_16 | RING_SIZE_CAP_256 | 0x2;

        for (sd, rt) in self.streams.iter_mut().zip(self.stream_rt.iter_mut()) {
            *sd = StreamDescriptor::default();
            rt.reset(self.output_rate_hz);
        }

        self.audio_out.clear();
        self.codec = HdaCodec::new();
    }

    fn posbuf_enabled(&self) -> bool {
        (self.dplbase & DPLBASE_ENABLE) != 0
    }

    fn posbuf_base_addr(&self) -> u64 {
        ((self.dpubase as u64) << 32) | (self.dplbase & DPLBASE_ADDR_MASK) as u64
    }

    fn update_position_buffer(&mut self, mem: &mut dyn MemoryAccess) {
        if (self.gctl & GCTL_CRST) == 0 {
            return;
        }
        if !self.posbuf_enabled() {
            return;
        }

        let base = self.posbuf_base_addr();
        for (stream, sd) in self.streams.iter().enumerate() {
            let entry_addr = base + (stream as u64) * 8;
            mem.write_u32(entry_addr, sd.lpib);
            mem.write_u32(entry_addr + 4, 0);
        }
    }

    fn corb_entries(&self) -> u16 {
        match self.corbsize & 0x3 {
            0 => 2,
            1 => 16,
            _ => 256,
        }
    }

    fn rirb_entries(&self) -> u16 {
        match self.rirbsize & 0x3 {
            0 => 2,
            1 => 16,
            _ => 256,
        }
    }

    fn corb_base(&self) -> u64 {
        ((self.corbubase as u64) << 32) | (self.corblbase as u64)
    }

    fn rirb_base(&self) -> u64 {
        ((self.rirbubase as u64) << 32) | (self.rirblbase as u64)
    }

    fn process_corb(&mut self, mem: &mut dyn MemoryAccess) {
        if (self.gctl & GCTL_CRST) == 0 {
            return;
        }
        if (self.corbctl & CORBCTL_RUN) == 0 || (self.rirbctl & RIRBCTL_RUN) == 0 {
            return;
        }

        let entries = self.corb_entries();
        let corb_base = self.corb_base();

        while self.corbrp != self.corbwp {
            self.corbrp = (self.corbrp + 1) % entries;
            let addr = corb_base + self.corbrp as u64 * 4;
            let cmd = mem.read_u32(addr);

            let cad = ((cmd >> 28) & 0x0f) as u8;
            let nid = ((cmd >> 20) & 0x7f) as u8;
            let verb_20 = cmd & 0x000f_ffff;

            let resp = if cad == 0 {
                self.codec.execute_verb(nid, verb_20)
            } else {
                0
            };
            self.write_rirb_response(mem, cad, resp);
        }
    }

    fn write_rirb_response(&mut self, mem: &mut dyn MemoryAccess, cad: u8, resp: u32) {
        let entries = self.rirb_entries();
        self.rirbwp = (self.rirbwp + 1) % entries;

        let addr = self.rirb_base() + self.rirbwp as u64 * 8;
        mem.write_u32(addr, resp);
        mem.write_u32(addr + 4, cad as u32);

        self.rirbsts |= 1; // response received
        if (self.rirbctl & RIRBCTL_RINTCTL) != 0 {
            self.raise_controller_interrupt();
        }
    }

    fn raise_controller_interrupt(&mut self) {
        self.intsts |= INTSTS_CIS;
        self.intsts |= INTSTS_GIS;
        self.update_irq_line();
    }

    fn raise_stream_interrupt(&mut self, stream: usize) {
        self.intsts |= 1 << stream;
        self.intsts |= INTSTS_GIS;

        // Set BCIS in SDSTS (upper byte of SDnCTL in this simplified model).
        self.streams[stream].ctl |= SD_STS_BCIS << 24;
        self.update_irq_line();
    }

    fn recalc_intsts_gis(&mut self) {
        if (self.intsts & (INTSTS_CIS | 0x3fff_ffff)) != 0 {
            self.intsts |= INTSTS_GIS;
        } else {
            self.intsts &= !INTSTS_GIS;
        }
    }

    fn clear_stream_status(&mut self, stream: usize, clear: u8) {
        if clear == 0 {
            return;
        }

        let sd = &mut self.streams[stream];
        let prev = (sd.ctl >> 24) as u8;
        let new = prev & !clear;
        if prev == new {
            return;
        }
        sd.ctl = (sd.ctl & 0x00ff_ffff) | ((new as u32) << 24);

        // In hardware, clearing SDSTS.BCIS also clears the corresponding SIS bit in INTSTS.
        let bcis = SD_STS_BCIS as u8;
        if (prev & bcis) != 0 && (new & bcis) == 0 {
            self.intsts &= !(1 << stream);
        }

        self.recalc_intsts_gis();
        self.update_irq_line();
    }

    fn reset_stream_engine(&mut self, stream: usize) {
        let sd = &mut self.streams[stream];
        sd.lpib = 0;
        // Stream reset clears SDSTS and any pending interrupt for the stream.
        sd.ctl &= 0x00ff_ffff;
        self.intsts &= !(1 << stream);
        self.stream_rt[stream].reset(self.output_rate_hz);
        self.recalc_intsts_gis();
        self.update_irq_line();
    }

    fn update_irq_line(&mut self) {
        // Simplified: assert if global interrupt enable is set and any enabled
        // interrupt source is pending.
        if (self.intctl & INTCTL_GIE) == 0 {
            self.irq_pending = false;
            return;
        }

        let pending_streams = self.intsts & 0x3fff_ffff;
        let enabled_streams = self.intctl & 0x3fff_ffff;
        let pending_controller = (self.intsts & INTSTS_CIS) != 0 && (self.intctl & (1 << 30)) != 0;

        self.irq_pending = (pending_streams & enabled_streams) != 0 || pending_controller;
    }

    fn process_output_stream(
        &mut self,
        mem: &mut dyn MemoryAccess,
        stream: usize,
        output_frames: usize,
    ) -> Vec<f32> {
        if (self.gctl & GCTL_CRST) == 0 {
            return Vec::new();
        }
        let sd = &self.streams[stream];
        if (sd.ctl & (SD_CTL_SRST | SD_CTL_RUN)) != (SD_CTL_SRST | SD_CTL_RUN) {
            return Vec::new();
        }
        let stream_num = ((sd.ctl & SD_CTL_STRM_MASK) >> SD_CTL_STRM_SHIFT) as u8;
        if stream_num == 0 || stream_num != self.codec.output_stream_id() {
            return Vec::new();
        }

        let fmt_raw = sd.fmt;
        if fmt_raw == 0 {
            return Vec::new();
        }

        let fmt = StreamFormat::from_hda_format(fmt_raw);

        let need_src = {
            let rt = &mut self.stream_rt[stream];
            if rt.last_fmt_raw != fmt_raw
                || rt.resampler.src_rate_hz() != fmt.sample_rate_hz
                || rt.resampler.dst_rate_hz() != self.output_rate_hz
            {
                rt.resampler
                    .reset_rates(fmt.sample_rate_hz, self.output_rate_hz);
                rt.last_fmt_raw = fmt_raw;
                rt.bdl_index = 0;
                rt.bdl_offset = 0;
            }

            // Ensure the resampler has enough source frames queued to synthesize the requested output.
            let required_src = rt.resampler.required_source_frames(output_frames);
            let queued_src = rt.resampler.queued_source_frames();
            required_src.saturating_sub(queued_src)
        };

        if need_src > 0 {
            let bytes = need_src * fmt.bytes_per_frame();
            let raw = self.dma_read_stream_bytes(mem, stream, bytes);
            let decoded = decode_pcm_to_stereo_f32(&raw, fmt);
            self.stream_rt[stream]
                .resampler
                .push_source_frames(&decoded);
        }

        let mut out = self.stream_rt[stream]
            .resampler
            .produce_interleaved_stereo(output_frames);
        self.apply_codec_output_controls(&mut out);
        out
    }

    fn apply_codec_output_controls(&self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }
        let [gain_l, gain_r] = self.codec.output_gain_scalars();
        if gain_l == 1.0 && gain_r == 1.0 {
            return;
        }

        for frame in samples.chunks_exact_mut(2) {
            frame[0] = apply_gain(frame[0], gain_l);
            frame[1] = apply_gain(frame[1], gain_r);
        }

        fn apply_gain(sample: f32, gain: f32) -> f32 {
            let mut sample = if sample.is_finite() { sample } else { 0.0 };
            sample *= gain;
            if sample > 1.0 {
                1.0
            } else if sample < -1.0 {
                -1.0
            } else {
                sample
            }
        }
    }

    fn process_capture_stream(
        &mut self,
        mem: &mut dyn MemoryAccess,
        stream: usize,
        output_frames: usize,
        capture: &mut dyn AudioCaptureSource,
    ) {
        if (self.gctl & GCTL_CRST) == 0 {
            return;
        }

        let sd = &self.streams[stream];
        if (sd.ctl & (SD_CTL_SRST | SD_CTL_RUN)) != (SD_CTL_SRST | SD_CTL_RUN) {
            return;
        }

        let stream_num = ((sd.ctl & SD_CTL_STRM_MASK) >> SD_CTL_STRM_SHIFT) as u8;
        if stream_num == 0 || stream_num != self.codec.input_stream_id() {
            return;
        }

        let fmt_raw = sd.fmt;
        if fmt_raw == 0 {
            return;
        }

        let fmt = StreamFormat::from_hda_format(fmt_raw);

        let dst_frames = {
            let rt = &mut self.stream_rt[stream];
            if rt.last_fmt_raw != fmt_raw
                || rt.resampler.src_rate_hz() != self.output_rate_hz
                || rt.resampler.dst_rate_hz() != fmt.sample_rate_hz
            {
                rt.resampler.reset_rates(self.output_rate_hz, fmt.sample_rate_hz);
                rt.last_fmt_raw = fmt_raw;
                rt.bdl_index = 0;
                rt.bdl_offset = 0;
                rt.capture_frame_accum = 0;
            }

            // Convert host time (output_frames @ output_rate_hz) into the number of guest-rate frames.
            rt.capture_frame_accum = rt
                .capture_frame_accum
                .wrapping_add(output_frames as u64 * fmt.sample_rate_hz as u64);
            let frames = (rt.capture_frame_accum / self.output_rate_hz as u64) as usize;
            rt.capture_frame_accum %= self.output_rate_hz as u64;
            frames
        };

        if dst_frames == 0 {
            return;
        }

        let need_src = {
            let rt = &mut self.stream_rt[stream];
            let required_src = rt.resampler.required_source_frames(dst_frames);
            let queued_src = rt.resampler.queued_source_frames();
            required_src.saturating_sub(queued_src)
        };

        if need_src > 0 {
            let mut mono = vec![0.0f32; need_src];
            let got = capture.read_mono_f32(&mut mono);
            if got < need_src {
                mono[got..].fill(0.0);
            }

            let stereo: Vec<[f32; 2]> = mono.iter().map(|&s| [s, s]).collect();
            self.stream_rt[stream]
                .resampler
                .push_source_frames(&stereo);
        }

        let stereo = self.stream_rt[stream]
            .resampler
            .produce_interleaved_stereo(dst_frames);
        if stereo.is_empty() {
            return;
        }

        // Downmix back to mono (the resampler operates on stereo).
        let produced_frames = stereo.len() / 2;
        let mut mono = Vec::with_capacity(produced_frames);
        for frame in 0..produced_frames {
            mono.push(stereo[frame * 2]);
        }

        let bytes = encode_mono_f32_to_pcm(&mono, fmt);
        let _ = self.dma_write_stream_bytes(mem, stream, &bytes);
    }

    fn dma_read_stream_bytes(
        &mut self,
        mem: &mut dyn MemoryAccess,
        stream: usize,
        mut bytes: usize,
    ) -> Vec<u8> {
        if self.streams[stream].cbl == 0 {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(bytes);
        let mut fire_ioc = false;

        {
            let sd = &mut self.streams[stream];
            let rt = &mut self.stream_rt[stream];

            // BDPL is 128-byte aligned in hardware; low bits must read as 0.
            let bdl_base = ((sd.bdpu as u64) << 32) | (sd.bdpl as u64 & !0x7f);

            while bytes > 0 {
                let entry = read_bdl_entry(mem, bdl_base, rt.bdl_index as usize);
                if entry.len == 0 {
                    break;
                }

                let remaining = entry.len.saturating_sub(rt.bdl_offset).min(bytes as u32) as usize;
                if remaining == 0 {
                    // Move to next entry.
                    rt.bdl_offset = 0;
                    if rt.bdl_index >= sd.lvi {
                        rt.bdl_index = 0;
                    } else {
                        rt.bdl_index += 1;
                    }
                    continue;
                }

                let mut chunk = vec![0u8; remaining];
                mem.read_physical(entry.addr + rt.bdl_offset as u64, &mut chunk);
                out.extend_from_slice(&chunk);

                rt.bdl_offset += remaining as u32;
                bytes -= remaining;

                // Update LPIB, wrapping at CBL if set.
                sd.lpib = sd.lpib.wrapping_add(remaining as u32);
                if sd.cbl != 0 && sd.lpib >= sd.cbl {
                    sd.lpib %= sd.cbl;
                }

                if rt.bdl_offset >= entry.len {
                    rt.bdl_offset = 0;
                    if entry.ioc {
                        // Latch BCIS regardless of IOCE (IOCE only controls interrupt generation).
                        sd.ctl |= SD_STS_BCIS << 24;
                        if (sd.ctl & SD_CTL_IOCE) != 0 {
                            fire_ioc = true;
                        }
                    }
                    if rt.bdl_index >= sd.lvi {
                        rt.bdl_index = 0;
                    } else {
                        rt.bdl_index += 1;
                    }
                }
            }
        }

        if fire_ioc {
            self.raise_stream_interrupt(stream);
        }

        out
    }

    fn dma_write_stream_bytes(
        &mut self,
        mem: &mut dyn MemoryAccess,
        stream: usize,
        mut bytes: &[u8],
    ) -> usize {
        if self.streams[stream].cbl == 0 {
            return 0;
        }

        let mut written = 0usize;
        let mut fire_ioc = false;

        {
            let sd = &mut self.streams[stream];
            let rt = &mut self.stream_rt[stream];

            let bdl_base = ((sd.bdpu as u64) << 32) | (sd.bdpl as u64 & !0x7f);

            while !bytes.is_empty() {
                let entry = read_bdl_entry(mem, bdl_base, rt.bdl_index as usize);
                if entry.len == 0 {
                    break;
                }

                let remaining =
                    entry.len.saturating_sub(rt.bdl_offset).min(bytes.len() as u32) as usize;
                if remaining == 0 {
                    // Move to next entry.
                    rt.bdl_offset = 0;
                    if rt.bdl_index >= sd.lvi {
                        rt.bdl_index = 0;
                    } else {
                        rt.bdl_index += 1;
                    }
                    continue;
                }

                mem.write_physical(entry.addr + rt.bdl_offset as u64, &bytes[..remaining]);
                bytes = &bytes[remaining..];
                written += remaining;

                rt.bdl_offset += remaining as u32;

                // Update LPIB, wrapping at CBL if set.
                sd.lpib = sd.lpib.wrapping_add(remaining as u32);
                if sd.cbl != 0 && sd.lpib >= sd.cbl {
                    sd.lpib %= sd.cbl;
                }

                if rt.bdl_offset >= entry.len {
                    rt.bdl_offset = 0;
                    if entry.ioc {
                        sd.ctl |= SD_STS_BCIS << 24;
                        if (sd.ctl & SD_CTL_IOCE) != 0 {
                            fire_ioc = true;
                        }
                    }
                    if rt.bdl_index >= sd.lvi {
                        rt.bdl_index = 0;
                    } else {
                        rt.bdl_index += 1;
                    }
                }
            }
        }

        if fire_ioc {
            self.raise_stream_interrupt(stream);
        }

        written
    }
}

/// Very small PCI wrapper for the HDA controller.
///
/// The wider Aero codebase will likely have a full PCI bus + BAR allocation
/// story; this model exists primarily so tests can validate that the device has
/// sensible PCI identifiers.
#[derive(Debug, Clone)]
pub struct HdaPciDevice {
    config: [u8; 256],
    pub hda: HdaController,
}

impl HdaPciDevice {
    pub fn new() -> Self {
        let mut config = [0u8; 256];
        // Vendor / device.
        config[0x00..0x02].copy_from_slice(&0x8086u16.to_le_bytes());
        config[0x02..0x04].copy_from_slice(&0x2668u16.to_le_bytes()); // ICH6 HDA
                                                                      // Revision ID.
        config[0x08] = 0x01;
        // Class code: multimedia audio controller (0x04), HDA (0x03).
        config[0x09] = 0x00; // prog-if
        config[0x0a] = 0x03; // subclass
        config[0x0b] = 0x04; // class
                             // Interrupt pin: INTA#.
        config[0x3d] = 0x01;

        Self {
            config,
            hda: HdaController::new(),
        }
    }

    pub fn config_read_u32(&self, offset: u64) -> u32 {
        let o = offset as usize;
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.config[o..o + 4]);
        u32::from_le_bytes(b)
    }

    pub fn config_write_u32(&mut self, offset: u64, value: u32) {
        let o = offset as usize;
        self.config[o..o + 4].copy_from_slice(&value.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::GuestMemory;

    fn verb_12(verb_id: u16, payload8: u8) -> u32 {
        ((verb_id as u32) << 8) | payload8 as u32
    }

    fn verb_4(group: u16, payload16: u16) -> u32 {
        let verb_id = (group << 8) | ((payload16 >> 8) as u16);
        ((verb_id as u32) << 8) | (payload16 as u8 as u32)
    }

    fn cmd(cad: u8, nid: u8, verb_20: u32) -> u32 {
        ((cad as u32) << 28) | ((nid as u32) << 20) | (verb_20 & 0x000f_ffff)
    }

    #[test]
    fn codec_verbs_round_trip() {
        let mut codec = HdaCodec::new();

        // Root parameters.
        assert_eq!(codec.execute_verb(0, verb_12(0xF00, 0x00)), 0x1af4_1620);
        // AFG widget enumeration should include both output and capture widgets.
        assert_eq!(codec.execute_verb(1, verb_12(0xF00, 0x04)), (2u32 << 16) | 4u32);

        // Set converter stream/channel.
        assert_eq!(codec.execute_verb(2, verb_12(0x706, 0x10)), 0);
        assert_eq!(codec.execute_verb(2, verb_12(0xF06, 0)), 0x10);

        // Set/get converter format (16-bit payload encoded in low 16 bits).
        assert_eq!(codec.execute_verb(2, verb_4(0x2, 0x1234)), 0);
        assert_eq!(codec.execute_verb(2, verb_12(0xA00, 0)), 0x1234);

        // Amp gain/mute.
        let set_left = (1 << 13) | (1 << 7) | 0x22;
        assert_eq!(codec.execute_verb(2, verb_4(0x3, set_left)), 0);
        let got = codec.execute_verb(2, verb_4(0xB, 1 << 13));
        assert_eq!(got & 0x7f, 0x22);
        assert_eq!((got >> 7) & 1, 1);

        // Input converter + mic pin should support the basic capture verbs.
        assert_eq!(codec.execute_verb(4, verb_12(0x706, 0x20)), 0);
        assert_eq!(codec.execute_verb(4, verb_12(0xF06, 0)), 0x20);
        assert_eq!(codec.execute_verb(4, verb_4(0x2, 0x0010)), 0);
        assert_eq!(codec.execute_verb(4, verb_12(0xA00, 0)), 0x0010);
        assert_eq!(codec.execute_verb(5, verb_12(0xF02, 0)), 4);
    }

    #[test]
    fn corb_rirb_processes_commands() {
        let mut hda = HdaController::new();
        let mut mem = GuestMemory::new(0x4000);

        // Enable controller.
        hda.mmio_write(REG_GCTL, 4, GCTL_CRST as u64);

        // Setup CORB/RIRB in guest memory.
        let corb_base = 0x1000u64;
        let rirb_base = 0x2000u64;
        hda.mmio_write(REG_CORBLBASE, 4, corb_base as u64);
        hda.mmio_write(REG_RIRBLBASE, 4, rirb_base as u64);

        // Set pointers so first command/response lands at entry 0.
        hda.mmio_write(REG_CORBRP, 2, 0x00ff);
        hda.mmio_write(REG_RIRBWP, 2, 0x00ff);

        // Enable response interrupts (CIS) and global interrupt.
        hda.mmio_write(REG_INTCTL, 4, (INTCTL_GIE | (1 << 30)) as u64);
        hda.mmio_write(REG_RIRBCTL, 1, (RIRBCTL_RUN | RIRBCTL_RINTCTL) as u64);
        hda.mmio_write(REG_CORBCTL, 1, CORBCTL_RUN as u64);

        // Queue one verb: root GET_PARAMETER vendor id.
        let verb = verb_12(0xF00, 0x00);
        mem.write_u32(corb_base + 0 * 4, cmd(0, 0, verb));
        hda.mmio_write(REG_CORBWP, 2, 0x0000);

        hda.process(&mut mem, 0);

        let resp = mem.read_u32(rirb_base + 0 * 8);
        assert_eq!(resp, 0x1af4_1620);
        assert!(hda.take_irq());
        assert_ne!(hda.mmio_read(REG_INTSTS, 4) as u32 & INTSTS_CIS, 0);
    }
}
