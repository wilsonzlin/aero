use aero_audio::pcm::LinearResampler;

use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_EVENT_IDX, VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};

pub const VIRTIO_DEVICE_TYPE_SND: u16 = 25;

pub const VIRTIO_SND_QUEUE_CONTROL: u16 = 0;
pub const VIRTIO_SND_QUEUE_EVENT: u16 = 1;
pub const VIRTIO_SND_QUEUE_TX: u16 = 2;
pub const VIRTIO_SND_QUEUE_RX: u16 = 3;

/// Control opcode: set the PCM input format used for subsequent TX buffers.
pub const VIRTIO_SND_CTRL_SET_FORMAT: u32 = 1;
/// Control opcode: reset the PCM input format back to defaults.
pub const VIRTIO_SND_CTRL_RESET: u32 = 2;

/// A minimal audio sink interface for virtio-snd output.
///
/// The browser host side is expected to push interleaved stereo `f32` samples
/// into a ring buffer that is consumed by an `AudioWorkletProcessor`.
pub trait SndOutput {
    fn push_interleaved_stereo_f32(&mut self, samples: &[f32]);
}

impl SndOutput for aero_audio::ring::AudioRingBuffer {
    fn push_interleaved_stereo_f32(&mut self, samples: &[f32]) {
        aero_audio::ring::AudioRingBuffer::push_interleaved_stereo(self, samples);
    }
}

#[derive(Debug, Clone, Copy)]
struct PcmFormat {
    sample_rate_hz: u32,
    channels: u8,
    bits_per_sample: u8,
}

impl Default for PcmFormat {
    fn default() -> Self {
        Self {
            sample_rate_hz: 48_000,
            channels: 2,
            bits_per_sample: 16,
        }
    }
}

impl PcmFormat {
    fn bytes_per_sample(self) -> Option<usize> {
        match self.bits_per_sample {
            8 => Some(1),
            16 => Some(2),
            20 | 24 | 32 => Some(4),
            _ => None,
        }
    }

    fn bytes_per_frame(self) -> Option<usize> {
        let bps = self.bytes_per_sample()?;
        Some(bps * self.channels as usize)
    }
}

/// Minimal virtio-snd device model.
///
/// The full virtio-snd specification is extensive; this device implements a
/// small subset aimed at custom guest drivers:
///
/// - Queue 0 (control): `VIRTIO_SND_CTRL_SET_FORMAT` and `VIRTIO_SND_CTRL_RESET`
/// - Queue 2 (tx): raw interleaved PCM frames with the configured format
///
/// The device converts input PCM into interleaved stereo `f32` at
/// `output_sample_rate_hz` and forwards it to the provided [`SndOutput`].
pub struct VirtioSnd<O: SndOutput> {
    output: O,
    output_sample_rate_hz: u32,

    negotiated_features: u64,
    input_format: PcmFormat,
    resampler: LinearResampler,

    pcm_scratch: Vec<u8>,
    decode_frames: Vec<[f32; 2]>,
    interleaved_scratch: Vec<f32>,
}

impl<O: SndOutput> VirtioSnd<O> {
    pub fn new(output: O, output_sample_rate_hz: u32) -> Self {
        let input_format = PcmFormat::default();
        Self {
            output,
            output_sample_rate_hz,
            negotiated_features: 0,
            input_format,
            resampler: LinearResampler::new(input_format.sample_rate_hz, output_sample_rate_hz),
            pcm_scratch: Vec::new(),
            decode_frames: Vec::new(),
            interleaved_scratch: Vec::new(),
        }
    }

    pub fn output_mut(&mut self) -> &mut O {
        &mut self.output
    }

    fn write_status_u32(
        mem: &mut dyn GuestMemory,
        chain: &DescriptorChain,
        status: u32,
    ) -> Result<u32, VirtioDeviceError> {
        let bytes = status.to_le_bytes();
        for d in chain.descriptors() {
            if !d.is_write_only() || d.len < 4 {
                continue;
            }
            let dst = mem
                .get_slice_mut(d.addr, 4)
                .map_err(|_| VirtioDeviceError::IoError)?;
            dst.copy_from_slice(&bytes);
            return Ok(4);
        }
        Ok(0)
    }

