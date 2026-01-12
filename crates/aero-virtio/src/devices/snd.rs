use aero_audio::pcm::LinearResampler;
use aero_audio::sink::AudioSink;

pub use aero_audio::capture::AudioCaptureSource;
pub use aero_audio::capture::SilenceCaptureSource as NullCaptureSource;

use aero_io_snapshot::io::audio::state as io_state;

use crate::devices::{VirtioDevice, VirtioDeviceError};
use crate::memory::GuestMemory;
use crate::pci::{VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1};
use crate::queue::{DescriptorChain, VirtQueue};

use std::collections::VecDeque;

pub const VIRTIO_DEVICE_TYPE_SND: u16 = 25;

pub const VIRTIO_SND_QUEUE_CONTROL: u16 = 0;
pub const VIRTIO_SND_QUEUE_EVENT: u16 = 1;
pub const VIRTIO_SND_QUEUE_TX: u16 = 2;
pub const VIRTIO_SND_QUEUE_RX: u16 = 3;

pub const VIRTIO_SND_R_JACK_INFO: u32 = 0x0001;
pub const VIRTIO_SND_R_JACK_REMAP: u32 = 0x0002;
pub const VIRTIO_SND_R_PCM_INFO: u32 = 0x0100;
pub const VIRTIO_SND_R_PCM_SET_PARAMS: u32 = 0x0101;
pub const VIRTIO_SND_R_PCM_PREPARE: u32 = 0x0102;
pub const VIRTIO_SND_R_PCM_RELEASE: u32 = 0x0103;
pub const VIRTIO_SND_R_PCM_START: u32 = 0x0104;
pub const VIRTIO_SND_R_PCM_STOP: u32 = 0x0105;
pub const VIRTIO_SND_R_CHMAP_INFO: u32 = 0x0200;

pub const VIRTIO_SND_S_OK: u32 = 0x0000;
pub const VIRTIO_SND_S_BAD_MSG: u32 = 0x0001;
pub const VIRTIO_SND_S_NOT_SUPP: u32 = 0x0002;
pub const VIRTIO_SND_S_IO_ERR: u32 = 0x0003;

pub const VIRTIO_SND_D_OUTPUT: u8 = 0x00;
pub const VIRTIO_SND_D_INPUT: u8 = 0x01;

pub const VIRTIO_SND_PCM_FMT_S16: u8 = 0x05;
pub const VIRTIO_SND_PCM_RATE_48000: u8 = 0x07;

pub const VIRTIO_SND_PCM_FMT_MASK_S16: u64 = 1u64 << VIRTIO_SND_PCM_FMT_S16;
pub const VIRTIO_SND_PCM_RATE_MASK_48000: u64 = 1u64 << VIRTIO_SND_PCM_RATE_48000;

pub const PLAYBACK_STREAM_ID: u32 = 0;
pub const CAPTURE_STREAM_ID: u32 = 1;

/// Sample rate used by the (minimal) virtio-snd guest contract implemented by this device.
///
/// The TX and RX PCM payloads are fixed at 48kHz S16_LE in the guest-facing ABI.
pub const PCM_SAMPLE_RATE_HZ: u32 = 48_000;

/// Maximum PCM payload size accepted in a single TX/RX descriptor chain.
///
/// This device is guest-driven and must treat descriptor lengths as untrusted. A malicious guest
/// could otherwise force the host to allocate unbounded scratch buffers when decoding/resampling.
///
/// 256KiB is ~1.3s of stereo S16_LE at 48kHz (and ~2.6s of mono capture), which is plenty for the
/// minimal Win7 contract while still bounding worst-case allocations.
const MAX_PCM_XFER_BYTES: u64 = 256 * 1024;

/// Defensive upper bound for host-provided sample rates.
///
/// Host integrations (including the browser/WASM runtime) may provide the output/capture sample
/// rates used for resampling. Snapshot files may also come from untrusted sources. Clamp rates to a
/// reasonable maximum to avoid allocating multi-gigabyte scratch buffers if a caller passes an
/// absurd value.
///
/// Keep this consistent with the equivalent clamp in the WASM virtio-snd bridge.
const MAX_HOST_SAMPLE_RATE_HZ: u32 = 384_000;

fn clamp_host_sample_rate_hz(rate_hz: u32) -> u32 {
    rate_hz.clamp(1, MAX_HOST_SAMPLE_RATE_HZ)
}

