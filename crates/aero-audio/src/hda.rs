use crate::capture::{AudioCaptureSource, SilenceCaptureSource};
use crate::clock::AudioFrameClock;
use crate::mem::MemoryAccess;
use crate::pcm::{
    decode_pcm_to_stereo_f32_into, encode_mono_f32_to_pcm_into, LinearResampler, StreamFormat,
};
use crate::ring::AudioRingBuffer;
use crate::sink::AudioSink;

#[cfg(feature = "io-snapshot")]
use aero_io_snapshot::io::audio::state::{
    AudioWorkletRingState, HdaCodecCaptureState, HdaCodecState, HdaControllerState,
    HdaStreamRuntimeState, HdaStreamState,
};

/// Size of the HDA MMIO region.
pub const HDA_MMIO_SIZE: usize = 0x4000;

// Global register offsets (subset).
const REG_GCAP: u64 = 0x00;
const REG_VMIN: u64 = 0x02;
const REG_VMAJ: u64 = 0x03;
const REG_GCTL: u64 = 0x08;
const REG_WAKEEN: u64 = 0x0c;
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
// Note: Per the Intel HDA spec, SDnCTL is 3 bytes (0x00..=0x02) and SDnSTS is a
// single byte at 0x03.
const SD_REG_CTL: u64 = 0x00;
const SD_REG_STS: u64 = 0x03;
const SD_REG_LPIB: u64 = 0x04;
const SD_REG_CBL: u64 = 0x08;
const SD_REG_LVI: u64 = 0x0c;
const SD_REG_FIFOW: u64 = 0x0e;
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
#[derive(Debug, Clone, Default)]
pub struct StreamDescriptor {
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

#[derive(Debug, Clone)]
struct StreamRuntime {
    bdl_index: u16,
    bdl_offset: u32,
    resampler: LinearResampler,
    last_fmt_raw: u16,
    capture_frame_accum: u64,
    dma_scratch: Vec<u8>,
    decode_scratch: Vec<[f32; 2]>,
    resample_out_scratch: Vec<f32>,
    capture_mono_scratch: Vec<f32>,
}

impl StreamRuntime {
    fn new(output_rate_hz: u32) -> Self {
        Self {
            bdl_index: 0,
            bdl_offset: 0,
            resampler: LinearResampler::new(output_rate_hz, output_rate_hz),
            last_fmt_raw: 0,
            capture_frame_accum: 0,
            dma_scratch: Vec::new(),
            decode_scratch: Vec::new(),
            resample_out_scratch: Vec::new(),
            capture_mono_scratch: Vec::new(),
        }
    }

    fn reset(&mut self, output_rate_hz: u32) {
        self.bdl_index = 0;
        self.bdl_offset = 0;
        self.resampler.reset_rates(output_rate_hz, output_rate_hz);
        self.last_fmt_raw = 0;
        self.capture_frame_accum = 0;
        self.dma_scratch.clear();
        self.decode_scratch.clear();
        self.resample_out_scratch.clear();
        self.capture_mono_scratch.clear();
    }
}

#[derive(Debug, Clone, Copy)]
struct BdlEntry {
    addr: u64,
    len: u32,
    ioc: bool,
}

fn read_bdl_entry(mem: &dyn MemoryAccess, base: u64, index: usize) -> BdlEntry {
    let entry_off = u64::try_from(index)
        .ok()
        .and_then(|idx| idx.checked_mul(16))
        .and_then(|off| base.checked_add(off));
    let Some(addr) = entry_off else {
        // Invalid/overflowing BDL pointer: treat as a null entry so callers stop DMA.
        return BdlEntry {
            addr: 0,
            len: 0,
            ioc: false,
        };
    };

    let buf_addr = mem.read_u64(addr);
    let len = addr.checked_add(8).map(|addr| mem.read_u32(addr)).unwrap_or(0);
    let flags = addr
        .checked_add(12)
        .map(|addr| mem.read_u32(addr))
        .unwrap_or(0);
    BdlEntry {
        addr: buf_addr,
        len,
        ioc: (flags & 1) != 0,
    }
}

fn dma_read_stream_bytes(
    mem: &dyn MemoryAccess,
    sd: &mut StreamDescriptor,
    bdl_index: &mut u16,
    bdl_offset: &mut u32,
    mut bytes: usize,
    out: &mut Vec<u8>,
) -> bool {
    out.clear();
    if sd.cbl == 0 || bytes == 0 {
        return false;
    }

    out.reserve(bytes);
    let mut fire_ioc = false;

    // BDPL is 128-byte aligned in hardware; low bits must read as 0.
    let bdl_base = ((sd.bdpu as u64) << 32) | (sd.bdpl as u64 & !0x7f);

    while bytes > 0 {
        let entry = read_bdl_entry(mem, bdl_base, *bdl_index as usize);
        if entry.len == 0 {
            break;
        }

        let remaining = entry.len.saturating_sub(*bdl_offset).min(bytes as u32) as usize;
        if remaining == 0 {
            // Move to next entry.
            *bdl_offset = 0;
            if *bdl_index >= sd.lvi {
                *bdl_index = 0;
            } else {
                *bdl_index += 1;
            }
            continue;
        }

        let start = out.len();
        out.resize(start + remaining, 0);
        if let Some(addr) = entry.addr.checked_add(*bdl_offset as u64) {
            mem.read_physical(addr, &mut out[start..start + remaining]);
        }

        *bdl_offset += remaining as u32;
        bytes -= remaining;

        // Update LPIB, wrapping at CBL if set.
        sd.lpib = sd.lpib.wrapping_add(remaining as u32);
        if sd.cbl != 0 && sd.lpib >= sd.cbl {
            sd.lpib %= sd.cbl;
        }

        if *bdl_offset >= entry.len {
            *bdl_offset = 0;
            if entry.ioc {
                // Latch BCIS regardless of IOCE (IOCE only controls interrupt generation).
                sd.ctl |= SD_STS_BCIS << 24;
                if (sd.ctl & SD_CTL_IOCE) != 0 {
                    fire_ioc = true;
                }
            }
            if *bdl_index >= sd.lvi {
                *bdl_index = 0;
            } else {
                *bdl_index += 1;
            }
        }
    }

    fire_ioc
}

fn dma_write_stream_bytes(
    mem: &mut dyn MemoryAccess,
    sd: &mut StreamDescriptor,
    bdl_index: &mut u16,
    bdl_offset: &mut u32,
    mut bytes: &[u8],
) -> (usize, bool) {
    if sd.cbl == 0 || bytes.is_empty() {
        return (0, false);
    }

    let mut written = 0usize;
    let mut fire_ioc = false;

    let bdl_base = ((sd.bdpu as u64) << 32) | (sd.bdpl as u64 & !0x7f);

    while !bytes.is_empty() {
        let entry = read_bdl_entry(mem, bdl_base, *bdl_index as usize);
        if entry.len == 0 {
            break;
        }

        let remaining = entry
            .len
            .saturating_sub(*bdl_offset)
            .min(bytes.len() as u32) as usize;
        if remaining == 0 {
            *bdl_offset = 0;
            if *bdl_index >= sd.lvi {
                *bdl_index = 0;
            } else {
                *bdl_index += 1;
            }
            continue;
        }

        if let Some(addr) = entry.addr.checked_add(*bdl_offset as u64) {
            mem.write_physical(addr, &bytes[..remaining]);
        }
        bytes = &bytes[remaining..];
        written += remaining;

        *bdl_offset += remaining as u32;

        sd.lpib = sd.lpib.wrapping_add(remaining as u32);
        if sd.cbl != 0 && sd.lpib >= sd.cbl {
            sd.lpib %= sd.cbl;
        }

        if *bdl_offset >= entry.len {
            *bdl_offset = 0;
            if entry.ioc {
                sd.ctl |= SD_STS_BCIS << 24;
                if (sd.ctl & SD_CTL_IOCE) != 0 {
                    fire_ioc = true;
                }
            }
            if *bdl_index >= sd.lvi {
                *bdl_index = 0;
            } else {
                *bdl_index += 1;
            }
        }
    }

    (written, fire_ioc)
}

fn supported_pcm_caps() -> u32 {
    // PCM Size, Rate capabilities (HDA spec). Advertise a minimal, common subset:
    // - 16-bit samples
    // - 44.1kHz and 48kHz
    (1 << 1) | (1 << 13) | (1 << 14)
}

fn mmio_read_sub_u32(value: u32, byte_offset: u64, size: usize) -> u64 {
    let shift = (byte_offset * 8) as u32;
    let mask = match size {
        1 => 0xffu64,
        2 => 0xffffu64,
        4 => 0xffff_ffffu64,
        _ => return 0,
    };
    ((value as u64) >> shift) & mask
}

fn mmio_write_sub_u32(orig: u32, byte_offset: u64, size: usize, value: u64) -> u32 {
    let shift = (byte_offset * 8) as u32;
    let mask = match size {
        1 => 0xffu32,
        2 => 0xffffu32,
        4 => 0xffff_ffffu32,
        _ => return orig,
    };
    let mask_shifted = mask << shift;
    (orig & !mask_shifted) | (((value as u32) & mask) << shift)
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
    power_state: u8,
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
                config_default: 0x0101_0010,
                power_state: 0,
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
                config_default: 0x01A1_0010,
                power_state: 0,
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
            0xB00..=0xBFF => self.get_amp_gain_mute(payload16),
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
            0x705 => {
                // SET_POWER_STATE
                self.output_pin.power_state = payload8 & 0x3;
                0
            }
            0xF05 => self.output_pin.power_state as u32,
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
            0x705 => {
                self.mic_pin.power_state = payload8 & 0x3;
                0
            }
            0xF05 => self.mic_pin.power_state as u32,
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
                (1u32 << 4) | (1u32 << 6) | (1u32 << 8)
            }
            0x0A => supported_pcm_caps(),
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

impl Default for HdaCodec {
    fn default() -> Self {
        Self::new()
    }
}

/// Minimal Intel HD Audio controller emulation.
#[derive(Debug, Clone)]
pub struct HdaController {
    gcap: u16,
    vmin: u8,
    vmaj: u8,
    gctl: u32,
    wakeen: u16,
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
    /// Host/output sample rate used when producing audio into the output sink.
    output_rate_hz: u32,
    /// Host/input sample rate used when consuming microphone samples for the capture stream.
    ///
    /// Defaults to `output_rate_hz`, but may be overridden if microphone capture uses a separate
    /// `AudioContext` (and therefore potentially a different `AudioContext.sampleRate`) than the
    /// output AudioWorklet graph.
    capture_sample_rate_hz: u32,
}

impl Default for HdaController {
    fn default() -> Self {
        Self::new()
    }
}

impl HdaController {
    pub fn new() -> Self {
        Self::new_with_output_rate(48_000)
    }