    fn write_status_u8(
        mem: &mut dyn GuestMemory,
        chain: &DescriptorChain,
        status: u8,
    ) -> Result<u32, VirtioDeviceError> {
        for d in chain.descriptors() {
            if !d.is_write_only() || d.len == 0 {
                continue;
            }
            let dst = mem
                .get_slice_mut(d.addr, 1)
                .map_err(|_| VirtioDeviceError::IoError)?;
            dst[0] = status;
            return Ok(1);
        }
        Ok(0)
    }

    fn read_ctrl_request(
        mem: &dyn GuestMemory,
        chain: &DescriptorChain,
    ) -> Result<[u8; 16], VirtioDeviceError> {
        let mut out = [0u8; 16];
        let mut written = 0usize;
        for d in chain.descriptors() {
            if d.is_write_only() {
                continue;
            }
            let slice = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;
            let take = (out.len() - written).min(slice.len());
            out[written..written + take].copy_from_slice(&slice[..take]);
            written += take;
            if written == out.len() {
                return Ok(out);
            }
        }
        Err(VirtioDeviceError::BadDescriptorChain)
    }

    fn process_control(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let req = Self::read_ctrl_request(mem, &chain)?;
        let opcode = u32::from_le_bytes(req[0..4].try_into().unwrap());
        let a = u32::from_le_bytes(req[4..8].try_into().unwrap());
        let b = u32::from_le_bytes(req[8..12].try_into().unwrap());
        let c = u32::from_le_bytes(req[12..16].try_into().unwrap());

        let mut status = 0u32;
        match opcode {
            VIRTIO_SND_CTRL_SET_FORMAT => {
                let fmt = PcmFormat {
                    sample_rate_hz: a,
                    channels: b as u8,
                    bits_per_sample: c as u8,
                };
                if fmt.sample_rate_hz == 0 || fmt.channels == 0 || fmt.bytes_per_frame().is_none() {
                    status = 1;
                } else {
                    self.input_format = fmt;
                    self.resampler
                        .reset_rates(fmt.sample_rate_hz, self.output_sample_rate_hz);
                }
            }
            VIRTIO_SND_CTRL_RESET => {
                self.input_format = PcmFormat::default();
                self.resampler
                    .reset_rates(self.input_format.sample_rate_hz, self.output_sample_rate_hz);
            }
            _ => status = 1,
        }

        let written = Self::write_status_u32(mem, &chain, status)?;
        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn process_tx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let Some(bytes_per_frame) = self.input_format.bytes_per_frame() else {
            let written = Self::write_status_u8(mem, &chain, 1)?;
            return queue
                .add_used(mem, chain.head_index(), written)
                .map_err(|_| VirtioDeviceError::IoError);
        };

        self.pcm_scratch.clear();
        let mut total_len = 0usize;
        for d in chain.descriptors() {
            if d.is_write_only() {
                continue;
            }
            total_len = total_len.saturating_add(d.len as usize);
        }
        self.pcm_scratch.reserve(total_len);

        for d in chain.descriptors() {
            if d.is_write_only() {
                continue;
            }
            let slice = mem
                .get_slice(d.addr, d.len as usize)
                .map_err(|_| VirtioDeviceError::IoError)?;
            self.pcm_scratch.extend_from_slice(slice);
        }

        let frames = self.pcm_scratch.len() / bytes_per_frame;
        if frames != 0 {
            self.decode_frames.clear();
            self.decode_frames.reserve(frames);
            decode_pcm_to_stereo_frames(&self.pcm_scratch, self.input_format, &mut self.decode_frames);

            if self.input_format.sample_rate_hz == self.output_sample_rate_hz {
                self.interleaved_scratch.clear();
                self.interleaved_scratch.reserve(self.decode_frames.len() * 2);
                for f in &self.decode_frames {
                    self.interleaved_scratch.push(f[0]);
                    self.interleaved_scratch.push(f[1]);
                }
                self.output
                    .push_interleaved_stereo_f32(&self.interleaved_scratch);
            } else {
                // Resample using the stateful linear resampler.
                if self.resampler.src_rate_hz() != self.input_format.sample_rate_hz
                    || self.resampler.dst_rate_hz() != self.output_sample_rate_hz
                {
                    self.resampler
                        .reset_rates(self.input_format.sample_rate_hz, self.output_sample_rate_hz);
                }
                self.resampler.push_source_frames(&self.decode_frames);
                let target = ((self.decode_frames.len() as u64)
                    .saturating_mul(self.output_sample_rate_hz as u64)
                    / self.input_format.sample_rate_hz as u64)
                    .saturating_add(2) as usize;
                let out = self.resampler.produce_interleaved_stereo(target);
                self.output.push_interleaved_stereo_f32(&out);
            }
        }

        let written = Self::write_status_u8(mem, &chain, 0)?;
        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }
}