const PLAYBACK_CHANNELS: u8 = 2;
/// Capture stream channel count.
///
/// We expose a single channel because the Web mic capture path currently yields
/// mono f32 samples.
const CAPTURE_CHANNELS: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureTelemetry {
    pub dropped_samples: u64,
    pub underrun_samples: u64,
    pub underrun_responses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PcmParams {
    buffer_bytes: u32,
    period_bytes: u32,
    channels: u8,
    format: u8,
    rate: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamState {
    Idle,
    ParamsSet,
    Prepared,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PcmStream {
    params: Option<PcmParams>,
    state: StreamState,
}

/// Minimal virtio-snd device model.
///
/// The full virtio-snd specification is extensive. This device implements a
/// tiny subset intended for a custom guest driver:
///
/// - One playback PCM stream (stereo, 48kHz, signed 16-bit LE).
/// - One capture PCM stream (mono, 48kHz, signed 16-bit LE).
/// - Control queue supports `PCM_INFO`, `PCM_SET_PARAMS`, `PCM_PREPARE`,
///   `PCM_START`, `PCM_STOP`, `PCM_RELEASE`.
/// - TX queue accepts `stream_id` + PCM payload and writes interleaved stereo
///   `f32` samples into an [`AudioSink`] (typically an AudioWorklet ring buffer).
/// - RX queue accepts `stream_id` and returns captured PCM into guest-provided
///   buffers.
pub struct VirtioSnd<O: AudioSink, I: AudioCaptureSource = NullCaptureSource> {
    output: O,
    capture_source: I,
    negotiated_features: u64,
    playback: PcmStream,
    capture: PcmStream,
    capture_telemetry: CaptureTelemetry,
    /// Host/output sample rate for the playback/TX path (guest 48kHz -> host/output rate).
    host_sample_rate_hz: u32,
    /// Host/input sample rate for the capture/RX path (host/input rate -> guest 48kHz).
    ///
    /// Defaults to `host_sample_rate_hz`, but may be overridden if the microphone capture graph
    /// runs at a different sample rate than the output `AudioContext`.
    capture_sample_rate_hz: u32,
    /// Resampler for the playback/TX path (guest 48kHz -> host/output rate).
    resampler: LinearResampler,
    decoded_frames_scratch: Vec<[f32; 2]>,
    resampled_scratch: Vec<f32>,
    /// Resampler for the capture/RX path (host/input rate -> guest 48kHz).
    capture_resampler: LinearResampler,
    capture_frames_scratch: Vec<[f32; 2]>,
    capture_interleaved_scratch: Vec<f32>,
    capture_samples_scratch: Vec<f32>,
    event_buffers: VecDeque<DescriptorChain>,
    pending_events: VecDeque<Vec<u8>>,
}

impl<O: AudioSink> VirtioSnd<O, NullCaptureSource> {
    pub fn new(output: O) -> Self {
        Self::new_with_capture(output, NullCaptureSource)
    }

    pub fn new_with_host_sample_rate(output: O, host_sample_rate_hz: u32) -> Self {
        assert!(
            host_sample_rate_hz > 0,
            "host_sample_rate_hz must be non-zero"
        );
        Self::new_with_capture_and_host_sample_rate(output, NullCaptureSource, host_sample_rate_hz)
    }
}

impl<O: AudioSink, I: AudioCaptureSource> VirtioSnd<O, I> {
    pub fn new_with_capture(output: O, capture_source: I) -> Self {
        Self::new_with_capture_and_host_sample_rate(output, capture_source, PCM_SAMPLE_RATE_HZ)
    }

    pub fn new_with_capture_and_host_sample_rate(
        output: O,
        capture_source: I,
        host_sample_rate_hz: u32,
    ) -> Self {
        assert!(
            host_sample_rate_hz > 0,
            "host_sample_rate_hz must be non-zero"
        );
        let host_sample_rate_hz = host_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
        Self {
            output,
            capture_source,
            negotiated_features: 0,
            playback: PcmStream {
                params: None,
                state: StreamState::Idle,
            },
            capture: PcmStream {
                params: None,
                state: StreamState::Idle,
            },
            capture_telemetry: CaptureTelemetry::default(),
            host_sample_rate_hz,
            capture_sample_rate_hz: host_sample_rate_hz,
            resampler: LinearResampler::new(PCM_SAMPLE_RATE_HZ, host_sample_rate_hz),
            decoded_frames_scratch: Vec::new(),
            resampled_scratch: Vec::new(),
            capture_resampler: LinearResampler::new(host_sample_rate_hz, PCM_SAMPLE_RATE_HZ),
            capture_frames_scratch: Vec::new(),
            capture_interleaved_scratch: Vec::new(),
            capture_samples_scratch: Vec::new(),
            event_buffers: VecDeque::new(),
            pending_events: VecDeque::new(),
        }
    }

    pub fn host_sample_rate_hz(&self) -> u32 {
        self.host_sample_rate_hz
    }

    pub fn set_host_sample_rate_hz(&mut self, host_sample_rate_hz: u32) {
        assert!(
            host_sample_rate_hz > 0,
            "host_sample_rate_hz must be non-zero"
        );
        let host_sample_rate_hz = host_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
        if self.host_sample_rate_hz == host_sample_rate_hz {
            return;
        }
        let prev = self.host_sample_rate_hz;
        self.host_sample_rate_hz = host_sample_rate_hz;
        self.resampler
            .reset_rates(PCM_SAMPLE_RATE_HZ, host_sample_rate_hz);
        self.decoded_frames_scratch.clear();
        self.resampled_scratch.clear();

        // If the capture path is still using the default (same as the previous host/output rate),
        // keep it in sync. Callers that want distinct playback + capture rates should set
        // `set_capture_sample_rate_hz(...)` explicitly.
        if self.capture_sample_rate_hz == prev {
            self.capture_sample_rate_hz = host_sample_rate_hz;
            self.capture_resampler
                .reset_rates(host_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
            self.capture_frames_scratch.clear();
            self.capture_interleaved_scratch.clear();
            self.capture_samples_scratch.clear();
        }
    }

    pub fn capture_sample_rate_hz(&self) -> u32 {
        self.capture_sample_rate_hz
    }

    pub fn set_capture_sample_rate_hz(&mut self, capture_sample_rate_hz: u32) {
        assert!(
            capture_sample_rate_hz > 0,
            "capture_sample_rate_hz must be non-zero"
        );
        let capture_sample_rate_hz =
            capture_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
        if self.capture_sample_rate_hz == capture_sample_rate_hz {
            return;
        }
        self.capture_sample_rate_hz = capture_sample_rate_hz;
        self.capture_resampler
            .reset_rates(capture_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
        self.capture_frames_scratch.clear();
        self.capture_interleaved_scratch.clear();
        self.capture_samples_scratch.clear();
    }

    pub fn output_mut(&mut self) -> &mut O {
        &mut self.output
    }

    pub fn capture_source_mut(&mut self) -> &mut I {
        &mut self.capture_source
    }

    pub fn capture_telemetry(&self) -> CaptureTelemetry {
        self.capture_telemetry
    }

    /// Snapshot the virtio-snd internal stream state into an `aero-io-snapshot` schema.
    ///
    /// This does **not** snapshot virtqueue transport state; that lives in `VirtioPciDevice`.
    pub fn snapshot_state(&self) -> io_state::VirtioSndState {
        let encode_stream = |s: &PcmStream| -> io_state::VirtioSndStreamState {
            let state = match s.state {
                StreamState::Idle => 0,
                StreamState::ParamsSet => 1,
                StreamState::Prepared => 2,
                StreamState::Running => 3,
            };
            let params = s.params.map(|p| io_state::VirtioSndPcmParamsState {
                buffer_bytes: p.buffer_bytes,
                period_bytes: p.period_bytes,
                channels: p.channels,
                format: p.format,
                rate: p.rate,
            });
            io_state::VirtioSndStreamState { state, params }
        };

        io_state::VirtioSndState {
            playback: encode_stream(&self.playback),
            capture: encode_stream(&self.capture),
            capture_telemetry: io_state::VirtioSndCaptureTelemetryState {
                dropped_samples: self.capture_telemetry.dropped_samples,
                underrun_samples: self.capture_telemetry.underrun_samples,
                underrun_responses: self.capture_telemetry.underrun_responses,
            },
            host_sample_rate_hz: self.host_sample_rate_hz,
            capture_sample_rate_hz: self.capture_sample_rate_hz,
        }
    }

    /// Restore virtio-snd internal stream state from a snapshot.
    ///
    /// The snapshot schema does not include queued/resampled audio; those buffers are reset to
    /// silence deterministically.
    pub fn restore_state(&mut self, state: &io_state::VirtioSndState) {
        let decode_stream = |dst: &mut PcmStream, src: &io_state::VirtioSndStreamState| {
            dst.params = src.params.as_ref().map(|p| PcmParams {
                buffer_bytes: p.buffer_bytes,
                period_bytes: p.period_bytes,
                channels: p.channels,
                format: p.format,
                rate: p.rate,
            });
            dst.state = match src.state {
                0 => StreamState::Idle,
                1 => StreamState::ParamsSet,
                2 => StreamState::Prepared,
                3 => StreamState::Running,
                _ => StreamState::Idle,
            };
        };

        decode_stream(&mut self.playback, &state.playback);
        decode_stream(&mut self.capture, &state.capture);
        self.capture_telemetry = CaptureTelemetry {
            dropped_samples: state.capture_telemetry.dropped_samples,
            underrun_samples: state.capture_telemetry.underrun_samples,
            underrun_responses: state.capture_telemetry.underrun_responses,
        };

        // Rates in older snapshots (or corrupted inputs) may be 0. Treat 0 as "use the guest
        // contract rate" (48kHz) so resampling code continues to work.
        let host_rate = if state.host_sample_rate_hz == 0 {
            PCM_SAMPLE_RATE_HZ
        } else {
            state.host_sample_rate_hz
        };
        // Default capture to the restored host rate if missing.
        let capture_rate = if state.capture_sample_rate_hz == 0 {
            host_rate
        } else {
            state.capture_sample_rate_hz
        };

        self.host_sample_rate_hz = clamp_host_sample_rate_hz(host_rate);
        self.capture_sample_rate_hz = clamp_host_sample_rate_hz(capture_rate);

        // Reset resamplers and scratch buffers deterministically. Snapshot restores preserve
        // guest-visible stream state but do not attempt to restore buffered host audio.
        self.resampler
            .reset_rates(PCM_SAMPLE_RATE_HZ, self.host_sample_rate_hz);
        self.capture_resampler
            .reset_rates(self.capture_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
        self.decoded_frames_scratch.clear();
        self.resampled_scratch.clear();
        self.capture_frames_scratch.clear();
        self.capture_interleaved_scratch.clear();
        self.capture_samples_scratch.clear();

        // Event queue buffers are guest-provided and are not serialized; clear any cached chains so
        // the transport can repopulate them after restore (see `VirtioPciDevice::rewind_*` helper).
        self.event_buffers.clear();
        self.pending_events.clear();
    }

    fn process_control(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let request = read_all_out(mem, &chain);
        let response = self.handle_control_request(&request);
        let written = write_all_in(mem, &chain, &response);
        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn flush_eventq(
        &mut self,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let mut need_irq = false;

        while let Some(chain) = self.event_buffers.pop_front() {
            let Some(event) = self.pending_events.pop_front() else {
                self.event_buffers.push_front(chain);
                break;
            };

            let descs = chain.descriptors();
            if descs.is_empty() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }

            let mut written = 0usize;
            for d in descs {
                if !d.is_write_only() {
                    return Err(VirtioDeviceError::BadDescriptorChain);
                }
                if written == event.len() {
                    break;
                }
                let take = (d.len as usize).min(event.len() - written);
                let dst = mem
                    .get_slice_mut(d.addr, take)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                dst.copy_from_slice(&event[written..written + take]);
                written += take;
            }

            need_irq |= queue
                .add_used(mem, chain.head_index(), written as u32)
                .map_err(|_| VirtioDeviceError::IoError)?;
        }

        Ok(need_irq)
    }

    fn handle_control_request(&mut self, request: &[u8]) -> Vec<u8> {
        if request.len() < 4 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }
        let code = u32::from_le_bytes(request[0..4].try_into().unwrap());
        match code {
            VIRTIO_SND_R_PCM_INFO => self.cmd_pcm_info(request),
            VIRTIO_SND_R_PCM_SET_PARAMS => self.cmd_pcm_set_params(request),
            VIRTIO_SND_R_PCM_PREPARE => self.cmd_pcm_simple(request, StreamSimpleCmd::Prepare),
            VIRTIO_SND_R_PCM_RELEASE => self.cmd_pcm_simple(request, StreamSimpleCmd::Release),
            VIRTIO_SND_R_PCM_START => self.cmd_pcm_simple(request, StreamSimpleCmd::Start),
            VIRTIO_SND_R_PCM_STOP => self.cmd_pcm_simple(request, StreamSimpleCmd::Stop),
            VIRTIO_SND_R_JACK_INFO | VIRTIO_SND_R_JACK_REMAP | VIRTIO_SND_R_CHMAP_INFO => {
                virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP)
            }
            _ => virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP),
        }
    }

    fn cmd_pcm_info(&self, request: &[u8]) -> Vec<u8> {
        if request.len() < 12 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let start_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let count = u32::from_le_bytes(request[8..12].try_into().unwrap());

        let mut resp = virtio_snd_hdr(VIRTIO_SND_S_OK);
        if count == 0 {
            return resp;
        }

        let end = start_id.saturating_add(count);
        if start_id == PLAYBACK_STREAM_ID && PLAYBACK_STREAM_ID < end {
            resp.extend_from_slice(&virtio_snd_pcm_info(
                PLAYBACK_STREAM_ID,
                VIRTIO_SND_D_OUTPUT,
                PLAYBACK_CHANNELS,
            ));
        }
        if start_id <= CAPTURE_STREAM_ID && CAPTURE_STREAM_ID < end {
            resp.extend_from_slice(&virtio_snd_pcm_info(
                CAPTURE_STREAM_ID,
                VIRTIO_SND_D_INPUT,
                CAPTURE_CHANNELS,
            ));
        }

        resp
    }

    fn cmd_pcm_set_params(&mut self, request: &[u8]) -> Vec<u8> {
        if request.len() < 24 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let stream_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let stream = match stream_id {
            PLAYBACK_STREAM_ID => &mut self.playback,
            CAPTURE_STREAM_ID => &mut self.capture,
            _ => return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG),
        };

        let buffer_bytes = u32::from_le_bytes(request[8..12].try_into().unwrap());
        let period_bytes = u32::from_le_bytes(request[12..16].try_into().unwrap());
        let channels = request[20];
        let format = request[21];
        let rate = request[22];

        let expected_channels = if stream_id == PLAYBACK_STREAM_ID {
            PLAYBACK_CHANNELS
        } else {
            CAPTURE_CHANNELS
        };

        if channels != expected_channels
            || format != VIRTIO_SND_PCM_FMT_S16
            || rate != VIRTIO_SND_PCM_RATE_48000
        {
            return virtio_snd_hdr(VIRTIO_SND_S_NOT_SUPP);
        }

        stream.params = Some(PcmParams {
            buffer_bytes,
            period_bytes,
            channels,
            format,
            rate,
        });
        stream.state = StreamState::ParamsSet;

        virtio_snd_hdr(VIRTIO_SND_S_OK)
    }

    fn cmd_pcm_simple(&mut self, request: &[u8], cmd: StreamSimpleCmd) -> Vec<u8> {
        if request.len() < 8 {
            return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG);
        }

        let stream_id = u32::from_le_bytes(request[4..8].try_into().unwrap());
        let stream = match stream_id {
            PLAYBACK_STREAM_ID => &mut self.playback,
            CAPTURE_STREAM_ID => &mut self.capture,
            _ => return virtio_snd_hdr(VIRTIO_SND_S_BAD_MSG),
        };

        let status = match cmd {
            StreamSimpleCmd::Prepare => match stream.state {
                StreamState::ParamsSet | StreamState::Prepared => {
                    stream.state = StreamState::Prepared;
                    VIRTIO_SND_S_OK
                }
                StreamState::Running | StreamState::Idle => VIRTIO_SND_S_IO_ERR,
            },
            StreamSimpleCmd::Release => {
                stream.params = None;
                stream.state = StreamState::Idle;
                VIRTIO_SND_S_OK
            }
            StreamSimpleCmd::Start => match stream.state {
                StreamState::Prepared => {
                    stream.state = StreamState::Running;
                    VIRTIO_SND_S_OK
                }
                StreamState::Running => VIRTIO_SND_S_OK,
                StreamState::Idle | StreamState::ParamsSet => VIRTIO_SND_S_IO_ERR,
            },
            StreamSimpleCmd::Stop => match stream.state {
                StreamState::Running => {
                    stream.state = StreamState::Prepared;
                    VIRTIO_SND_S_OK
                }
                _ => VIRTIO_SND_S_IO_ERR,
            },
        };

        virtio_snd_hdr(status)
    }

    fn process_tx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let status = self.handle_tx_chain(mem, &chain);
        let resp = virtio_snd_pcm_status(status, 0);
        let written = write_all_in(mem, &chain, &resp);
        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn handle_tx_chain(&mut self, mem: &mut dyn GuestMemory, chain: &DescriptorChain) -> u32 {
        // Sum the readable descriptor lengths first so we can reject pathological sizes without
        // attempting to map large slices or allocate large decode buffers.
        let mut out_bytes_total: u64 = 0;
        let mut in_response = false;
        for d in chain.descriptors() {
            if d.is_write_only() {
                in_response = true;
                continue;
            }
            if in_response {
                // Virtio requires all device-writable descriptors to come after readable ones.
                return VIRTIO_SND_S_BAD_MSG;
            }
            out_bytes_total = out_bytes_total.saturating_add(d.len as u64);
            if out_bytes_total > 8 + MAX_PCM_XFER_BYTES {
                return VIRTIO_SND_S_BAD_MSG;
            }
        }
        if out_bytes_total < 8 {
            return VIRTIO_SND_S_BAD_MSG;
        }

        let mut hdr = [0u8; 8];
        let mut hdr_len = 0usize;
        let mut parsed_stream = false;

        let mut pending_lo: Option<u8> = None;
        let mut pending_left: Option<f32> = None;
        self.decoded_frames_scratch.clear();

        for desc in chain.descriptors().iter().filter(|d| !d.is_write_only()) {
            let mut slice = match mem.get_slice(desc.addr, desc.len as usize) {
                Ok(slice) => slice,
                Err(_) => return VIRTIO_SND_S_BAD_MSG,
            };

            if hdr_len < hdr.len() {
                let take = (hdr.len() - hdr_len).min(slice.len());
                hdr[hdr_len..hdr_len + take].copy_from_slice(&slice[..take]);
                hdr_len += take;
                slice = &slice[take..];

                if hdr_len < hdr.len() {
                    continue;
                }

                let stream_id = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                if stream_id != PLAYBACK_STREAM_ID {
                    return VIRTIO_SND_S_BAD_MSG;
                }

                if self.playback.state != StreamState::Running {
                    return VIRTIO_SND_S_IO_ERR;
                }

                parsed_stream = true;
            }

            for &b in slice {
                if let Some(lo) = pending_lo.take() {
                    let sample = i16::from_le_bytes([lo, b]);
                    let sample = sample as f32 / 32768.0;
                    if let Some(left) = pending_left.take() {
                        self.decoded_frames_scratch.push([left, sample]);
                    } else {
                        pending_left = Some(sample);
                    }
                } else {
                    pending_lo = Some(b);
                }
            }
        }

        if !parsed_stream || hdr_len != hdr.len() {
            return VIRTIO_SND_S_BAD_MSG;
        }

        if pending_lo.is_some() {
            return VIRTIO_SND_S_BAD_MSG;
        }

        // Stereo frames must be complete.
        if pending_left.is_some() {
            return VIRTIO_SND_S_BAD_MSG;
        }

        if !self.decoded_frames_scratch.is_empty() {
            self.resampled_scratch.clear();
            if self.host_sample_rate_hz == PCM_SAMPLE_RATE_HZ {
                self.resampled_scratch
                    .reserve(self.decoded_frames_scratch.len() * 2);
                for [l, r] in self.decoded_frames_scratch.iter().copied() {
                    self.resampled_scratch.push(l);
                    self.resampled_scratch.push(r);
                }
            } else {
                if self.resampler.dst_rate_hz() != self.host_sample_rate_hz {
                    self.resampler
                        .reset_rates(PCM_SAMPLE_RATE_HZ, self.host_sample_rate_hz);
                }
                self.resampler
                    .push_source_frames(&self.decoded_frames_scratch);

                // Best-effort reserve based on queued source frames. This avoids allocations in the
                // steady-state hot path once buffers have warmed up.
                let queued_src = self.resampler.queued_source_frames() as u64;
                let dst_rate = self.host_sample_rate_hz as u64;
                let reserve_frames =
                    queued_src.saturating_mul(dst_rate) / (PCM_SAMPLE_RATE_HZ as u64) + 2;
                self.resampled_scratch.reserve(reserve_frames as usize * 2);
                let _ = self
                    .resampler
                    .produce_available_interleaved_stereo_into(&mut self.resampled_scratch);
            }

            if !self.resampled_scratch.is_empty() {
                self.output.push_interleaved_f32(&self.resampled_scratch);
            }
        }

        VIRTIO_SND_S_OK
    }

    fn process_rx(
        &mut self,
        chain: DescriptorChain,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        let written = self.handle_rx_chain(mem, &chain);
        queue
            .add_used(mem, chain.head_index(), written)
            .map_err(|_| VirtioDeviceError::IoError)
    }

    fn handle_rx_chain(&mut self, mem: &mut dyn GuestMemory, chain: &DescriptorChain) -> u32 {
        let mut hdr = [0u8; 8];
        let mut hdr_len = 0usize;
        // Sum the device-readable descriptor lengths using saturating arithmetic so malicious
        // guests cannot trigger `usize` overflow (notably on 32-bit targets like wasm32).
        //
        // We only need to know whether the total exceeds the 8-byte header.
        let mut out_bytes_total: u64 = 0;
        for d in chain.descriptors().iter().filter(|d| !d.is_write_only()) {
            out_bytes_total = out_bytes_total.saturating_add(d.len as u64);
            if out_bytes_total > hdr.len() as u64 {
                break;
            }
        }
        let extra_out = out_bytes_total > hdr.len() as u64;

        for desc in chain.descriptors().iter().filter(|d| !d.is_write_only()) {
            if hdr_len >= hdr.len() {
                break;
            }
            let take = (hdr.len() - hdr_len).min(desc.len as usize);
            if take == 0 {
                continue;
            }
            let slice = match mem.get_slice(desc.addr, take) {
                Ok(slice) => slice,
                Err(_) => break,
            };
            hdr[hdr_len..hdr_len + take].copy_from_slice(slice);
            hdr_len += take;
        }

        let mut status = if hdr_len != hdr.len() || extra_out {
            VIRTIO_SND_S_BAD_MSG
        } else {
            let stream_id = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            if stream_id != CAPTURE_STREAM_ID {
                VIRTIO_SND_S_BAD_MSG
            } else if self.capture.state != StreamState::Running {
                VIRTIO_SND_S_IO_ERR
            } else {
                VIRTIO_SND_S_OK
            }
        };

        let in_descs: Vec<_> = chain
            .descriptors()
            .iter()
            .copied()
            .filter(|d| d.is_write_only())
            .collect();

        // If the guest forgot to provide any writable buffers, we can't return a
        // response payload or status. Still complete the chain to avoid
        // stalling the virtqueue.
        if in_descs.is_empty() {
            return 0;
        }

        if in_descs.len() < 2 {
            status = VIRTIO_SND_S_BAD_MSG;
        }

        let resp_desc = *in_descs.last().unwrap();
        let payload_descs = &in_descs[..in_descs.len().saturating_sub(1)];

        let payload_bytes_u64 = payload_descs
            .iter()
            .fold(0u64, |acc, d| acc.saturating_add(d.len as u64));
        if payload_bytes_u64 > MAX_PCM_XFER_BYTES {
            status = VIRTIO_SND_S_BAD_MSG;
        }
        let payload_bytes = match usize::try_from(payload_bytes_u64) {
            Ok(v) => v,
            Err(_) => {
                status = VIRTIO_SND_S_BAD_MSG;
                0
            }
        };
        if !payload_bytes.is_multiple_of(2) {
            status = VIRTIO_SND_S_BAD_MSG;
        }

        if resp_desc.len < 8 {
            status = VIRTIO_SND_S_BAD_MSG;
        }

        // Always write deterministic output into the guest payload buffers.
        let payload_written = if payload_descs.is_empty() {
            0usize
        } else if status == VIRTIO_SND_S_OK {
            let samples_needed = payload_bytes / 2;
            if samples_needed == 0 {
                write_payload_silence(mem, payload_descs, MAX_PCM_XFER_BYTES as usize)
            } else {
                self.capture_telemetry.dropped_samples +=
                    self.capture_source.take_dropped_samples();

                if self.capture_sample_rate_hz == PCM_SAMPLE_RATE_HZ {
                    self.capture_samples_scratch.resize(samples_needed, 0.0);
                    let got = self
                        .capture_source
                        .read_mono_f32(&mut self.capture_samples_scratch[..]);
                    if got < samples_needed {
                        self.capture_telemetry.underrun_samples += (samples_needed - got) as u64;
                        self.capture_telemetry.underrun_responses += 1;
                        self.capture_samples_scratch[got..].fill(0.0);
                    }

                    write_pcm_payload_s16le(
                        mem,
                        payload_descs,
                        &self.capture_samples_scratch[..samples_needed],
                    )
                } else {
                    // Resample host microphone samples from the host/input rate to the guest contract
                    // rate (48kHz). `AudioCaptureSource` itself has no sample rate metadata, so the
                    // device relies on `capture_sample_rate_hz` being configured to match the host
                    // capture graph (typically `AudioContext.sampleRate`). By default, the capture
                    // rate tracks `host_sample_rate_hz`.
                    if self.capture_resampler.src_rate_hz() != self.capture_sample_rate_hz
                        || self.capture_resampler.dst_rate_hz() != PCM_SAMPLE_RATE_HZ
                    {
                        self.capture_resampler
                            .reset_rates(self.capture_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
                    }

                    let required_src = self
                        .capture_resampler
                        .required_source_frames(samples_needed);
                    let queued_src = self.capture_resampler.queued_source_frames();
                    let need_src = required_src.saturating_sub(queued_src);

                    if need_src > 0 {
                        self.capture_samples_scratch.resize(need_src, 0.0);
                        let got = self
                            .capture_source
                            .read_mono_f32(&mut self.capture_samples_scratch[..]);
                        if got < need_src {
                            // Track underruns in host sample units (matches `dropped_samples`).
                            self.capture_telemetry.underrun_samples += (need_src - got) as u64;
                            self.capture_telemetry.underrun_responses += 1;
                            self.capture_samples_scratch[got..].fill(0.0);
                        }

                        self.capture_frames_scratch.resize(need_src, [0.0; 2]);
                        for (dst, &s) in self
                            .capture_frames_scratch
                            .iter_mut()
                            .zip(&self.capture_samples_scratch)
                        {
                            *dst = [s, s];
                        }
                        self.capture_resampler
                            .push_source_frames(&self.capture_frames_scratch);
                    }

                    let produced_frames = self.capture_resampler.produce_interleaved_stereo_into(
                        samples_needed,
                        &mut self.capture_interleaved_scratch,
                    );

                    self.capture_samples_scratch.resize(produced_frames, 0.0);
                    for i in 0..produced_frames {
                        self.capture_samples_scratch[i] = self.capture_interleaved_scratch[i * 2];
                    }

                    write_pcm_payload_s16le(mem, payload_descs, &self.capture_samples_scratch)
                }
            }
        } else {
            write_payload_silence(mem, payload_descs, MAX_PCM_XFER_BYTES as usize)
        };

        let resp = virtio_snd_pcm_status(status, 0);
        let resp_len = (resp_desc.len as usize).min(resp.len());
        let mut resp_written = 0usize;
        if resp_len != 0 {
            if let Ok(out) = mem.get_slice_mut(resp_desc.addr, resp_len) {
                out.copy_from_slice(&resp[..resp_len]);
                resp_written = resp_len;
            }
        }

        (payload_written + resp_written) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamSimpleCmd {
    Prepare,
    Release,
    Start,
    Stop,
}

fn virtio_snd_hdr(status: u32) -> Vec<u8> {
    status.to_le_bytes().to_vec()
}

fn virtio_snd_pcm_info(stream_id: u32, direction: u8, channels: u8) -> [u8; 32] {
    let mut buf = [0u8; 32];
    // stream_id
    buf[0..4].copy_from_slice(&stream_id.to_le_bytes());
    // features
    buf[4..8].copy_from_slice(&0u32.to_le_bytes());
    // formats/rates bitmasks
    buf[8..16].copy_from_slice(&VIRTIO_SND_PCM_FMT_MASK_S16.to_le_bytes());
    buf[16..24].copy_from_slice(&VIRTIO_SND_PCM_RATE_MASK_48000.to_le_bytes());
    // direction + channel bounds
    buf[24] = direction;
    buf[25] = channels;
    buf[26] = channels;
    buf
}

fn virtio_snd_pcm_status(status: u32, latency_bytes: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&status.to_le_bytes());
    buf[4..8].copy_from_slice(&latency_bytes.to_le_bytes());
    buf
}

fn read_all_out(mem: &dyn GuestMemory, chain: &DescriptorChain) -> Vec<u8> {
    // Control requests are small (<=24 bytes for the subset we implement), so a
    // small cap avoids pathological allocations from malicious guests.
    const MAX_BYTES: usize = 4096;

    let mut out = Vec::new();
    for d in chain.descriptors().iter().filter(|d| !d.is_write_only()) {
        if out.len() >= MAX_BYTES {
            break;
        }
        let remaining = MAX_BYTES - out.len();
        let take = (d.len as usize).min(remaining);
        if take == 0 {
            continue;
        }
        let slice = match mem.get_slice(d.addr, take) {
            Ok(slice) => slice,
            Err(_) => break,
        };
        out.extend_from_slice(slice);
    }
    out
}

fn write_all_in(mem: &mut dyn GuestMemory, chain: &DescriptorChain, data: &[u8]) -> u32 {
    let mut remaining = data;
    let mut written = 0usize;
    for d in chain.descriptors().iter().filter(|d| d.is_write_only()) {
        if remaining.is_empty() {
            break;
        }
        let take = (d.len as usize).min(remaining.len());
        let Ok(dst) = mem.get_slice_mut(d.addr, take) else {
            break;
        };
        dst.copy_from_slice(&remaining[..take]);
        written += take;
        remaining = &remaining[take..];
    }

    written as u32
}

fn write_payload_silence(
    mem: &mut dyn GuestMemory,
    descs: &[crate::queue::Descriptor],
    max_bytes: usize,
) -> usize {
    let mut written = 0usize;
    for d in descs {
        if written >= max_bytes {
            break;
        }
        if d.len == 0 {
            continue;
        }
        let remaining = max_bytes - written;
        let take = (d.len as usize).min(remaining);
        if take == 0 {
            continue;
        }
        let Ok(slice) = mem.get_slice_mut(d.addr, take) else {
            break;
        };
        slice.fill(0);
        written += take;
    }
    written
}

fn f32_to_i16(sample: f32) -> i16 {
    let s = sample.clamp(-1.0, 1.0);
    let scaled = (s * 32768.0).round();
    let clamped = scaled.clamp(i16::MIN as f32, i16::MAX as f32);
    clamped as i16
}

fn write_pcm_payload_s16le(
    mem: &mut dyn GuestMemory,
    descs: &[crate::queue::Descriptor],
    samples: &[f32],
) -> usize {
    let mut sample_iter = samples.iter();
    let mut cur_bytes = [0u8; 2];
    let mut cur_pos = 2usize;
    let mut written = 0usize;

    for d in descs {
        if d.len == 0 {
            continue;
        }
        let Ok(slice) = mem.get_slice_mut(d.addr, d.len as usize) else {
            break;
        };

        for b in slice {
            if cur_pos >= 2 {
                let sample = *sample_iter.next().unwrap_or(&0.0);
                cur_bytes = f32_to_i16(sample).to_le_bytes();
                cur_pos = 0;
            }
            *b = cur_bytes[cur_pos];
            cur_pos += 1;
            written += 1;
        }
    }

    written
}

impl<O: AudioSink + 'static, I: AudioCaptureSource + 'static> VirtioDevice for VirtioSnd<O, I> {
    fn device_type(&self) -> u16 {
        VIRTIO_DEVICE_TYPE_SND
    }

    fn device_features(&self) -> u64 {
        VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC
    }

    fn set_features(&mut self, features: u64) {
        self.negotiated_features = features;
    }

    fn num_queues(&self) -> u16 {
        4
    }

    fn queue_max_size(&self, queue: u16) -> u16 {
        match queue {
            VIRTIO_SND_QUEUE_CONTROL | VIRTIO_SND_QUEUE_EVENT | VIRTIO_SND_QUEUE_RX => 64,
            VIRTIO_SND_QUEUE_TX => 256,
            _ => 0,
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
            VIRTIO_SND_QUEUE_EVENT => {
                if chain.descriptors().is_empty()
                    || chain.descriptors().iter().any(|d| !d.is_write_only())
                {
                    return Err(VirtioDeviceError::BadDescriptorChain);
                }

                self.event_buffers.push_back(chain);
                self.flush_eventq(queue, mem)
            }
            VIRTIO_SND_QUEUE_TX => self.process_tx(chain, queue, mem),
            VIRTIO_SND_QUEUE_RX => self.process_rx(chain, queue, mem),
            _ => queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError),
        }
    }

    fn poll_queue(
        &mut self,
        queue_index: u16,
        queue: &mut VirtQueue,
        mem: &mut dyn GuestMemory,
    ) -> Result<bool, VirtioDeviceError> {
        match queue_index {
            VIRTIO_SND_QUEUE_EVENT => self.flush_eventq(queue, mem),
            _ => Ok(false),
        }
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // virtio-snd config:
        // 0x00 le32 jacks
        // 0x04 le32 streams
        // 0x08 le32 chmaps
        let mut cfg = [0u8; 12];
        cfg[0..4].copy_from_slice(&0u32.to_le_bytes());
        cfg[4..8].copy_from_slice(&2u32.to_le_bytes());
        cfg[8..12].copy_from_slice(&0u32.to_le_bytes());

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
        self.playback = PcmStream {
            params: None,
            state: StreamState::Idle,
        };
        self.capture = PcmStream {
            params: None,
            state: StreamState::Idle,
        };
        self.capture_telemetry = CaptureTelemetry::default();
        self.resampler
            .reset_rates(PCM_SAMPLE_RATE_HZ, self.host_sample_rate_hz);
        self.decoded_frames_scratch.clear();
        self.resampled_scratch.clear();
        self.capture_resampler
            .reset_rates(self.capture_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
        self.capture_frames_scratch.clear();
        self.capture_interleaved_scratch.clear();
        self.capture_samples_scratch.clear();
        self.event_buffers.clear();
        self.pending_events.clear();
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::VirtioDevice;

    fn status(resp: &[u8]) -> u32 {
        u32::from_le_bytes(resp[0..4].try_into().unwrap())
    }

    #[test]
    fn virtio_snd_clamps_host_and_capture_sample_rates_to_avoid_oom() {
        let mut snd = VirtioSnd::new_with_host_sample_rate(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
            u32::MAX,
        );
        assert_eq!(
            snd.host_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );
        assert_eq!(
            snd.capture_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );

        snd.set_host_sample_rate_hz(u32::MAX);
        assert_eq!(
            snd.host_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );

        snd.set_capture_sample_rate_hz(u32::MAX);
        assert_eq!(
            snd.capture_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );
    }

    #[test]
    fn virtio_snd_config_reports_two_streams() {
        let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut cfg = [0u8; 12];
        snd.read_config(0, &mut cfg);

        assert_eq!(u32::from_le_bytes(cfg[0..4].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(cfg[4..8].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(cfg[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn virtio_snd_contract_v1_features() {
        let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        assert_eq!(
            snd.device_features(),
            VIRTIO_F_VERSION_1 | VIRTIO_F_RING_INDIRECT_DESC
        );
    }

    #[test]
    fn virtio_snd_contract_v1_queue_sizes() {
        let snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        assert_eq!(snd.queue_max_size(VIRTIO_SND_QUEUE_CONTROL), 64);
        assert_eq!(snd.queue_max_size(VIRTIO_SND_QUEUE_EVENT), 64);
        assert_eq!(snd.queue_max_size(VIRTIO_SND_QUEUE_TX), 256);
        assert_eq!(snd.queue_max_size(VIRTIO_SND_QUEUE_RX), 64);
    }

    #[test]
    fn control_pcm_prepare_requires_params() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        let mut prepare = Vec::new();
        prepare.extend_from_slice(&VIRTIO_SND_R_PCM_PREPARE.to_le_bytes());
        prepare.extend_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        let resp = snd.handle_control_request(&prepare);
        assert_eq!(status(&resp), VIRTIO_SND_S_IO_ERR);
    }

    #[test]
    fn control_pcm_set_params_rejects_unsupported_format() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        let mut req = [0u8; 24];
        req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
        req[4..8].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        req[8..12].copy_from_slice(&4096u32.to_le_bytes());
        req[12..16].copy_from_slice(&1024u32.to_le_bytes());
        // features [16..20] = 0
        req[20] = 1; // channels (unsupported; device is fixed stereo)
        req[21] = VIRTIO_SND_PCM_FMT_S16;
        req[22] = VIRTIO_SND_PCM_RATE_48000;

        let resp = snd.handle_control_request(&req);
        assert_eq!(status(&resp), VIRTIO_SND_S_NOT_SUPP);
    }

    #[test]
    fn control_pcm_info_reports_playback_and_capture_streams() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        let mut req = Vec::new();
        req.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
        req.extend_from_slice(&0u32.to_le_bytes()); // start_id
        req.extend_from_slice(&2u32.to_le_bytes()); // count

        let resp = snd.handle_control_request(&req);
        assert_eq!(status(&resp), VIRTIO_SND_S_OK);
        assert_eq!(resp.len(), 4 + 32 + 32);

        let playback = &resp[4..4 + 32];
        assert_eq!(
            u32::from_le_bytes(playback[0..4].try_into().unwrap()),
            PLAYBACK_STREAM_ID
        );
        assert_eq!(playback[24], VIRTIO_SND_D_OUTPUT);
        assert_eq!(playback[25], PLAYBACK_CHANNELS);
        assert_eq!(playback[26], PLAYBACK_CHANNELS);

        let capture = &resp[4 + 32..4 + 64];
        assert_eq!(
            u32::from_le_bytes(capture[0..4].try_into().unwrap()),
            CAPTURE_STREAM_ID
        );
        assert_eq!(capture[24], VIRTIO_SND_D_INPUT);
        assert_eq!(capture[25], CAPTURE_CHANNELS);
        assert_eq!(capture[26], CAPTURE_CHANNELS);
    }

    #[test]
    fn control_pcm_info_respects_requested_range() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        let mut req = Vec::new();
        req.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
        req.extend_from_slice(&CAPTURE_STREAM_ID.to_le_bytes()); // start_id
        req.extend_from_slice(&1u32.to_le_bytes()); // count

        let resp = snd.handle_control_request(&req);
        assert_eq!(status(&resp), VIRTIO_SND_S_OK);
        assert_eq!(resp.len(), 4 + 32);

        let entry = &resp[4..];
        assert_eq!(
            u32::from_le_bytes(entry[0..4].try_into().unwrap()),
            CAPTURE_STREAM_ID
        );
        assert_eq!(entry[24], VIRTIO_SND_D_INPUT);
        assert_eq!(entry[25], CAPTURE_CHANNELS);
        assert_eq!(entry[26], CAPTURE_CHANNELS);
    }
}