    pub fn new_with_output_rate(output_rate_hz: u32) -> Self {
        assert!(output_rate_hz > 0, "output_rate_hz must be non-zero");
        let num_output_streams: usize = 1;
        let num_input_streams: usize = 1;
        let num_bidir_streams: usize = 0;
        let num_streams = num_output_streams + num_input_streams + num_bidir_streams;
        let audio_ring_frames = (output_rate_hz as usize / 10).max(1); // ~100ms
        Self {
            // GCAP: OSS=1, ISS=1, BSS=0, NSDO=1.
            gcap: ((num_output_streams as u16) & 0xF)
                | (((num_input_streams as u16) & 0xF) << 4)
                | (((num_bidir_streams as u16) & 0xF) << 8)
                | (1u16 << 12),
            vmin: 0x00,
            vmaj: 0x01,
            gctl: 0,
            wakeen: 0,
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
            audio_out: AudioRingBuffer::new_stereo(audio_ring_frames),

            irq_pending: false,
            output_rate_hz,
            capture_sample_rate_hz: output_rate_hz,
        }
    }

    /// Return the host/output sample rate used by the controller when emitting audio.
    ///
    /// This is the "time base" for the `output_frames` argument passed to [`HdaController::process`]
    /// and [`HdaController::process_into`].
    pub fn output_rate_hz(&self) -> u32 {
        self.output_rate_hz
    }

    /// Return the host/input sample rate expected by the capture stream.
    ///
    /// This should typically match the microphone capture graph's sample rate (for example, the
    /// `AudioContext.sampleRate` used by the mic AudioWorklet/ScriptProcessor path).
    ///
    /// By default, the capture sample rate tracks [`Self::output_rate_hz`].
    pub fn capture_sample_rate_hz(&self) -> u32 {
        self.capture_sample_rate_hz
    }

    /// Set the host/output sample rate used by the controller when emitting audio.
    ///
    /// The controller will resample guest PCM streams to this rate before pushing into the
    /// output sink.
    pub fn set_output_rate_hz(&mut self, output_rate_hz: u32) {
        assert!(output_rate_hz > 0, "output_rate_hz must be non-zero");
        if self.output_rate_hz == output_rate_hz {
            return;
        }

        let prev = self.output_rate_hz;
        self.output_rate_hz = output_rate_hz;
        if self.capture_sample_rate_hz == prev {
            self.capture_sample_rate_hz = output_rate_hz;
        }

        // Reset stream resamplers (but keep DMA position tracking so changing the host rate doesn't
        // rewind guest playback/capture).
        //
        // Stream descriptor order follows the HDA spec: output streams, input streams, bidirectional streams.
        let oss = (self.gcap & 0xF) as usize;
        let iss = ((self.gcap >> 4) & 0xF) as usize;
        let capture_sample_rate_hz = self.capture_sample_rate_hz;
        for (idx, rt) in self.stream_rt.iter_mut().enumerate() {
            rt.capture_frame_accum = 0;
            rt.dma_scratch.clear();
            rt.decode_scratch.clear();
            rt.resample_out_scratch.clear();
            rt.capture_mono_scratch.clear();

            if idx < oss {
                // Playback: guest-rate -> host-rate.
                let src = rt.resampler.src_rate_hz();
                rt.resampler.reset_rates(src, output_rate_hz);
            } else if idx < oss + iss {
                // Capture: host-rate -> guest-rate.
                let dst = rt.resampler.dst_rate_hz();
                rt.resampler.reset_rates(capture_sample_rate_hz, dst);
            } else {
                // Bidir streams (unused): treat as playback for now.
                let src = rt.resampler.src_rate_hz();
                rt.resampler.reset_rates(src, output_rate_hz);
            }
        }

        self.audio_out = AudioRingBuffer::new_stereo((output_rate_hz / 10).max(1) as usize);
    }