impl<O: SndOutput + 'static> VirtioDevice for VirtioSnd<O> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_SND
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_RING_EVENT_IDX
    }

    fn set_features(&mut self, features: u64) {
        self.negotiated_features = features;
    }

    fn num_queues(&self) -> u16 {
        // controlq + eventq + txq + rxq
        4
    }

    fn queue_max_size(&self, queue: u16) -> u16 {
        match queue {
            VIRTIO_SND_QUEUE_CONTROL | VIRTIO_SND_QUEUE_EVENT => 64,
            _ => 256,
        }
    }

    fn process_queue(
        &mut self,
        queue_index: u16,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let _ = self.negotiated_features;
        match queue_index {
            VIRTIO_SND_QUEUE_CONTROL => self.process_control(chain, queue, mem),
            VIRTIO_SND_QUEUE_TX => self.process_tx(chain, queue, mem),
            _ => {
                // Best-effort: complete but do nothing.
                queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)
            }
        }
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // Custom minimal config:
        // 0x00 u32 output_sample_rate_hz
        // 0x04 u8  output_channels (always 2)
        // 0x05 u8  reserved
        // 0x06 u16 reserved
        // 0x08 u32 current_input_sample_rate_hz
        // 0x0c u32 current_input_channels/bits (low8=channels, high8=bits)
        let mut cfg = [0u8; 16];
        cfg[0..4].copy_from_slice(&self.output_sample_rate_hz.to_le_bytes());
        cfg[4] = 2;
        cfg[8..12].copy_from_slice(&self.input_format.sample_rate_hz.to_le_bytes());
        let packed: u32 = (self.input_format.channels as u32) | ((self.input_format.bits_per_sample as u32) << 8);
        cfg[12..16].copy_from_slice(&packed.to_le_bytes());

        let start = offset as usize;
        if start >= cfg.len() {
            data.fill(0);
            return;
        }
        let end = (start + data.len()).min(cfg.len());
        data[..end - start].copy_from_slice(&cfg[start..end]);
        if end - start < data.len() {
            data[end - start..].fill(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn reset(&mut self) {
        self.negotiated_features = 0;
        self.input_format = PcmFormat::default();
        self.resampler
            .reset_rates(self.input_format.sample_rate_hz, self.output_sample_rate_hz);
        self.pcm_scratch.clear();
        self.decode_frames.clear();
        self.interleaved_scratch.clear();
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

fn decode_pcm_to_stereo_frames(input: &[u8], fmt: PcmFormat, out: &mut Vec<[f32; 2]>) {
    let Some(bytes_per_frame) = fmt.bytes_per_frame() else {
        return;
    };
    let frames = input.len() / bytes_per_frame;
    out.clear();
    out.reserve(frames);

    for frame in 0..frames {
        let frame_off = frame * bytes_per_frame;
        let l = decode_sample_at(input, frame_off, fmt, 0);
        let r = if fmt.channels == 1 {
            l
        } else if fmt.channels >= 2 {
            decode_sample_at(input, frame_off, fmt, 1)
        } else {
            0.0
        };
        out.push([l, r]);
    }
}

fn decode_sample_at(input: &[u8], frame_off: usize, fmt: PcmFormat, channel: u8) -> f32 {
    let Some(bps) = fmt.bytes_per_sample() else {
        return 0.0;
    };
    let chan = channel as usize;
    let off = frame_off + chan * bps;
    if off + bps > input.len() {
        return 0.0;
    }
    decode_one_sample(&input[off..off + bps], fmt.bits_per_sample)
}

fn decode_one_sample(bytes: &[u8], bits_per_sample: u8) -> f32 {
    match bits_per_sample {
        8 => (bytes[0] as f32 - 128.0) / 128.0,
        16 => {
            let v = i16::from_le_bytes([bytes[0], bytes[1]]) as f32;
            v / 32768.0
        }
        20 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let v = (raw << 12) >> 12;
            v as f32 / 524_288.0
        }
        24 => {
            let raw = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let v = (raw << 8) >> 8;
            v as f32 / 8_388_608.0
        }
        32 => {
            let v = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32;
            v / 2_147_483_648.0
        }
        _ => 0.0,
    }
}