    /// Set the host/input sample rate used when pulling microphone samples for the capture stream.
    ///
    /// This does not affect the output sample rate/time base used by [`Self::process`] and friends.
    pub fn set_capture_sample_rate_hz(&mut self, capture_sample_rate_hz: u32) {
        assert!(
            capture_sample_rate_hz > 0,
            "capture_sample_rate_hz must be non-zero"
        );
        if self.capture_sample_rate_hz == capture_sample_rate_hz {
            return;
        }
        self.capture_sample_rate_hz = capture_sample_rate_hz;

        // Reset capture stream resamplers (but keep DMA position tracking).
        let oss = (self.gcap & 0xF) as usize;
        let iss = ((self.gcap >> 4) & 0xF) as usize;
        for rt in self.stream_rt.iter_mut().skip(oss).take(iss) {
            rt.dma_scratch.clear();
            rt.decode_scratch.clear();
            rt.resample_out_scratch.clear();
            rt.capture_mono_scratch.clear();

            let dst = rt.resampler.dst_rate_hz();
            rt.resampler.reset_rates(capture_sample_rate_hz, dst);
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

    /// Returns the current asserted *level* of the controller's IRQ line.
    ///
    /// This is derived from the guest-visible interrupt control/status registers and therefore
    /// does **not** clear or otherwise mutate interrupt state. This is intended for level-triggered
    /// interrupt routing (PCI INTx): the line remains asserted while an enabled interrupt source is
    /// pending.
    pub fn irq_level(&self) -> bool {
        // Simplified: assert if global interrupt enable is set and any enabled interrupt source
        // is pending.
        if (self.intctl & INTCTL_GIE) == 0 {
            return false;
        }

        let pending_streams = self.intsts & 0x3fff_ffff;
        let enabled_streams = self.intctl & 0x3fff_ffff;
        let pending_controller = (self.intsts & INTSTS_CIS) != 0 && (self.intctl & (1 << 30)) != 0;

        (pending_streams & enabled_streams) != 0 || pending_controller
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

        self.process_output_stream(mem, 0, output_frames);
        let out_samples = &self.stream_rt[0].resample_out_scratch;
        if !out_samples.is_empty() {
            if let Some(ref mut sink) = sink {
                sink.push_interleaved_f32(out_samples);
            } else {
                self.audio_out.push_interleaved_stereo(out_samples);
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
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        let end = match offset.checked_add(size as u64) {
            Some(end) => end,
            None => return 0,
        };
        if end > HDA_MMIO_SIZE as u64 {
            return 0;
        }

        if end <= REG_VMAJ + 1 {
            let value = (self.gcap as u32)
                | ((self.vmin as u32) << (((REG_VMIN - REG_GCAP) * 8) as u32))
                | ((self.vmaj as u32) << (((REG_VMAJ - REG_GCAP) * 8) as u32));
            return mmio_read_sub_u32(value, offset, size);
        }
        if offset >= REG_GCTL && end <= REG_GCTL + 4 {
            return mmio_read_sub_u32(self.gctl, offset - REG_GCTL, size);
        }
        if offset >= REG_WAKEEN && end <= REG_STATESTS + 2 {
            let value = (self.wakeen as u32)
                | ((self.statests as u32) << (((REG_STATESTS - REG_WAKEEN) * 8) as u32));
            return mmio_read_sub_u32(value, offset - REG_WAKEEN, size);
        }
        if offset >= REG_INTCTL && end <= REG_INTCTL + 4 {
            return mmio_read_sub_u32(self.intctl, offset - REG_INTCTL, size);
        }
        if offset >= REG_INTSTS && end <= REG_INTSTS + 4 {
            return mmio_read_sub_u32(self.intsts, offset - REG_INTSTS, size);
        }

        if offset >= REG_CORBLBASE && end <= REG_CORBLBASE + 4 {
            return mmio_read_sub_u32(self.corblbase, offset - REG_CORBLBASE, size);
        }
        if offset >= REG_CORBUBASE && end <= REG_CORBUBASE + 4 {
            return mmio_read_sub_u32(self.corbubase, offset - REG_CORBUBASE, size);
        }
        if offset >= REG_CORBWP && end <= REG_CORBRP + 2 {
            let value = (self.corbwp as u32)
                | ((self.corbrp as u32) << (((REG_CORBRP - REG_CORBWP) * 8) as u32));
            return mmio_read_sub_u32(value, offset - REG_CORBWP, size);
        }
        if offset >= REG_CORBCTL && end <= REG_CORBSIZE + 2 {
            let value = (self.corbctl as u32)
                | ((self.corbsts as u32) << (((REG_CORBSTS - REG_CORBCTL) * 8) as u32))
                | ((self.corbsize as u32) << (((REG_CORBSIZE - REG_CORBCTL) * 8) as u32));
            return mmio_read_sub_u32(value, offset - REG_CORBCTL, size);
        }

        if offset >= REG_RIRBLBASE && end <= REG_RIRBLBASE + 4 {
            return mmio_read_sub_u32(self.rirblbase, offset - REG_RIRBLBASE, size);
        }
        if offset >= REG_RIRBUBASE && end <= REG_RIRBUBASE + 4 {
            return mmio_read_sub_u32(self.rirbubase, offset - REG_RIRBUBASE, size);
        }
        if offset >= REG_RIRBWP && end <= REG_RINTCNT + 2 {
            let value = (self.rirbwp as u32)
                | ((self.rintcnt as u32) << (((REG_RINTCNT - REG_RIRBWP) * 8) as u32));
            return mmio_read_sub_u32(value, offset - REG_RIRBWP, size);
        }
        if offset >= REG_RIRBCTL && end <= REG_RIRBSIZE + 2 {
            let value = (self.rirbctl as u32)
                | ((self.rirbsts as u32) << (((REG_RIRBSTS - REG_RIRBCTL) * 8) as u32))
                | ((self.rirbsize as u32) << (((REG_RIRBSIZE - REG_RIRBCTL) * 8) as u32));
            return mmio_read_sub_u32(value, offset - REG_RIRBCTL, size);
        }

        if offset >= REG_DPLBASE && end <= REG_DPLBASE + 4 {
            return mmio_read_sub_u32(self.dplbase, offset - REG_DPLBASE, size);
        }
        if offset >= REG_DPUBASE && end <= REG_DPUBASE + 4 {
            return mmio_read_sub_u32(self.dpubase, offset - REG_DPUBASE, size);
        }

        let sd_end = REG_SD_BASE + SD_STRIDE * self.streams.len() as u64;
        if offset >= REG_SD_BASE && offset < sd_end {
            let stream = ((offset - REG_SD_BASE) / SD_STRIDE) as usize;
            let reg = (offset - REG_SD_BASE) % SD_STRIDE;
            if reg + size as u64 > SD_STRIDE {
                return 0;
            }
            let sd = &self.streams[stream];

            if reg < SD_REG_LPIB {
                let start = reg - SD_REG_CTL;
                if start + size as u64 > 4 {
                    return 0;
                }
                return mmio_read_sub_u32(sd.ctl, start, size);
            }

            if reg >= SD_REG_LPIB && reg + size as u64 <= SD_REG_LPIB + 4 {
                return mmio_read_sub_u32(sd.lpib, reg - SD_REG_LPIB, size);
            }
            if reg >= SD_REG_CBL && reg + size as u64 <= SD_REG_CBL + 4 {
                return mmio_read_sub_u32(sd.cbl, reg - SD_REG_CBL, size);
            }
            if reg >= SD_REG_LVI && reg + size as u64 <= SD_REG_FIFOW + 2 {
                let value = (sd.lvi as u32)
                    | ((sd.fifow as u32) << (((SD_REG_FIFOW - SD_REG_LVI) * 8) as u32));
                return mmio_read_sub_u32(value, reg - SD_REG_LVI, size);
            }
            if reg >= SD_REG_FIFOS && reg + size as u64 <= SD_REG_FMT + 2 {
                let value = (sd.fifos as u32)
                    | ((sd.fmt as u32) << (((SD_REG_FMT - SD_REG_FIFOS) * 8) as u32));
                return mmio_read_sub_u32(value, reg - SD_REG_FIFOS, size);
            }
            if reg >= SD_REG_BDPL && reg + size as u64 <= SD_REG_BDPL + 4 {
                return mmio_read_sub_u32(sd.bdpl, reg - SD_REG_BDPL, size);
            }
            if reg >= SD_REG_BDPU && reg + size as u64 <= SD_REG_BDPU + 4 {
                return mmio_read_sub_u32(sd.bdpu, reg - SD_REG_BDPU, size);
            }
        }

        0
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let end = match offset.checked_add(size as u64) {
            Some(end) => end,
            None => return,
        };
        if end > HDA_MMIO_SIZE as u64 {
            return;
        }

        if offset >= REG_GCTL && end <= REG_GCTL + 4 {
            let prev = self.gctl;
            let new = mmio_write_sub_u32(prev, offset - REG_GCTL, size, value);
            self.gctl = new;
            let prev_crst = (prev & GCTL_CRST) != 0;
            let new_crst = (new & GCTL_CRST) != 0;
            if prev_crst && !new_crst {
                self.reset();
            } else if !prev_crst && new_crst {
                // Leaving reset: report codec 0 presence.
                self.statests |= 1;
            }
            return;
        }
        if offset >= REG_WAKEEN && end <= REG_STATESTS + 2 {
            for i in 0..size {
                let byte = ((value >> (i * 8)) & 0xff) as u8;
                let addr = offset + i as u64;
                match addr {
                    REG_WAKEEN => self.wakeen = (self.wakeen & !0x00ff) | byte as u16,
                    _ if addr == REG_WAKEEN + 1 => {
                        self.wakeen = (self.wakeen & !0xff00) | ((byte as u16) << 8)
                    }
                    REG_STATESTS => self.statests &= !(byte as u16),
                    _ if addr == REG_STATESTS + 1 => self.statests &= !((byte as u16) << 8),
                    _ => {}
                }
            }
            return;
        }
        if offset >= REG_INTCTL && end <= REG_INTCTL + 4 {
            self.intctl = mmio_write_sub_u32(self.intctl, offset - REG_INTCTL, size, value);
            self.update_irq_line();
            return;
        }
        if offset >= REG_INTSTS && end <= REG_INTSTS + 4 {
            let rel = offset - REG_INTSTS;
            let mut clear_mask = 0u32;
            for i in 0..size {
                let byte = ((value >> (i * 8)) & 0xff) as u32;
                clear_mask |= byte << (((rel + i as u64) * 8) as u32);
            }
            self.intsts &= !clear_mask;
            self.recalc_intsts_gis();
            self.update_irq_line();
            return;
        }

        if offset >= REG_CORBLBASE && end <= REG_CORBLBASE + 4 {
            self.corblbase =
                mmio_write_sub_u32(self.corblbase, offset - REG_CORBLBASE, size, value);
            return;
        }
        if offset >= REG_CORBUBASE && end <= REG_CORBUBASE + 4 {
            self.corbubase =
                mmio_write_sub_u32(self.corbubase, offset - REG_CORBUBASE, size, value);
            return;
        }
        if offset >= REG_CORBWP && end <= REG_CORBRP + 2 {
            let current = (self.corbwp as u32)
                | ((self.corbrp as u32) << (((REG_CORBRP - REG_CORBWP) * 8) as u32));
            let new = mmio_write_sub_u32(current, offset - REG_CORBWP, size, value);
            let new_wp = (new & 0xffff) as u16;
            let new_rp = (new >> 16) as u16;
            let mask = self.corb_ptr_mask();
            self.corbwp = (new_wp & 0x00ff) & mask;
            if (new_rp & 0x8000) != 0 {
                self.corbrp = 0;
            } else {
                self.corbrp = (new_rp & 0x00ff) & mask;
            }
            return;
        }
        if offset >= REG_CORBCTL && end <= REG_CORBSIZE + 2 {
            for i in 0..size {
                let byte = ((value >> (i * 8)) & 0xff) as u8;
                let addr = offset + i as u64;
                match addr {
                    REG_CORBCTL => self.corbctl = byte,
                    REG_CORBSTS => self.corbsts &= !byte,
                    REG_CORBSIZE => {
                        // Only the size selection bits (1:0) are writable; capability bits are RO.
                        self.corbsize = (self.corbsize & !0x3) | (byte & 0x3);
                    }
                    _ => {}
                }
            }
            let mask = self.corb_ptr_mask();
            self.corbwp &= mask;
            self.corbrp &= mask;
            return;
        }

        if offset >= REG_RIRBLBASE && end <= REG_RIRBLBASE + 4 {
            self.rirblbase =
                mmio_write_sub_u32(self.rirblbase, offset - REG_RIRBLBASE, size, value);
            return;
        }
        if offset >= REG_RIRBUBASE && end <= REG_RIRBUBASE + 4 {
            self.rirbubase =
                mmio_write_sub_u32(self.rirbubase, offset - REG_RIRBUBASE, size, value);
            return;
        }
        if offset >= REG_RIRBWP && end <= REG_RINTCNT + 2 {
            let current = (self.rirbwp as u32)
                | ((self.rintcnt as u32) << (((REG_RINTCNT - REG_RIRBWP) * 8) as u32));
            let new = mmio_write_sub_u32(current, offset - REG_RIRBWP, size, value);
            let new_wp = (new & 0xffff) as u16;
            let new_cnt = (new >> 16) as u16;
            if (new_wp & 0x8000) != 0 {
                self.rirbwp = 0;
            } else {
                self.rirbwp = (new_wp & 0x00ff) & self.rirb_ptr_mask();
            }
            self.rintcnt = new_cnt;
            return;
        }
        if offset >= REG_RIRBCTL && end <= REG_RIRBSIZE + 2 {
            for i in 0..size {
                let byte = ((value >> (i * 8)) & 0xff) as u8;
                let addr = offset + i as u64;
                match addr {
                    REG_RIRBCTL => self.rirbctl = byte,
                    REG_RIRBSTS => self.rirbsts &= !byte,
                    REG_RIRBSIZE => {
                        // Only the size selection bits (1:0) are writable; capability bits are RO.
                        self.rirbsize = (self.rirbsize & !0x3) | (byte & 0x3);
                    }
                    _ => {}
                }
            }
            self.rirbwp &= self.rirb_ptr_mask();
            return;
        }

        if offset >= REG_DPLBASE && end <= REG_DPLBASE + 4 {
            let v = mmio_write_sub_u32(self.dplbase, offset - REG_DPLBASE, size, value);
            // Bits 6:1 are reserved and must read as 0; the base is 128-byte aligned.
            self.dplbase = (v & DPLBASE_ENABLE) | (v & DPLBASE_ADDR_MASK);
            return;
        }
        if offset >= REG_DPUBASE && end <= REG_DPUBASE + 4 {
            self.dpubase = mmio_write_sub_u32(self.dpubase, offset - REG_DPUBASE, size, value);
            return;
        }

        let sd_end = REG_SD_BASE + SD_STRIDE * self.streams.len() as u64;
        if offset >= REG_SD_BASE && offset < sd_end {
            let stream = ((offset - REG_SD_BASE) / SD_STRIDE) as usize;
            let reg = (offset - REG_SD_BASE) % SD_STRIDE;
            if reg + size as u64 > SD_STRIDE {
                return;
            }

            if reg < SD_REG_LPIB {
                if reg == SD_REG_STS && size == 1 {
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

                // Only model writes fully contained in the SDnCTL bytes (0..2). Writes
                // touching the SDnSTS byte must use the 1-byte RW1C path above.
                let start = (reg - SD_REG_CTL) as usize;
                if start.saturating_add(size) > 3 {
                    return;
                }

                let (prev, now) = {
                    let sd = &mut self.streams[stream];
                    let prev = sd.ctl;
                    sd.ctl = mmio_write_sub_u32(sd.ctl, reg - SD_REG_CTL, size, value);
                    (prev, sd.ctl)
                };

                if (prev & SD_CTL_SRST) != 0 && (now & SD_CTL_SRST) == 0 {
                    self.reset_stream_engine(stream);
                }
                return;
            }

            let sd = &mut self.streams[stream];
            if reg >= SD_REG_LPIB && reg + size as u64 <= SD_REG_LPIB + 4 {
                // Read-only in hardware.
                return;
            }
            if reg >= SD_REG_CBL && reg + size as u64 <= SD_REG_CBL + 4 {
                sd.cbl = mmio_write_sub_u32(sd.cbl, reg - SD_REG_CBL, size, value);
                return;
            }
            if reg >= SD_REG_LVI && reg + size as u64 <= SD_REG_FIFOW + 2 {
                let current = (sd.lvi as u32)
                    | ((sd.fifow as u32) << (((SD_REG_FIFOW - SD_REG_LVI) * 8) as u32));
                let new = mmio_write_sub_u32(current, reg - SD_REG_LVI, size, value);
                // SDnLVI is 8 bits in the Intel HDA spec; upper bits are reserved.
                sd.lvi = (new & 0xff) as u16;
                sd.fifow = (new >> 16) as u16;

                // Keep the stream runtime's BDL cursor consistent with the guest-programmed
                // LVI. This avoids out-of-range BDL entry reads if the guest shrinks LVI while
                // a stream is running.
                let rt = &mut self.stream_rt[stream];
                if rt.bdl_index > sd.lvi {
                    rt.bdl_index = 0;
                    rt.bdl_offset = 0;
                }
                return;
            }
            if reg >= SD_REG_FIFOS && reg + size as u64 <= SD_REG_FMT + 2 {
                let current = (sd.fifos as u32)
                    | ((sd.fmt as u32) << (((SD_REG_FMT - SD_REG_FIFOS) * 8) as u32));
                let new = mmio_write_sub_u32(current, reg - SD_REG_FIFOS, size, value);
                sd.fifos = (new & 0xffff) as u16;
                sd.fmt = (new >> 16) as u16;
                return;
            }
            if reg >= SD_REG_BDPL && reg + size as u64 <= SD_REG_BDPL + 4 {
                sd.bdpl = mmio_write_sub_u32(sd.bdpl, reg - SD_REG_BDPL, size, value);
                return;
            }
            if reg >= SD_REG_BDPU && reg + size as u64 <= SD_REG_BDPU + 4 {
                sd.bdpu = mmio_write_sub_u32(sd.bdpu, reg - SD_REG_BDPU, size, value);
            }
        }
    }

    fn reset(&mut self) {
        self.gctl = 0;
        self.wakeen = 0;
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
            let Some(entry_addr) = base.checked_add((stream as u64) * 8) else {
                break;
            };
            mem.write_u32(entry_addr, sd.lpib);
            if let Some(hi_addr) = entry_addr.checked_add(4) {
                mem.write_u32(hi_addr, 0);
            }
        }
    }

    fn corb_entries(&self) -> u16 {
        match self.corbsize & 0x3 {
            0 => 2,
            1 => 16,
            _ => 256,
        }
    }

    fn corb_ptr_mask(&self) -> u16 {
        self.corb_entries().saturating_sub(1)
    }

    fn rirb_entries(&self) -> u16 {
        match self.rirbsize & 0x3 {
            0 => 2,
            1 => 16,
            _ => 256,
        }
    }

    fn rirb_ptr_mask(&self) -> u16 {
        self.rirb_entries().saturating_sub(1)
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
        let mask = self.corb_ptr_mask();

        // Keep pointers in range even if the guest (or snapshot restore) provides out-of-range
        // values for the currently-selected CORB size. Without this, the `while corbrp != corbwp`
        // loop below can spin forever (e.g. entries=2, corbwp=3).
        self.corbwp &= mask;
        self.corbrp &= mask;

        // Defensive bound: we should never process more entries than the ring can hold.
        let mut processed = 0u16;
        while self.corbrp != self.corbwp && processed < entries {
            processed += 1;
            self.corbrp = (self.corbrp + 1) & mask;
            let Some(addr) = corb_base.checked_add(self.corbrp as u64 * 4) else {
                break;
            };
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

        if let Some(addr) = self.rirb_base().checked_add(self.rirbwp as u64 * 8) {
            mem.write_u32(addr, resp);
            if let Some(hi_addr) = addr.checked_add(4) {
                mem.write_u32(hi_addr, cad as u32);
            }
        }

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
        self.irq_pending = self.irq_level();
    }

    fn process_output_stream(
        &mut self,
        mem: &mut dyn MemoryAccess,
        stream: usize,
        output_frames: usize,
    ) {
        self.stream_rt[stream].resample_out_scratch.clear();

        if (self.gctl & GCTL_CRST) == 0 {
            return;
        }
        if (self.streams[stream].ctl & (SD_CTL_SRST | SD_CTL_RUN)) != (SD_CTL_SRST | SD_CTL_RUN) {
            return;
        }

        let stream_num = ((self.streams[stream].ctl & SD_CTL_STRM_MASK) >> SD_CTL_STRM_SHIFT) as u8;
        if stream_num == 0 || stream_num != self.codec.output_stream_id() {
            return;
        }

        let fmt_raw = self.streams[stream].fmt;
        if fmt_raw == 0 {
            return;
        }

        let fmt = StreamFormat::from_hda_format(fmt_raw);
        let output_rate_hz = self.output_rate_hz;
        let [gain_l, gain_r] = self.codec.output_gain_scalars();

        let mut fire_ioc = false;
        {
            let sd = &mut self.streams[stream];
            let rt = &mut self.stream_rt[stream];
            let fmt_changed =
                rt.last_fmt_raw != fmt_raw || rt.resampler.src_rate_hz() != fmt.sample_rate_hz;
            let dst_changed = rt.resampler.dst_rate_hz() != output_rate_hz;
            if fmt_changed || dst_changed {
                rt.resampler.reset_rates(fmt.sample_rate_hz, output_rate_hz);
                if fmt_changed {
                    rt.last_fmt_raw = fmt_raw;
                    rt.bdl_index = 0;
                    rt.bdl_offset = 0;
                }
            }

            // Ensure the resampler has enough source frames queued to synthesize the requested output.
            let required_src = rt.resampler.required_source_frames(output_frames);
            let queued_src = rt.resampler.queued_source_frames();
            let need_src = required_src.saturating_sub(queued_src);

            if need_src > 0 {
                let bytes = need_src * fmt.bytes_per_frame();
                fire_ioc |= dma_read_stream_bytes(
                    mem,
                    sd,
                    &mut rt.bdl_index,
                    &mut rt.bdl_offset,
                    bytes,
                    &mut rt.dma_scratch,
                );
                decode_pcm_to_stereo_f32_into(&rt.dma_scratch, fmt, &mut rt.decode_scratch);
                if !rt.decode_scratch.is_empty() {
                    rt.resampler.push_source_frames(&rt.decode_scratch);
                }
            }

            rt.resampler
                .produce_interleaved_stereo_into(output_frames, &mut rt.resample_out_scratch);
            Self::apply_codec_output_controls(&mut rt.resample_out_scratch, gain_l, gain_r);
        }

        if fire_ioc {
            self.raise_stream_interrupt(stream);
        }
    }

    fn apply_codec_output_controls(samples: &mut [f32], gain_l: f32, gain_r: f32) {
        if samples.is_empty() {
            return;
        }
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
            sample.clamp(-1.0, 1.0)
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

        let ctl = self.streams[stream].ctl;
        if (ctl & (SD_CTL_SRST | SD_CTL_RUN)) != (SD_CTL_SRST | SD_CTL_RUN) {
            return;
        }

        let stream_num = ((ctl & SD_CTL_STRM_MASK) >> SD_CTL_STRM_SHIFT) as u8;
        if stream_num == 0 || stream_num != self.codec.input_stream_id() {
            return;
        }

        let fmt_raw = self.streams[stream].fmt;
        if fmt_raw == 0 {
            return;
        }

        let fmt = StreamFormat::from_hda_format(fmt_raw);

        let dst_frames = {
            let rt = &mut self.stream_rt[stream];
            let fmt_changed =
                rt.last_fmt_raw != fmt_raw || rt.resampler.dst_rate_hz() != fmt.sample_rate_hz;
            let src_changed = rt.resampler.src_rate_hz() != self.capture_sample_rate_hz;
            if fmt_changed || src_changed {
                rt.resampler
                    .reset_rates(self.capture_sample_rate_hz, fmt.sample_rate_hz);
                if fmt_changed {
                    rt.last_fmt_raw = fmt_raw;
                    rt.bdl_index = 0;
                    rt.bdl_offset = 0;
                    rt.capture_frame_accum = 0;
                }
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

        let fire_ioc;
        {
            let rt = &mut self.stream_rt[stream];

            let required_src = rt.resampler.required_source_frames(dst_frames);
            let queued_src = rt.resampler.queued_source_frames();
            let need_src = required_src.saturating_sub(queued_src);

            if need_src > 0 {
                rt.capture_mono_scratch.resize(need_src, 0.0);
                let got = capture.read_mono_f32(&mut rt.capture_mono_scratch);
                if got < need_src {
                    rt.capture_mono_scratch[got..].fill(0.0);
                }

                rt.decode_scratch.resize(need_src, [0.0; 2]);
                for (dst, &s) in rt.decode_scratch.iter_mut().zip(&rt.capture_mono_scratch) {
                    *dst = [s, s];
                }
                rt.resampler.push_source_frames(&rt.decode_scratch);
            }

            rt.resampler
                .produce_interleaved_stereo_into(dst_frames, &mut rt.resample_out_scratch);
            let produced_frames = rt.resample_out_scratch.len() / 2;
            if produced_frames == 0 {
                return;
            }

            rt.capture_mono_scratch.resize(produced_frames, 0.0);
            for i in 0..produced_frames {
                rt.capture_mono_scratch[i] = rt.resample_out_scratch[i * 2];
            }
            encode_mono_f32_to_pcm_into(&rt.capture_mono_scratch, fmt, &mut rt.dma_scratch);

            let sd = &mut self.streams[stream];
            let (_, ioc) = dma_write_stream_bytes(
                mem,
                sd,
                &mut rt.bdl_index,
                &mut rt.bdl_offset,
                &rt.dma_scratch,
            );
            fire_ioc = ioc;
        }

        if fire_ioc {
            self.raise_stream_interrupt(stream);
        }
    }
}

#[cfg(feature = "io-snapshot")]
impl HdaController {
    pub fn snapshot_state(&self, worklet_ring: AudioWorkletRingState) -> HdaControllerState {
        HdaControllerState {
            gctl: self.gctl,
            wakeen: self.wakeen,
            statests: self.statests,
            intctl: self.intctl,
            intsts: self.intsts,
            output_rate_hz: self.output_rate_hz,
            capture_sample_rate_hz: self.capture_sample_rate_hz,
            dplbase: self.dplbase,
            dpubase: self.dpubase,

            corblbase: self.corblbase,
            corbubase: self.corbubase,
            corbwp: self.corbwp,
            corbrp: self.corbrp,
            corbctl: self.corbctl,
            corbsts: self.corbsts,
            corbsize: self.corbsize,

            rirblbase: self.rirblbase,
            rirbubase: self.rirbubase,
            rirbwp: self.rirbwp,
            rirbctl: self.rirbctl,
            rirbsts: self.rirbsts,
            rirbsize: self.rirbsize,
            rintcnt: self.rintcnt,

            streams: self
                .streams
                .iter()
                .map(|sd| HdaStreamState {
                    ctl: sd.ctl,
                    lpib: sd.lpib,
                    cbl: sd.cbl,
                    lvi: sd.lvi,
                    fifow: sd.fifow,
                    fifos: sd.fifos,
                    fmt: sd.fmt,
                    bdpl: sd.bdpl,
                    bdpu: sd.bdpu,
                })
                .collect(),
            stream_runtime: self
                .stream_rt
                .iter()
                .map(|rt| HdaStreamRuntimeState {
                    bdl_index: rt.bdl_index,
                    bdl_offset: rt.bdl_offset,
                    last_fmt_raw: rt.last_fmt_raw,
                    resampler_src_pos_bits: rt.resampler.snapshot_src_pos_bits(),
                    resampler_queued_frames: rt.resampler.queued_source_frames() as u32,
                })
                .collect(),
            stream_capture_frame_accum: self
                .stream_rt
                .iter()
                .map(|rt| rt.capture_frame_accum)
                .collect(),
            codec: HdaCodecState {
                output_stream_id: self.codec.output.stream_id,
                output_channel: self.codec.output.channel,
                output_format: self.codec.output.format,
                amp_gain_left: self.codec.output.amp_gain_left,
                amp_gain_right: self.codec.output.amp_gain_right,
                amp_mute_left: self.codec.output.amp_mute_left,
                amp_mute_right: self.codec.output.amp_mute_right,
                pin_conn_select: self.codec.output_pin.conn_select,
                pin_ctl: self.codec.output_pin.pin_ctl,
                output_pin_power_state: self.codec.output_pin.power_state,
                afg_power_state: self.codec.afg_power_state,
            },
            codec_capture: HdaCodecCaptureState {
                input_stream_id: self.codec.input.stream_id,
                input_channel: self.codec.input.channel,
                input_format: self.codec.input.format,
                mic_pin_conn_select: self.codec.mic_pin.conn_select,
                mic_pin_ctl: self.codec.mic_pin.pin_ctl,
                mic_pin_power_state: self.codec.mic_pin.power_state,
            },
            worklet_ring,
        }
    }

    pub fn restore_state(&mut self, state: &HdaControllerState) {
        fn clamp_snapshot_rate_hz(rate_hz: u32) -> u32 {
            // Snapshot files may come from untrusted sources; clamp to a reasonable upper bound so
            // restore cannot allocate multi-gigabyte host buffers.
            const MAX_HOST_SAMPLE_RATE_HZ: u32 = 384_000;
            rate_hz.clamp(1, MAX_HOST_SAMPLE_RATE_HZ)
        }

        if state.output_rate_hz != 0 {
            self.set_output_rate_hz(clamp_snapshot_rate_hz(state.output_rate_hz));
        }
        if state.capture_sample_rate_hz != 0 {
            self.set_capture_sample_rate_hz(clamp_snapshot_rate_hz(state.capture_sample_rate_hz));
        }

        self.gctl = state.gctl;
        self.wakeen = state.wakeen;
        self.statests = state.statests;
        self.intctl = state.intctl;
        self.intsts = state.intsts;

        // Bits 6:1 are reserved and must read as 0; the base is 128-byte aligned.
        self.dplbase = (state.dplbase & DPLBASE_ENABLE) | (state.dplbase & DPLBASE_ADDR_MASK);
        self.dpubase = state.dpubase;

        self.corblbase = state.corblbase;
        self.corbubase = state.corbubase;
        self.corbwp = state.corbwp;
        self.corbrp = state.corbrp;
        self.corbctl = state.corbctl;
        self.corbsts = state.corbsts;
        self.corbsize = state.corbsize;
        let corb_mask = self.corb_ptr_mask();
        self.corbwp &= corb_mask;
        self.corbrp &= corb_mask;

        self.rirblbase = state.rirblbase;
        self.rirbubase = state.rirbubase;
        self.rirbwp = state.rirbwp;
        self.rintcnt = state.rintcnt;
        self.rirbctl = state.rirbctl;
        self.rirbsts = state.rirbsts;
        self.rirbsize = state.rirbsize;
        self.rirbwp &= self.rirb_ptr_mask();

        for (sd, s) in self.streams.iter_mut().zip(&state.streams) {
            sd.ctl = s.ctl;
            sd.lpib = s.lpib;
            sd.cbl = s.cbl;
            // SDnLVI is 8 bits in the Intel HDA spec; upper bits are reserved.
            sd.lvi = s.lvi & 0xff;
            sd.fifow = s.fifow;
            sd.fifos = s.fifos;
            sd.fmt = s.fmt;
            sd.bdpl = s.bdpl;
            sd.bdpu = s.bdpu;
        }

        let num_output_streams = (self.gcap & 0x0f) as usize;
        let num_input_streams = ((self.gcap >> 4) & 0x0f) as usize;

        for (idx, (rt, s)) in self
            .stream_rt
            .iter_mut()
            .zip(&state.stream_runtime)
            .enumerate()
        {
            let lvi = self.streams.get(idx).map(|sd| sd.lvi).unwrap_or(0);
            if s.bdl_index <= lvi {
                rt.bdl_index = s.bdl_index;
                rt.bdl_offset = s.bdl_offset;
            } else {
                // Snapshot may be corrupted/untrusted; clamp invalid BDL indices to avoid reading
                // descriptor entries outside the guest's programmed list.
                rt.bdl_index = 0;
                rt.bdl_offset = 0;
            }
            rt.last_fmt_raw = s.last_fmt_raw;

            let fmt_raw = if s.last_fmt_raw != 0 {
                s.last_fmt_raw
            } else {
                self.streams.get(idx).map(|sd| sd.fmt).unwrap_or(0)
            };
            let src_rate_hz = if fmt_raw != 0 {
                StreamFormat::from_hda_format(fmt_raw).sample_rate_hz
            } else {
                self.output_rate_hz
            };

            let is_capture_stream =
                idx >= num_output_streams && idx < num_output_streams + num_input_streams;
            let (resampler_src_rate, resampler_dst_rate) = if is_capture_stream {
                (self.capture_sample_rate_hz, src_rate_hz)
            } else {
                (src_rate_hz, self.output_rate_hz)
            };

            rt.resampler.restore_snapshot_state(
                resampler_src_rate,
                resampler_dst_rate,
                s.resampler_src_pos_bits,
                s.resampler_queued_frames,
            );
        }

        self.codec.output.stream_id = state.codec.output_stream_id;
        self.codec.output.channel = state.codec.output_channel;
        self.codec.output.format = state.codec.output_format;
        self.codec.output.amp_gain_left = state.codec.amp_gain_left;
        self.codec.output.amp_gain_right = state.codec.amp_gain_right;
        self.codec.output.amp_mute_left = state.codec.amp_mute_left;
        self.codec.output.amp_mute_right = state.codec.amp_mute_right;
        self.codec.output_pin.conn_select = state.codec.pin_conn_select;
        self.codec.output_pin.pin_ctl = state.codec.pin_ctl;
        self.codec.output_pin.power_state = state.codec.output_pin_power_state;
        self.codec.afg_power_state = state.codec.afg_power_state;
        self.codec.input.stream_id = state.codec_capture.input_stream_id;
        self.codec.input.channel = state.codec_capture.input_channel;
        self.codec.input.format = state.codec_capture.input_format;
        self.codec.mic_pin.conn_select = state.codec_capture.mic_pin_conn_select;
        self.codec.mic_pin.pin_ctl = state.codec_capture.mic_pin_ctl;
        self.codec.mic_pin.power_state = state.codec_capture.mic_pin_power_state;

        for (rt, v) in self
            .stream_rt
            .iter_mut()
            .zip(&state.stream_capture_frame_accum)
        {
            // `capture_frame_accum` is a fractional remainder accumulator and should always be
            // `< output_rate_hz`. Clamp corrupted/untrusted snapshot values to avoid creating an
            // enormous `dst_frames` count in `process_capture_stream`.
            rt.capture_frame_accum = v % self.output_rate_hz as u64;
        }

        // Host-side output buffering is recreated on restore.
        self.audio_out.clear();

        // Derive IRQ line state from restored registers.
        self.update_irq_line();
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
        let verb_id = (group << 8) | (payload16 >> 8);
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
        assert_eq!(
            codec.execute_verb(1, verb_12(0xF00, 0x04)),
            (2u32 << 16) | 4u32
        );

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
        hda.mmio_write(REG_CORBLBASE, 4, corb_base);
        hda.mmio_write(REG_RIRBLBASE, 4, rirb_base);

        // Set pointers so first command/response lands at entry 0.
        hda.mmio_write(REG_CORBRP, 2, 0x00ff);
        hda.mmio_write(REG_RIRBWP, 2, 0x00ff);

        // Enable response interrupts (CIS) and global interrupt.
        hda.mmio_write(REG_INTCTL, 4, (INTCTL_GIE | (1 << 30)) as u64);
        hda.mmio_write(REG_RIRBCTL, 1, (RIRBCTL_RUN | RIRBCTL_RINTCTL) as u64);
        hda.mmio_write(REG_CORBCTL, 1, CORBCTL_RUN as u64);

        // Queue one verb: root GET_PARAMETER vendor id.
        let verb = verb_12(0xF00, 0x00);
        mem.write_u32(corb_base, cmd(0, 0, verb));
        hda.mmio_write(REG_CORBWP, 2, 0x0000);

        hda.process(&mut mem, 0);

        let resp = mem.read_u32(rirb_base);
        assert_eq!(resp, 0x1af4_1620);
        assert!(hda.take_irq());
        assert_ne!(hda.mmio_read(REG_INTSTS, 4) as u32 & INTSTS_CIS, 0);
    }

    #[test]
    fn process_reuses_stream_scratch_buffers() {
        let mut hda = HdaController::new();
        let mut mem = GuestMemory::new(0x10_000);

        hda.mmio_write(REG_GCTL, 4, GCTL_CRST as u64);

        // Configure the codec to use stream ID 1 for output and stream ID 2 for capture.
        hda.codec_mut().execute_verb(2, verb_12(0x706, 0x10));
        hda.codec_mut().execute_verb(4, verb_12(0x706, 0x20));

        let out_bdl_base = 0x1000u64;
        let out_buf_base = 0x3000u64;
        let out_buf_len = 0x2000u32;
        mem.write_u64(out_bdl_base, out_buf_base);
        mem.write_u32(out_bdl_base + 8, out_buf_len);
        mem.write_u32(out_bdl_base + 12, 0);

        {
            let sd = hda.stream_mut(0);
            sd.ctl = SD_CTL_SRST | SD_CTL_RUN | ((1u32) << SD_CTL_STRM_SHIFT);
            sd.cbl = out_buf_len;
            sd.lvi = 0;
            sd.fmt = 0x0011; // 48kHz, 16-bit, stereo
            sd.bdpl = out_bdl_base as u32;
            sd.bdpu = 0;
        }

        let in_bdl_base = 0x2000u64;
        let in_buf_base = 0x5000u64;
        let in_buf_len = 0x2000u32;
        mem.write_u64(in_bdl_base, in_buf_base);
        mem.write_u32(in_bdl_base + 8, in_buf_len);
        mem.write_u32(in_bdl_base + 12, 0);

        {
            let sd = hda.stream_mut(1);
            sd.ctl = SD_CTL_SRST | SD_CTL_RUN | ((2u32) << SD_CTL_STRM_SHIFT);
            sd.cbl = in_buf_len;
            sd.lvi = 0;
            sd.fmt = 0x0010; // 48kHz, 16-bit, mono
            sd.bdpl = in_bdl_base as u32;
            sd.bdpu = 0;
        }

        let frames = 480usize; // 10ms at 48kHz.

        // Warmup: allow the scratch buffers to grow to their steady-state capacities.
        for _ in 0..10 {
            hda.process(&mut mem, frames);
        }

        let caps_out = (
            hda.stream_rt[0].dma_scratch.capacity(),
            hda.stream_rt[0].decode_scratch.capacity(),
            hda.stream_rt[0].resample_out_scratch.capacity(),
        );
        let caps_in = (
            hda.stream_rt[1].dma_scratch.capacity(),
            hda.stream_rt[1].decode_scratch.capacity(),
            hda.stream_rt[1].resample_out_scratch.capacity(),
            hda.stream_rt[1].capture_mono_scratch.capacity(),
        );

        for _ in 0..1000 {
            hda.process(&mut mem, frames);
            assert_eq!(hda.stream_rt[0].dma_scratch.capacity(), caps_out.0);
            assert_eq!(hda.stream_rt[0].decode_scratch.capacity(), caps_out.1);
            assert_eq!(hda.stream_rt[0].resample_out_scratch.capacity(), caps_out.2);

            assert_eq!(hda.stream_rt[1].dma_scratch.capacity(), caps_in.0);
            assert_eq!(hda.stream_rt[1].decode_scratch.capacity(), caps_in.1);
            assert_eq!(hda.stream_rt[1].resample_out_scratch.capacity(), caps_in.2);
            assert_eq!(hda.stream_rt[1].capture_mono_scratch.capacity(), caps_in.3);
        }
    }
}
