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

// virtio-snd JACK IDs exposed by the Aero device model / Win7 topology mapping.
pub const JACK_ID_SPEAKER: u32 = 0;
pub const JACK_ID_MICROPHONE: u32 = 1;
pub const JACK_ID_COUNT: u32 = 2;

// virtio-snd eventq messages (virtio-snd specification).
pub const VIRTIO_SND_EVT_JACK_CONNECTED: u32 = 0x1000;
pub const VIRTIO_SND_EVT_JACK_DISCONNECTED: u32 = 0x1001;
pub const VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED: u32 = 0x1100;
pub const VIRTIO_SND_EVT_PCM_XRUN: u32 = 0x1101;
pub const VIRTIO_SND_EVT_CTL_NOTIFY: u32 = 0x1200;

/// Sample rate used by the (minimal) virtio-snd guest contract implemented by this device.
///
/// The TX and RX PCM payloads are fixed at 48kHz S16_LE in the guest-facing ABI.
pub const PCM_SAMPLE_RATE_HZ: u32 = 48_000;

/// Contract v1 safety cap for PCM payload bytes in a single TX/RX descriptor chain.
///
/// This value is normative for the Windows 7 guest driver contract:
/// `docs/windows7-virtio-driver-contract.md` §3.4.6.
///
/// The cap is on **PCM payload bytes** (excluding the 8-byte TX header and excluding the RX
/// header/status descriptors).
///
/// This device is guest-driven and must treat descriptor lengths as untrusted. A malicious guest
/// could otherwise force the host to allocate unbounded scratch buffers when decoding/resampling.
///
/// 256 KiB (262,144 bytes) is ~1.3s of stereo S16_LE at 48kHz (and ~2.6s of mono capture), which is
/// plenty for the minimal Win7 contract while still bounding worst-case allocations.
const MAX_PCM_XFER_BYTES: u64 = 256 * 1024;

/// Defensive cap on the number of queued virtio-snd events.
///
/// The virtio-snd `eventq` is guest-driven: the guest must post writable buffers before the device
/// can deliver events. The Aero contract v1 does not require any events and some guests may not
/// service the queue promptly. To avoid unbounded host memory growth (e.g. a malicious guest that
/// never posts event buffers, combined with a host integration that keeps queueing events), cap the
/// pending FIFO at a small bounded size.
const MAX_PENDING_EVENTS: usize = 256;

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
    pending_events: VecDeque<[u8; 8]>,
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

    /// Queue a raw virtio-snd `eventq` message (`struct virtio_snd_event`).
    ///
    /// The message is delivered best-effort: it will only be written once the guest posts a writable
    /// event buffer chain on `eventq` and the transport polls queues.
    pub fn queue_event(&mut self, event_type: u32, data: u32) {
        if self.pending_events.len() >= MAX_PENDING_EVENTS {
            // Drop the oldest event to preserve bounded memory usage while keeping the most recent
            // state transitions (e.g. jack connected/disconnected).
            let _ = self.pending_events.pop_front();
        }
        let mut evt = [0u8; 8];
        evt[0..4].copy_from_slice(&event_type.to_le_bytes());
        evt[4..8].copy_from_slice(&data.to_le_bytes());
        self.pending_events.push_back(evt);
    }

    /// Convenience helper: queue a JACK connected/disconnected event.
    pub fn queue_jack_event(&mut self, jack_id: u32, connected: bool) {
        let event_type = if connected {
            VIRTIO_SND_EVT_JACK_CONNECTED
        } else {
            VIRTIO_SND_EVT_JACK_DISCONNECTED
        };
        self.queue_event(event_type, jack_id);
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

    fn reset_playback_audio_state(&mut self) {
        self.resampler
            .reset_rates(PCM_SAMPLE_RATE_HZ, self.host_sample_rate_hz);
        self.decoded_frames_scratch.clear();
        self.resampled_scratch.clear();
    }

    fn reset_capture_audio_state(&mut self) {
        self.capture_resampler
            .reset_rates(self.capture_sample_rate_hz, PCM_SAMPLE_RATE_HZ);
        self.capture_frames_scratch.clear();
        self.capture_interleaved_scratch.clear();
        self.capture_samples_scratch.clear();
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
            let Some(event_len) = self.pending_events.front().map(|event| event.len()) else {
                self.event_buffers.push_front(chain);
                break;
            };

            let descs = chain.descriptors();
            if descs.is_empty() {
                return Err(VirtioDeviceError::BadDescriptorChain);
            }

            let mut capacity = 0usize;
            for d in descs {
                if !d.is_write_only() {
                    return Err(VirtioDeviceError::BadDescriptorChain);
                }
                if capacity >= event_len {
                    break;
                }
                capacity = capacity.saturating_add(d.len as usize);
            }

            // If the guest posted a buffer chain that is too small for a full event,
            // complete it with 0 bytes (returning it to the guest) but keep the
            // pending event so it can be delivered into a future adequately-sized
            // chain.
            if capacity < event_len {
                need_irq |= queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                continue;
            }

            // Ensure the guest buffer is valid before writing. If the guest supplies an out-of-bounds
            // DMA range, treat it as a malformed eventq chain and complete it with 0 bytes (without
            // dropping the pending event).
            let mut checked = 0usize;
            let mut guest_ok = true;
            for d in descs {
                if checked == event_len {
                    break;
                }
                let take = (d.len as usize).min(event_len - checked);
                if take == 0 {
                    continue;
                }
                if mem.get_slice(d.addr, take).is_err() {
                    guest_ok = false;
                    break;
                }
                checked += take;
            }
            if !guest_ok {
                need_irq |= queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                continue;
            }

            let mut written = 0usize;
            let mut wrote_all = true;
            {
                // Borrow the pending event immutably until the write completes; only pop it from
                // the queue once we know it has been fully delivered.
                let event = self
                    .pending_events
                    .front()
                    .expect("pending_events was non-empty when event_len was read");

                for d in descs {
                    if written == event_len {
                        break;
                    }
                    let take = (d.len as usize).min(event_len - written);
                    if take == 0 {
                        continue;
                    }
                    let Ok(dst) = mem.get_slice_mut(d.addr, take) else {
                        wrote_all = false;
                        break;
                    };
                    dst.copy_from_slice(&event[written..written + take]);
                    written += take;
                }
            }

            if !wrote_all || written != event_len {
                need_irq |= queue
                    .add_used(mem, chain.head_index(), 0)
                    .map_err(|_| VirtioDeviceError::IoError)?;
                continue;
            }

            // The event was fully written into guest memory; retire it.
            self.pending_events
                .pop_front()
                .expect("pending_events was non-empty when write succeeded");

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
        let mut reset_playback = false;
        let mut reset_capture = false;

        let status = match stream_id {
            PLAYBACK_STREAM_ID => {
                let stream = &mut self.playback;
                match cmd {
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
                        reset_playback = true;
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
                            reset_playback = true;
                            VIRTIO_SND_S_OK
                        }
                        _ => VIRTIO_SND_S_IO_ERR,
                    },
                }
            }
            CAPTURE_STREAM_ID => {
                let stream = &mut self.capture;
                match cmd {
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
                        reset_capture = true;
                        // Note: keep `capture_telemetry` across stream lifecycles so host-level mic
                        // underrun/drop debugging survives guest stop/restart flows.
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
                            reset_capture = true;
                            VIRTIO_SND_S_OK
                        }
                        _ => VIRTIO_SND_S_IO_ERR,
                    },
                }
            }
            _ => VIRTIO_SND_S_BAD_MSG,
        };

        if reset_playback {
            self.reset_playback_audio_state();
        }
        if reset_capture {
            self.reset_capture_audio_state();
        }

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
        if take == 0 {
            continue;
        }
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

                // Prevent unbounded growth if a corrupted/malicious driver repeatedly publishes
                // event buffers (e.g. by moving `avail.idx` far ahead and causing the transport to
                // re-consume stale ring entries). A correct driver cannot have more outstanding
                // event buffers than the queue size.
                let max_buffers = queue.size() as usize;
                if max_buffers != 0 && self.event_buffers.len() >= max_buffers {
                    return queue
                        .add_used(mem, chain.head_index(), 0)
                        .map_err(|_| VirtioDeviceError::IoError);
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
    use crate::memory::{
        read_u16_le, read_u32_le, write_u16_le, write_u32_le, write_u64_le, GuestMemory, GuestRam,
    };
    use crate::queue::{
        PoppedDescriptorChain, VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
    };

    #[derive(Debug, Default, Clone)]
    struct TestCaptureSource {
        inner: aero_audio::capture::VecDequeCaptureSource,
        read_calls: usize,
        samples_read: usize,
        last_requested: Option<usize>,
        dropped_samples: u64,
    }

    impl TestCaptureSource {
        fn push_samples(&mut self, samples: &[f32]) {
            self.inner.push_samples(samples);
        }

        fn remaining_samples(&self) -> usize {
            self.inner.len()
        }
    }

    impl AudioCaptureSource for TestCaptureSource {
        fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
            self.read_calls += 1;
            self.last_requested = Some(out.len());
            let got = self.inner.read_mono_f32(out);
            self.samples_read += got;
            got
        }

        fn take_dropped_samples(&mut self) -> u64 {
            let v = self.dropped_samples;
            self.dropped_samples = 0;
            v
        }
    }

    /// [`GuestMemory`] implementation that panics on any access.
    ///
    /// Used to ensure some rejection paths return before attempting to map guest buffers.
    struct PanicGuestMemory;

    impl GuestMemory for PanicGuestMemory {
        fn len(&self) -> u64 {
            0
        }

        fn read(&self, _addr: u64, _dst: &mut [u8]) -> Result<(), crate::memory::GuestMemoryError> {
            panic!("unexpected GuestMemory::read")
        }

        fn write(
            &mut self,
            _addr: u64,
            _src: &[u8],
        ) -> Result<(), crate::memory::GuestMemoryError> {
            panic!("unexpected GuestMemory::write")
        }

        fn get_slice(
            &self,
            _addr: u64,
            _len: usize,
        ) -> Result<&[u8], crate::memory::GuestMemoryError> {
            panic!("unexpected GuestMemory::get_slice")
        }

        fn get_slice_mut(
            &mut self,
            _addr: u64,
            _len: usize,
        ) -> Result<&mut [u8], crate::memory::GuestMemoryError> {
            panic!("unexpected GuestMemory::get_slice_mut")
        }
    }

    fn status(resp: &[u8]) -> u32 {
        u32::from_le_bytes(resp[0..4].try_into().unwrap())
    }

    fn write_desc(
        mem: &mut GuestRam,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    fn pop_chain(queue: &mut VirtQueue, mem: &GuestRam) -> DescriptorChain {
        match queue.pop_descriptor_chain(mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        }
    }

    fn read_status_code(mem: &GuestRam, addr: u64) -> u32 {
        read_u32_le(mem, addr).unwrap()
    }

    fn drive_playback_to_prepared(snd: &mut VirtioSnd<aero_audio::ring::AudioRingBuffer>) {
        control_set_params(snd, PLAYBACK_STREAM_ID);
        control_simple(snd, VIRTIO_SND_R_PCM_PREPARE, PLAYBACK_STREAM_ID);
    }

    fn drive_playback_to_running(snd: &mut VirtioSnd<aero_audio::ring::AudioRingBuffer>) {
        drive_playback_to_prepared(snd);
        control_simple(snd, VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID);
    }

    fn write_bytes(mem: &mut GuestRam, addr: u64, data: &[u8]) {
        let start = addr as usize;
        let end = start + data.len();
        mem.as_mut_slice()[start..end].copy_from_slice(data);
    }

    fn build_chain(
        mem: &mut GuestRam,
        desc_table: u64,
        avail: u64,
        used: u64,
        descs: &[(u64, u32, bool)],
    ) -> DescriptorChain {
        let qsize = 8u16;
        assert!(
            descs.len() <= qsize as usize,
            "test descriptor chain length must fit in the fixed-size virtqueue"
        );

        for (i, &(addr, len, write_only)) in descs.iter().enumerate() {
            let mut flags = 0u16;
            let next = if i + 1 < descs.len() {
                flags |= VIRTQ_DESC_F_NEXT;
                (i + 1) as u16
            } else {
                0
            };
            if write_only {
                flags |= VIRTQ_DESC_F_WRITE;
            }
            write_desc(mem, desc_table, i as u16, addr, len, flags, next);
        }

        // Populate a single avail ring entry pointing at descriptor 0.
        write_u16_le(mem, avail, 0).unwrap(); // avail.flags
        write_u16_le(mem, avail + 2, 1).unwrap(); // avail.idx
        write_u16_le(mem, avail + 4, 0).unwrap(); // avail.ring[0] = head index
        write_u16_le(mem, used, 0).unwrap(); // used.flags
        write_u16_le(mem, used + 2, 0).unwrap(); // used.idx

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        match queue.pop_descriptor_chain(mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        }
    }

    fn control_set_params<O: AudioSink, I: AudioCaptureSource>(
        snd: &mut VirtioSnd<O, I>,
        stream_id: u32,
    ) {
        let channels = if stream_id == PLAYBACK_STREAM_ID {
            PLAYBACK_CHANNELS
        } else {
            CAPTURE_CHANNELS
        };
        let mut req = [0u8; 24];
        req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
        req[4..8].copy_from_slice(&stream_id.to_le_bytes());
        req[8..12].copy_from_slice(&4096u32.to_le_bytes());
        req[12..16].copy_from_slice(&1024u32.to_le_bytes());
        req[20] = channels;
        req[21] = VIRTIO_SND_PCM_FMT_S16;
        req[22] = VIRTIO_SND_PCM_RATE_48000;

        let resp = snd.handle_control_request(&req);
        assert_eq!(status(&resp), VIRTIO_SND_S_OK);
    }

    fn control_simple<O: AudioSink, I: AudioCaptureSource>(
        snd: &mut VirtioSnd<O, I>,
        code: u32,
        stream_id: u32,
    ) {
        let mut req = Vec::new();
        req.extend_from_slice(&code.to_le_bytes());
        req.extend_from_slice(&stream_id.to_le_bytes());
        let resp = snd.handle_control_request(&req);
        assert_eq!(status(&resp), VIRTIO_SND_S_OK);
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
    fn virtio_snd_snapshot_restore_roundtrip_stream_state_and_params() {
        let mut snd = VirtioSnd::new_with_host_sample_rate(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
            44_100,
        );
        snd.set_capture_sample_rate_hz(32_000);

        snd.playback = PcmStream {
            state: StreamState::Running,
            params: Some(PcmParams {
                buffer_bytes: 4096,
                period_bytes: 1024,
                channels: PLAYBACK_CHANNELS,
                format: VIRTIO_SND_PCM_FMT_S16,
                rate: VIRTIO_SND_PCM_RATE_48000,
            }),
        };
        snd.capture = PcmStream {
            state: StreamState::Prepared,
            params: Some(PcmParams {
                buffer_bytes: 2048,
                period_bytes: 512,
                channels: CAPTURE_CHANNELS,
                format: VIRTIO_SND_PCM_FMT_S16,
                rate: VIRTIO_SND_PCM_RATE_48000,
            }),
        };
        snd.capture_telemetry = CaptureTelemetry {
            dropped_samples: 7,
            underrun_samples: 11,
            underrun_responses: 13,
        };

        let state = snd.snapshot_state();

        let mut restored = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        restored.restore_state(&state);
        assert_eq!(restored.host_sample_rate_hz(), 44_100);
        assert_eq!(restored.capture_sample_rate_hz(), 32_000);

        let reencoded = restored.snapshot_state();
        assert_eq!(reencoded, state);
    }

    #[test]
    fn virtio_snd_restore_state_defaults_and_clamps_sample_rates() {
        // Legacy/corrupted snapshots may contain 0 sample rates. Treat 0 as the guest contract
        // rate (48kHz), and default capture to the restored host rate.
        let state_zero_rates = io_state::VirtioSndState {
            host_sample_rate_hz: 0,
            capture_sample_rate_hz: 0,
            ..io_state::VirtioSndState::default()
        };

        let mut snd = VirtioSnd::new_with_host_sample_rate(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
            44_100,
        );
        snd.set_capture_sample_rate_hz(32_000);
        snd.restore_state(&state_zero_rates);
        assert_eq!(snd.host_sample_rate_hz(), PCM_SAMPLE_RATE_HZ);
        assert_eq!(snd.capture_sample_rate_hz(), PCM_SAMPLE_RATE_HZ);

        // Clamp absurd sample rates to avoid multi-gigabyte allocations on restore.
        let state_absurd_rates = io_state::VirtioSndState {
            host_sample_rate_hz: u32::MAX,
            capture_sample_rate_hz: u32::MAX,
            ..io_state::VirtioSndState::default()
        };
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        snd.restore_state(&state_absurd_rates);
        assert_eq!(
            snd.host_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );
        assert_eq!(
            snd.capture_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );
    }

    #[test]
    fn virtio_snd_restore_state_clears_runtime_event_queues() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 1u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // Seed a single write-only descriptor so we can create a real `DescriptorChain`.
        write_desc(&mut mem, desc_table, 0, 0x4000, 8, VIRTQ_DESC_F_WRITE, 0);
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = match queue
            .pop_descriptor_chain(&mem)
            .unwrap()
            .expect("expected descriptor chain")
        {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };

        snd.event_buffers.push_back(chain);
        snd.queue_event(1, 2);
        assert!(!snd.event_buffers.is_empty());
        assert!(!snd.pending_events.is_empty());

        snd.restore_state(&io_state::VirtioSndState::default());
        assert!(
            snd.event_buffers.is_empty(),
            "restore_state must clear guest-provided event buffers"
        );
        assert!(
            snd.pending_events.is_empty(),
            "restore_state must clear pending event payloads"
        );
    }

    #[test]
    fn virtio_snd_eventq_flush_writes_pending_event_and_completes_used() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf = 0x4000;

        let qsize = 1u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // One write-only event buffer (virtio-snd events are 8 bytes).
        write_desc(&mut mem, desc_table, 0, buf, 8, VIRTQ_DESC_F_WRITE, 0);
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        let evt_type = 0x1100u32; // VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED
        let evt_data = 0x1234u32;
        let mut evt = [0u8; 8];
        evt[0..4].copy_from_slice(&evt_type.to_le_bytes());
        evt[4..8].copy_from_slice(&evt_data.to_le_bytes());
        snd.queue_event(evt_type, evt_data);

        let need_irq = snd.flush_eventq(&mut queue, &mut mem).unwrap();
        assert!(need_irq, "used entry should trigger an interrupt by default");

        assert!(snd.event_buffers.is_empty());
        assert!(snd.pending_events.is_empty());

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 4).unwrap(), 0);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);

        assert_eq!(mem.get_slice(buf, 8).unwrap(), &evt);
    }

    #[test]
    fn virtio_snd_eventq_flush_can_split_event_across_descriptors() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf0 = 0x4000;
        let buf1 = 0x5000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // Two 4-byte write-only descriptors chained together.
        write_desc(
            &mut mem,
            desc_table,
            0,
            buf0,
            4,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            1,
        );
        write_desc(&mut mem, desc_table, 1, buf1, 4, VIRTQ_DESC_F_WRITE, 0);
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        let mut evt = [0u8; 8];
        evt.copy_from_slice(&[
            0x00, 0x11, 0x00, 0x00, // type = 0x1100
            0x78, 0x56, 0x34, 0x12, // data = 0x12345678
        ]);
        snd.queue_event(0x1100, 0x12345678);

        snd.flush_eventq(&mut queue, &mut mem).unwrap();

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);

        assert_eq!(mem.get_slice(buf0, 4).unwrap(), &evt[0..4]);
        assert_eq!(mem.get_slice(buf1, 4).unwrap(), &evt[4..8]);
    }

    #[test]
    fn virtio_snd_eventq_flush_keeps_event_pending_when_buffer_too_small() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf_small = 0x4000;
        let buf_full = 0x5000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // Initialise rings (flags/idx).
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        // Post a too-small (4-byte) write-only buffer.
        mem.write(buf_small, &[0xAAu8; 4]).unwrap();
        write_desc(&mut mem, desc_table, 0, buf_small, 4, VIRTQ_DESC_F_WRITE, 0);
        write_u16_le(&mut mem, avail + 4, 0).unwrap(); // avail.ring[0] = desc 0
        write_u16_le(&mut mem, avail + 2, 1).unwrap(); // avail.idx = 1

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        let evt = [1u8, 2, 3, 4, 5, 6, 7, 8];
        snd.queue_event(0x04030201, 0x08070605);

        snd.flush_eventq(&mut queue, &mut mem).unwrap();

        // The small buffer should be completed with 0 bytes and the event should remain pending.
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 0);
        assert_eq!(mem.get_slice(buf_small, 4).unwrap(), &[0xAAu8; 4]);
        assert_eq!(snd.pending_events.len(), 1);
        assert!(snd.event_buffers.is_empty());

        // Now post a full-sized (8-byte) buffer and ensure the pending event is delivered.
        mem.write(buf_full, &[0u8; 8]).unwrap();
        write_desc(&mut mem, desc_table, 1, buf_full, 8, VIRTQ_DESC_F_WRITE, 0);
        write_u16_le(&mut mem, avail + 6, 1).unwrap(); // avail.ring[1] = desc 1
        write_u16_le(&mut mem, avail + 2, 2).unwrap(); // avail.idx = 2

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        snd.flush_eventq(&mut queue, &mut mem).unwrap();

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 2);
        assert_eq!(read_u32_le(&mem, used + 12).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 16).unwrap(), 8);
        assert_eq!(mem.get_slice(buf_full, 8).unwrap(), &evt);
        assert!(snd.pending_events.is_empty());
    }

    #[test]
    fn virtio_snd_eventq_flush_keeps_event_pending_when_guest_address_is_invalid() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 1u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        // Descriptor points 4 bytes before the end of guest RAM but claims 8 bytes.
        let oob_addr = 0x10000 - 4;
        write_desc(&mut mem, desc_table, 0, oob_addr, 8, VIRTQ_DESC_F_WRITE, 0);

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        snd.queue_event(0x04030201, 0x08070605);

        snd.flush_eventq(&mut queue, &mut mem).unwrap();

        // The invalid buffer should be completed with 0 bytes and the event should remain pending.
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 0);
        assert_eq!(snd.pending_events.len(), 1);
    }

    #[test]
    fn virtio_snd_eventq_flush_ignores_zero_length_descriptors() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let buf = 0x4000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        // Descriptor 0 is zero-length and uses an out-of-bounds address. A robust device should not
        // attempt to map it.
        write_desc(
            &mut mem,
            desc_table,
            0,
            0xFFFF_FFFFu64,
            0,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            1,
        );
        write_desc(&mut mem, desc_table, 1, buf, 8, VIRTQ_DESC_F_WRITE, 0);

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.event_buffers.push_back(chain);

        let evt = [1u8, 2, 3, 4, 5, 6, 7, 8];
        snd.queue_event(0x04030201, 0x08070605);

        snd.flush_eventq(&mut queue, &mut mem).unwrap();

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 4).unwrap(), 0);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);
        assert_eq!(mem.get_slice(buf, 8).unwrap(), &evt);
        assert!(snd.pending_events.is_empty());
    }

    #[test]
    fn virtio_snd_pending_events_are_bounded() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));

        // Queue more events than the cap to ensure we never grow unbounded.
        for i in 0..(MAX_PENDING_EVENTS + 10) {
            snd.queue_event(VIRTIO_SND_EVT_PCM_PERIOD_ELAPSED, i as u32);
        }

        assert_eq!(snd.pending_events.len(), MAX_PENDING_EVENTS);

        // Oldest events should have been dropped; the first remaining should be data=10.
        let first = snd.pending_events.front().unwrap();
        let data = u32::from_le_bytes(first[4..8].try_into().unwrap());
        assert_eq!(data, 10);
    }

    #[test]
    fn virtio_snd_write_all_in_skips_zero_length_descriptors() {
        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;
        let out_addr = 0x4000;
        let valid_in_addr = 0x5000;
        let oob_in_addr = 0x10000 + 1;

        write_bytes(&mut mem, out_addr, &[1, 2, 3, 4]);

        // OUT request, then a zero-length IN descriptor with an out-of-bounds address, followed by
        // a valid IN descriptor for the response.
        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (out_addr, 4, false),
                (oob_in_addr, 0, true),
                (valid_in_addr, 4, true),
            ],
        );

        let resp = [0xAA, 0xBB, 0xCC, 0xDD];
        let written = write_all_in(&mut mem, &chain, &resp);
        assert_eq!(written, resp.len() as u32);
        assert_eq!(mem.get_slice(valid_in_addr, resp.len()).unwrap(), &resp);
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

    #[test]
    fn event_buffer_queue_is_bounded() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        let mut mem = GuestRam::new(0x10000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        for i in 0..qsize {
            let buf_addr = 0x4000 + u64::from(i) * 0x100;
            write_desc(&mut mem, desc_table, i, buf_addr, 8, VIRTQ_DESC_F_WRITE, 0);
        }

        // Malicious: claim there are 1000 available entries, but only provide `qsize` ring slots.
        let avail_idx = 1000u16;
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, avail_idx).unwrap();
        for i in 0..qsize {
            write_u16_le(&mut mem, avail + 4 + u64::from(i) * 2, i).unwrap();
        }
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        for _ in 0..avail_idx {
            let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
                PoppedDescriptorChain::Chain(chain) => chain,
                PoppedDescriptorChain::Invalid { error, .. } => {
                    panic!("unexpected descriptor chain parse error: {error:?}")
                }
            };
            snd.process_queue(VIRTIO_SND_QUEUE_EVENT, chain, &mut queue, &mut mem)
                .unwrap();
        }

        assert_eq!(snd.event_buffers.len(), qsize as usize);
        assert_eq!(
            read_u16_le(&mem, used + 2).unwrap(),
            avail_idx - qsize,
            "extra event buffers should be completed with used.len=0 once the internal queue is full"
        );
    }

    #[test]
    fn tx_rejects_payloads_over_max_pcm_xfer_bytes() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        snd.playback.state = StreamState::Running;

        let mut mem = GuestRam::new(0x100000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x9000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        mem.write(header_addr, &hdr).unwrap();

        let oversize = (MAX_PCM_XFER_BYTES + 2) as u32;
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload_addr,
            oversize,
            VIRTQ_DESC_F_NEXT,
            2,
        );
        write_desc(&mut mem, desc_table, 2, resp_addr, 8, VIRTQ_DESC_F_WRITE, 0);

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };

        snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
            .unwrap();

        let resp = mem.get_slice(resp_addr, 8).unwrap();
        assert_eq!(
            u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            VIRTIO_SND_S_BAD_MSG
        );
    }

    #[test]
    fn rx_rejects_payloads_over_max_pcm_xfer_bytes() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        snd.capture.state = StreamState::Running;

        let mut mem = GuestRam::new(0x100000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let header_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = payload_addr + MAX_PCM_XFER_BYTES + 0x100;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        mem.write(header_addr, &hdr).unwrap();

        let oversize = (MAX_PCM_XFER_BYTES + 2) as u32;
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload_addr,
            oversize,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(&mut mem, desc_table, 2, resp_addr, 8, VIRTQ_DESC_F_WRITE, 0);

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = match queue.pop_descriptor_chain(&mem).unwrap().unwrap() {
            PoppedDescriptorChain::Chain(chain) => chain,
            PoppedDescriptorChain::Invalid { error, .. } => {
                panic!("unexpected descriptor chain parse error: {error:?}")
            }
        };

        snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
            .unwrap();

        let resp = mem.get_slice(resp_addr, 8).unwrap();
        assert_eq!(
            u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            VIRTIO_SND_S_BAD_MSG
        );
    }

    #[test]
    fn tx_rejects_chain_with_write_only_descriptor_before_payload() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;
        let payload_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);
        // One stereo frame, values don't matter for this ordering test.
        write_bytes(&mut mem, payload_addr, &[0x00, 0x00, 0x00, 0x00]);

        // Invalid ordering: OUT hdr, IN resp, OUT payload.
        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (resp_addr, 8, true),
                (payload_addr, 4, false),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(
            snd.output_mut().available_frames(),
            0,
            "invalid TX chains must not enqueue host audio"
        );
    }

    #[test]
    fn tx_rejects_payload_larger_than_max_pcm_xfer_bytes() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        // Allocate enough guest RAM to cover the oversized payload descriptor. The TX handler should
        // reject this chain *before* attempting to map the full payload slice, but keeping the
        // memory region valid makes the test resilient to future refactors.
        let mut mem = GuestRam::new(0x50000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x9000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        let payload_len = (MAX_PCM_XFER_BYTES + 1) as u32;
        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload_len, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(
            snd.output_mut().available_frames(),
            0,
            "oversized TX payloads must not enqueue host audio"
        );
    }

    #[test]
    fn tx_rejects_oversize_payload_without_mapping_guest_memory() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        // The chain is built from real guest RAM (needed for virtqueue parsing), but the TX handler
        // is invoked with a GuestMemory impl that panics on any access. This ensures the oversize
        // rejection path returns before calling `get_slice`.
        let chain = {
            let mut mem = GuestRam::new(0x10000);
            let hdr_addr = 0x4000;
            let payload_addr = 0x5000;
            let resp_addr = 0x6000;
            build_chain(
                &mut mem,
                desc_table,
                avail,
                used,
                &[
                    (hdr_addr, 8, false),
                    (payload_addr, (MAX_PCM_XFER_BYTES + 1) as u32, false),
                    (resp_addr, 8, true),
                ],
            )
        };

        let mut panic_mem = PanicGuestMemory;
        let status = snd.handle_tx_chain(&mut panic_mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_returns_io_err_when_playback_stream_is_not_running() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_prepared(&mut snd);
        assert_eq!(snd.playback.state, StreamState::Prepared);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[(hdr_addr, 8, false), (resp_addr, 8, true)],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_IO_ERR);
        assert_eq!(
            snd.output_mut().available_frames(),
            0,
            "non-running playback streams must not enqueue host audio"
        );
    }

    #[test]
    fn tx_header_only_is_ok_and_enqueues_no_audio() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[(hdr_addr, 8, false), (resp_addr, 8, true)],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_decodes_i16_extremes_within_expected_f32_range() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // One frame: L=i16::MIN (-32768 -> -1.0), R=i16::MAX (32767 -> 0.9999695...).
        let mut payload = [0u8; 4];
        payload[0..2].copy_from_slice(&i16::MIN.to_le_bytes());
        payload[2..4].copy_from_slice(&i16::MAX.to_le_bytes());
        write_bytes(&mut mem, payload_addr, &payload);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let frames = snd.output_mut().available_frames();
        assert_eq!(frames, 1);
        let samples = snd.output_mut().pop_interleaved_stereo(frames);
        assert_eq!(samples.len(), 2);

        let expected_left = -1.0f32;
        let expected_right = (i16::MAX as f32) / 32768.0;
        assert!(
            (samples[0] - expected_left).abs() <= 1e-6,
            "left sample expected {expected_left}, got {}",
            samples[0]
        );
        assert!(
            (samples[1] - expected_right).abs() <= 1e-6,
            "right sample expected {expected_right}, got {}",
            samples[1]
        );
        for &s in &samples {
            assert!(
                (-1.0..=1.0).contains(&s),
                "decoded sample must be within [-1, 1], got {s}"
            );
        }
    }

    #[test]
    fn tx_process_queue_writes_pcm_status_ok_into_response_descriptor() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        write_desc(
            &mut mem,
            desc_table,
            0,
            hdr_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            resp_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
            .unwrap();

        let expected = virtio_snd_pcm_status(VIRTIO_SND_S_OK, 0);
        assert_eq!(mem.get_slice(resp_addr, 8).unwrap(), &expected);

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);

        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_process_queue_writes_pcm_status_bad_msg_for_bad_payload() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // Incomplete stereo frame: only one i16 sample.
        write_bytes(&mut mem, payload_addr, &16384i16.to_le_bytes());

        write_desc(
            &mut mem,
            desc_table,
            0,
            hdr_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload_addr,
            2,
            VIRTQ_DESC_F_NEXT,
            2,
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            resp_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
            .unwrap();

        let expected = virtio_snd_pcm_status(VIRTIO_SND_S_BAD_MSG, 0);
        assert_eq!(mem.get_slice(resp_addr, 8).unwrap(), &expected);

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_process_queue_writes_pcm_status_io_err_when_stream_not_running() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_prepared(&mut snd);
        assert_eq!(snd.playback.state, StreamState::Prepared);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        write_desc(
            &mut mem,
            desc_table,
            0,
            hdr_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            resp_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
            .unwrap();

        let expected = virtio_snd_pcm_status(VIRTIO_SND_S_IO_ERR, 0);
        assert_eq!(mem.get_slice(resp_addr, 8).unwrap(), &expected);

        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 8);

        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_valid_running_writes_non_silent_audio_into_sink() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);
        assert_eq!(snd.playback.state, StreamState::Running);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // Two stereo frames:
        //   frame0: L=0.5,  R=0.25
        //   frame1: L=-0.5, R=-0.25
        let payload: [u8; 8] = {
            let mut out = [0u8; 8];
            out[0..2].copy_from_slice(&16384i16.to_le_bytes());
            out[2..4].copy_from_slice(&8192i16.to_le_bytes());
            out[4..6].copy_from_slice(&(-16384i16).to_le_bytes());
            out[6..8].copy_from_slice(&(-8192i16).to_le_bytes());
            out
        };
        write_bytes(&mut mem, payload_addr, &payload);

        let before = snd.output_mut().available_frames();
        assert_eq!(before, 0);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let after = snd.output_mut().available_frames();
        assert!(after > before, "TX should enqueue decoded audio frames");

        let samples = snd.output_mut().pop_interleaved_stereo(after);
        assert_eq!(samples.len(), after * 2);
        assert!(
            samples.iter().any(|&s| s != 0.0),
            "decoded audio should not be silent"
        );
        for &s in &samples {
            assert!(
                s >= -1.0 && s <= 1.0,
                "decoded sample must be in [-1, 1], got {s}"
            );
        }

        let expected = [0.5, 0.25, -0.5, -0.25];
        assert!(
            samples.len() >= expected.len(),
            "expected at least {} samples, got {}",
            expected.len(),
            samples.len()
        );
        for (idx, (&actual, &exp)) in samples.iter().zip(&expected).enumerate() {
            let diff = (actual - exp).abs();
            assert!(
                diff <= 1e-6,
                "sample[{idx}] expected {exp}, got {actual} (|diff|={diff})"
            );
        }
    }

    #[test]
    fn tx_resamples_to_host_rate_and_preserves_stereo_interleaving() {
        // Pick a host rate different from the guest contract rate so the linear resampler path is
        // exercised.
        let mut snd = VirtioSnd::new_with_host_sample_rate(
            aero_audio::ring::AudioRingBuffer::new_stereo(16),
            96_000,
        );
        assert_eq!(snd.host_sample_rate_hz(), 96_000);
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // Two identical stereo frames so resampling output remains stable and easy to assert.
        // L=0.5, R=-0.25
        let payload: [u8; 8] = {
            let mut out = [0u8; 8];
            out[0..2].copy_from_slice(&16384i16.to_le_bytes());
            out[2..4].copy_from_slice(&(-8192i16).to_le_bytes());
            out[4..6].copy_from_slice(&16384i16.to_le_bytes());
            out[6..8].copy_from_slice(&(-8192i16).to_le_bytes());
            out
        };
        write_bytes(&mut mem, payload_addr, &payload);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let frames = snd.output_mut().available_frames();
        assert!(frames > 0, "resampled TX should enqueue at least one frame");

        let samples = snd.output_mut().pop_interleaved_stereo(frames);
        assert_eq!(samples.len(), frames * 2);

        // All produced frames should preserve the constant stereo values.
        for (i, chunk) in samples.chunks_exact(2).enumerate() {
            let l = chunk[0];
            let r = chunk[1];
            assert!(
                (l - 0.5).abs() <= 1e-6,
                "frame[{i}] left expected 0.5, got {l}"
            );
            assert!(
                (r - -0.25).abs() <= 1e-6,
                "frame[{i}] right expected -0.25, got {r}"
            );
        }
    }

    #[test]
    fn tx_rejects_non_playback_stream_id() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[(hdr_addr, 8, false), (resp_addr, 8, true)],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_rejects_payload_with_odd_byte_count() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // One stereo frame plus an extra trailing byte (odd-length payload).
        // L=0.5, R=-0.25
        let payload: [u8; 5] = [0x00, 0x40, 0x00, 0xE0, 0xAA];
        write_bytes(&mut mem, payload_addr, &payload);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_rejects_payload_with_incomplete_stereo_frame() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let payload_addr = 0x5000;
        let resp_addr = 0x6000;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // Only one i16 sample (missing the right channel).
        let payload: [u8; 2] = 16384i16.to_le_bytes();
        write_bytes(&mut mem, payload_addr, &payload);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (payload_addr, payload.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_rejects_payload_when_payload_descriptor_is_out_of_bounds() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;
        let invalid_payload_addr = mem.len() - 2;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        write_bytes(&mut mem, hdr_addr, &hdr);

        // Payload descriptor extends beyond the end of guest memory.
        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_addr, 8, false),
                (invalid_payload_addr, 4, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_rejects_when_header_too_short() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_addr = 0x4000;
        let resp_addr = 0x5000;

        // Only 7 bytes total (< 8-byte header).
        write_bytes(&mut mem, hdr_addr, &[0u8; 7]);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[(hdr_addr, 7, false), (resp_addr, 8, true)],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_rejects_when_guest_memory_read_fails() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        // Point the readable header descriptor beyond the end of guest memory so `get_slice` fails.
        let invalid_hdr_addr = mem.len() - 4;
        let resp_addr = 0x5000;

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[(invalid_hdr_addr, 8, false), (resp_addr, 8, true)],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_BAD_MSG);
        assert_eq!(snd.output_mut().available_frames(), 0);
    }

    #[test]
    fn tx_accepts_header_split_across_descriptors() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_part1_addr = 0x4000;
        let hdr_part2_and_payload_addr = 0x5000;
        let resp_addr = 0x6000;

        // Split header across two descriptors:
        // - first 6 bytes: stream_id + first 2 reserved bytes
        // - remaining 2 reserved bytes + one stereo frame payload
        let stream_id = PLAYBACK_STREAM_ID.to_le_bytes();
        write_bytes(
            &mut mem,
            hdr_part1_addr,
            &[stream_id[0], stream_id[1], stream_id[2], stream_id[3], 0, 0],
        );
        // Remaining header bytes are 0,0; payload is L=0.5 (0x4000), R=-0.25 (0xE000).
        write_bytes(
            &mut mem,
            hdr_part2_and_payload_addr,
            &[0, 0, 0x00, 0x40, 0x00, 0xE0],
        );

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_part1_addr, 6, false),
                (hdr_part2_and_payload_addr, 6, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let frames = snd.output_mut().available_frames();
        assert_eq!(frames, 1);
        let samples = snd.output_mut().pop_interleaved_stereo(frames);
        assert_eq!(samples, vec![0.5, -0.25]);
    }

    #[test]
    fn tx_parses_pcm_samples_split_across_descriptor_boundary() {
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_and_one_payload_byte_addr = 0x4000;
        let payload_rest_addr = 0x5000;
        let resp_addr = 0x6000;

        // Descriptor 0: full 8-byte header + one payload byte (low byte of left sample).
        let mut first = [0u8; 9];
        first[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        // reserved [4..8] = 0
        first[8] = 0x00; // left sample lo byte for 0x4000 (0.5)
        write_bytes(&mut mem, hdr_and_one_payload_byte_addr, &first);

        // Descriptor 1: remaining payload bytes: left hi, right lo, right hi.
        // Left = 0.5 (0x4000), Right = -0.25 (0xE000).
        let rest: [u8; 3] = [0x40, 0x00, 0xE0];
        write_bytes(&mut mem, payload_rest_addr, &rest);

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_and_one_payload_byte_addr, first.len() as u32, false),
                (payload_rest_addr, rest.len() as u32, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let frames = snd.output_mut().available_frames();
        assert_eq!(frames, 1);
        let samples = snd.output_mut().pop_interleaved_stereo(frames);
        assert_eq!(samples, vec![0.5, -0.25]);
    }

    #[test]
    fn tx_parses_stereo_frame_split_between_descriptors() {
        // Specifically exercise `pending_left` crossing a descriptor boundary by placing the left
        // channel sample in one descriptor and the right channel sample in the next.
        let mut snd = VirtioSnd::new(aero_audio::ring::AudioRingBuffer::new_stereo(8));
        drive_playback_to_running(&mut snd);

        let mut mem = GuestRam::new(0x10000);
        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let hdr_plus_left_addr = 0x4000;
        let right_addr = 0x5000;
        let resp_addr = 0x6000;

        // Descriptor 0: full 8-byte header + the left channel sample (0.5 -> 0x4000).
        let mut first = [0u8; 10];
        first[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        // reserved [4..8] = 0
        first[8..10].copy_from_slice(&16384i16.to_le_bytes());
        write_bytes(&mut mem, hdr_plus_left_addr, &first);

        // Descriptor 1: right channel sample (-0.25 -> 0xE000).
        write_bytes(&mut mem, right_addr, &(-8192i16).to_le_bytes());

        let chain = build_chain(
            &mut mem,
            desc_table,
            avail,
            used,
            &[
                (hdr_plus_left_addr, first.len() as u32, false),
                (right_addr, 2, false),
                (resp_addr, 8, true),
            ],
        );

        let status = snd.handle_tx_chain(&mut mem, &chain);
        assert_eq!(status, VIRTIO_SND_S_OK);

        let frames = snd.output_mut().available_frames();
        assert_eq!(frames, 1);
        let samples = snd.output_mut().pop_interleaved_stereo(frames);
        assert_eq!(samples, vec![0.5, -0.25]);
    }

    #[test]
    fn rx_returns_io_err_when_capture_stream_not_running_and_zeros_payload() {
        let mut snd = VirtioSnd::new_with_capture(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
            TestCaptureSource::default(),
        );

        // Valid RX chain header, but stream not running.
        assert_ne!(snd.capture.state, StreamState::Running);

        let mut mem = GuestRam::new(0x20000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let header_addr = 0x4000;
        let payload_addr = 0x4100;
        let status_addr = 0x4200;

        // Header: stream_id + reserved
        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        mem.get_slice_mut(header_addr, 8)
            .unwrap()
            .copy_from_slice(&hdr);

        // Fill payload/status with non-zero bytes so we can assert deterministic writes.
        mem.get_slice_mut(payload_addr, 8).unwrap().fill(0xAA);
        mem.get_slice_mut(status_addr, 8).unwrap().fill(0xBB);

        // 0: OUT header, 1: IN payload, 2: IN status
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload_addr,
            8,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            status_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        // Submit chain head=0.
        write_u16_le(&mut mem, avail, 0).unwrap(); // flags
        write_u16_le(&mut mem, avail + 2, 1).unwrap(); // idx
        write_u16_le(&mut mem, avail + 4, 0).unwrap(); // ring[0]
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
            .unwrap();

        let status_code = read_status_code(&mem, status_addr);
        assert_eq!(status_code, VIRTIO_SND_S_IO_ERR);
        assert_eq!(read_u32_le(&mem, status_addr + 4).unwrap(), 0);

        assert_eq!(mem.get_slice(payload_addr, 8).unwrap(), &[0u8; 8]);

        assert_eq!(snd.capture_source.read_calls, 0);
        assert_eq!(snd.capture_source.samples_read, 0);

        // Used element should report payload + status bytes written.
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 1);
        assert_eq!(read_u32_le(&mem, used + 8).unwrap(), 16);
    }

    #[test]
    fn rx_bad_chain_writes_silence_payload_and_reports_bad_msg() {
        let mut snd = VirtioSnd::new_with_capture(
            aero_audio::ring::AudioRingBuffer::new_stereo(8),
            TestCaptureSource::default(),
        );
        // Force the stream into Running so the error comes from chain parsing.
        snd.capture.state = StreamState::Running;

        let mut mem = GuestRam::new(0x20000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let header_addr = 0x4000;
        let extra_out_addr = 0x4010;
        let payload_addr = 0x4100;
        let status_addr = 0x4200;

        // Header: valid stream_id, but add an extra OUT descriptor to trigger `extra_out`.
        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        mem.get_slice_mut(header_addr, 8)
            .unwrap()
            .copy_from_slice(&hdr);
        mem.get_slice_mut(extra_out_addr, 1).unwrap()[0] = 0x99;

        mem.get_slice_mut(payload_addr, 8).unwrap().fill(0xCC);
        mem.get_slice_mut(status_addr, 8).unwrap().fill(0xDD);

        // 0: OUT header, 1: OUT extra byte (invalid), 2: IN payload, 3: IN status
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            extra_out_addr,
            1,
            VIRTQ_DESC_F_NEXT,
            2,
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            payload_addr,
            8,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            3,
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            status_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
            .unwrap();

        assert_eq!(read_status_code(&mem, status_addr), VIRTIO_SND_S_BAD_MSG);
        assert_eq!(mem.get_slice(payload_addr, 8).unwrap(), &[0u8; 8]);
        assert_eq!(snd.capture_source.read_calls, 0);
        assert_eq!(snd.capture_source.samples_read, 0);
    }

    #[test]
    fn rx_running_captures_expected_s16le_samples() {
        let mut source = TestCaptureSource::default();
        source.push_samples(&[-1.0, -0.5, 0.0, 1.0]);
        let mut snd =
            VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), source);
        snd.capture.state = StreamState::Running;

        let mut mem = GuestRam::new(0x20000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let header_addr = 0x4000;
        let payload1_addr = 0x4100;
        let payload2_addr = 0x4200;
        let status_addr = 0x4300;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        mem.get_slice_mut(header_addr, 8)
            .unwrap()
            .copy_from_slice(&hdr);

        mem.get_slice_mut(payload1_addr, 5).unwrap().fill(0xAA);
        mem.get_slice_mut(payload2_addr, 3).unwrap().fill(0xAA);
        mem.get_slice_mut(status_addr, 8).unwrap().fill(0xAA);

        // 0: OUT header, 1-2: IN payload, 3: IN status
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload1_addr,
            5,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            payload2_addr,
            3,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            3,
        );
        write_desc(
            &mut mem,
            desc_table,
            3,
            status_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
            .unwrap();

        assert_eq!(read_status_code(&mem, status_addr), VIRTIO_SND_S_OK);

        let expected: [u8; 8] = [
            0x00, 0x80, // -1.0 -> -32768
            0x00, 0xC0, // -0.5 -> -16384
            0x00, 0x00, // 0.0 -> 0
            0xFF, 0x7F, // 1.0 -> 32767
        ];
        assert_eq!(mem.get_slice(payload1_addr, 5).unwrap(), &expected[..5]);
        assert_eq!(mem.get_slice(payload2_addr, 3).unwrap(), &expected[5..]);

        assert_eq!(snd.capture_source.read_calls, 1);
        assert_eq!(snd.capture_source.last_requested, Some(4));
        assert_eq!(snd.capture_source.samples_read, 4);
        assert_eq!(snd.capture_source.remaining_samples(), 0);
        assert_eq!(snd.capture_telemetry, CaptureTelemetry::default());
    }

    #[test]
    fn rx_underrun_zero_fills_and_increments_telemetry() {
        let mut source = TestCaptureSource::default();
        source.push_samples(&[0.25, -0.25]);
        let mut snd =
            VirtioSnd::new_with_capture(aero_audio::ring::AudioRingBuffer::new_stereo(8), source);
        snd.capture.state = StreamState::Running;

        let mut mem = GuestRam::new(0x20000);

        let desc_table = 0x1000;
        let avail = 0x2000;
        let used = 0x3000;

        let qsize = 8u16;
        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: qsize,
                desc_addr: desc_table,
                avail_addr: avail,
                used_addr: used,
            },
            false,
        )
        .unwrap();

        let header_addr = 0x4000;
        let payload_addr = 0x4100;
        let status_addr = 0x4200;

        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
        mem.get_slice_mut(header_addr, 8)
            .unwrap()
            .copy_from_slice(&hdr);

        mem.get_slice_mut(payload_addr, 10).unwrap().fill(0xAB);
        mem.get_slice_mut(status_addr, 8).unwrap().fill(0xAB);

        // Need 5 samples (10 bytes), but only provide 2 -> underrun of 3.
        write_desc(
            &mut mem,
            desc_table,
            0,
            header_addr,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut mem,
            desc_table,
            1,
            payload_addr,
            10,
            VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
            2,
        );
        write_desc(
            &mut mem,
            desc_table,
            2,
            status_addr,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, 1).unwrap();
        write_u16_le(&mut mem, avail + 4, 0).unwrap();
        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        let chain = pop_chain(&mut queue, &mem);
        snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
            .unwrap();

        assert_eq!(read_status_code(&mem, status_addr), VIRTIO_SND_S_OK);

        let expected: [u8; 10] = [
            0x00, 0x20, // 0.25 -> 8192
            0x00, 0xE0, // -0.25 -> -8192
            0x00, 0x00, // silence (underrun)
            0x00, 0x00, // silence
            0x00, 0x00, // silence
        ];
        assert_eq!(mem.get_slice(payload_addr, 10).unwrap(), &expected);

        assert_eq!(snd.capture_source.read_calls, 1);
        assert_eq!(snd.capture_source.last_requested, Some(5));
        assert_eq!(snd.capture_source.samples_read, 2);
        assert_eq!(snd.capture_source.remaining_samples(), 0);

        assert_eq!(
            snd.capture_telemetry,
            CaptureTelemetry {
                dropped_samples: 0,
                underrun_samples: 3,
                underrun_responses: 1,
            }
        );
    }

    #[test]
    fn playback_stop_clears_resampler_state_to_avoid_stale_audio() {
        // Exercise both upsampling and downsampling resampler paths. Both can leave queued source
        // frames/fractional position, which must be cleared on STOP.
        for host_rate_hz in [96_000u32, 44_100u32] {
            let mut snd = VirtioSnd::new_with_host_sample_rate(
                aero_audio::ring::AudioRingBuffer::new_stereo(32),
                host_rate_hz,
            );
            let host_rate_hz = snd.host_sample_rate_hz();
            assert_ne!(host_rate_hz, PCM_SAMPLE_RATE_HZ);

            control_set_params(&mut snd, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID);

            let mut mem = GuestRam::new(0x10000);
            let desc_table = 0x1000;
            let avail = 0x2000;
            let used = 0x3000;

            let qsize = 8u16;
            let mut queue = VirtQueue::new(
                VirtQueueConfig {
                    size: qsize,
                    desc_addr: desc_table,
                    avail_addr: avail,
                    used_addr: used,
                },
                false,
            )
            .unwrap();

            let req1_addr = 0x4000;
            let resp1_addr = 0x5000;
            let req2_addr = 0x6000;
            let resp2_addr = 0x7000;

            // First TX: two non-zero stereo frames (ensures downsampling can leave a queued frame).
            let sample = i16::MAX;
            let mut tx1 = Vec::new();
            tx1.extend_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
            tx1.extend_from_slice(&0u32.to_le_bytes());
            for _ in 0..2 {
                tx1.extend_from_slice(&sample.to_le_bytes());
                tx1.extend_from_slice(&sample.to_le_bytes());
            }
            mem.write(req1_addr, &tx1).unwrap();

            // Second TX: one silent stereo frame. If the resampler still has a queued non-zero
            // frame from before STOP, the first output frame after restart will be non-zero due to
            // interpolation.
            let mut tx2 = Vec::new();
            tx2.extend_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
            tx2.extend_from_slice(&0u32.to_le_bytes());
            tx2.extend_from_slice(&0i16.to_le_bytes());
            tx2.extend_from_slice(&0i16.to_le_bytes());
            mem.write(req2_addr, &tx2).unwrap();

            // Two descriptor chains: [0,1] then [2,3].
            write_desc(
                &mut mem,
                desc_table,
                0,
                req1_addr,
                tx1.len() as u32,
                VIRTQ_DESC_F_NEXT,
                1,
            );
            write_desc(
                &mut mem,
                desc_table,
                1,
                resp1_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );
            write_desc(
                &mut mem,
                desc_table,
                2,
                req2_addr,
                tx2.len() as u32,
                VIRTQ_DESC_F_NEXT,
                3,
            );
            write_desc(
                &mut mem,
                desc_table,
                3,
                resp2_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            write_u16_le(&mut mem, avail, 0).unwrap();
            write_u16_le(&mut mem, avail + 2, 2).unwrap(); // two available chains
            write_u16_le(&mut mem, avail + 4, 0).unwrap();
            write_u16_le(&mut mem, avail + 6, 2).unwrap();
            write_u16_le(&mut mem, used, 0).unwrap();
            write_u16_le(&mut mem, used + 2, 0).unwrap();

            // Process the first TX.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
                .unwrap();

            let frames = snd.output_mut().available_frames();
            let first_out = snd.output_mut().pop_interleaved_stereo(frames);
            assert!(
                first_out.iter().any(|&s| s.abs() > 1e-6),
                "first TX should produce non-zero audio (host_rate_hz={host_rate_hz})"
            );
            assert!(
                snd.resampler.queued_source_frames() > 0,
                "precondition: first TX should leave queued resampler state (host_rate_hz={host_rate_hz})"
            );
            assert!(
                snd.resampler.required_source_frames(1) > 1,
                "precondition: first TX should leave fractional resampler state (host_rate_hz={host_rate_hz})"
            );

            // Stop and restart the playback stream without providing any new non-zero audio.
            control_simple(&mut snd, VIRTIO_SND_R_PCM_STOP, PLAYBACK_STREAM_ID);
            assert_eq!(snd.playback.state, StreamState::Prepared);
            assert!(
                snd.playback.params.is_some(),
                "PCM_STOP should preserve playback params"
            );
            assert_eq!(
                snd.resampler.queued_source_frames(),
                0,
                "PCM_STOP should clear queued playback resampler frames"
            );
            assert!(
                snd.decoded_frames_scratch.is_empty() && snd.resampled_scratch.is_empty(),
                "PCM_STOP should clear playback scratch buffers"
            );
            assert_eq!(
                snd.resampler.src_rate_hz(),
                PCM_SAMPLE_RATE_HZ,
                "PCM_STOP should preserve playback resampler src rate"
            );
            assert_eq!(
                snd.resampler.dst_rate_hz(),
                host_rate_hz,
                "PCM_STOP should preserve playback resampler dst rate"
            );
            assert_eq!(
                snd.resampler.required_source_frames(1),
                1,
                "PCM_STOP should reset playback resampler fractional position"
            );
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID);

            // Process the second TX and ensure the queued frame from the first TX was not replayed.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
                .unwrap();

            let frames = snd.output_mut().available_frames();
            assert!(frames > 0, "second TX should produce at least one frame");
            let out = snd.output_mut().pop_interleaved_stereo(frames);
            assert!(
                out.iter().all(|&s| s.abs() <= 1e-6),
                "audio after STOP/START should not contain stale queued samples (host_rate_hz={host_rate_hz})"
            );
        }
    }

    #[test]
    fn capture_stop_clears_resampler_state_to_avoid_stale_samples() {
        // Exercise both upsampling and downsampling capture resampler paths.
        for capture_rate_hz in [44_100u32, 88_200u32] {
            let mut snd = VirtioSnd::new_with_capture(
                aero_audio::ring::AudioRingBuffer::new_stereo(32),
                TestCaptureSource::default(),
            );
            snd.set_capture_sample_rate_hz(capture_rate_hz);
            let capture_rate_hz = snd.capture_sample_rate_hz();
            assert_ne!(capture_rate_hz, PCM_SAMPLE_RATE_HZ);

            control_set_params(&mut snd, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, CAPTURE_STREAM_ID);

            let samples_needed = 10usize;
            let required_src = snd.capture_resampler.required_source_frames(samples_needed);
            let src_samples = vec![1.0f32; required_src];
            snd.capture_source.push_samples(&src_samples);

            let payload_bytes = samples_needed * 2;

            let mut mem = GuestRam::new(0x10000);
            let desc_table = 0x1000;
            let avail = 0x2000;
            let used = 0x3000;

            let qsize = 8u16;
            let mut queue = VirtQueue::new(
                VirtQueueConfig {
                    size: qsize,
                    desc_addr: desc_table,
                    avail_addr: avail,
                    used_addr: used,
                },
                false,
            )
            .unwrap();

            let hdr1_addr = 0x4000;
            let payload1_addr = 0x5000;
            let resp1_addr = 0x5800;
            let hdr2_addr = 0x6000;
            let payload2_addr = 0x7000;
            let resp2_addr = 0x7800;

            let mut hdr = [0u8; 8];
            hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
            mem.get_slice_mut(hdr1_addr, 8)
                .unwrap()
                .copy_from_slice(&hdr);
            mem.get_slice_mut(hdr2_addr, 8)
                .unwrap()
                .copy_from_slice(&hdr);

            // Two RX descriptor chains: [0,1,2] then [3,4,5].
            write_desc(&mut mem, desc_table, 0, hdr1_addr, 8, VIRTQ_DESC_F_NEXT, 1);
            write_desc(
                &mut mem,
                desc_table,
                1,
                payload1_addr,
                payload_bytes as u32,
                VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                2,
            );
            write_desc(
                &mut mem,
                desc_table,
                2,
                resp1_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            write_desc(&mut mem, desc_table, 3, hdr2_addr, 8, VIRTQ_DESC_F_NEXT, 4);
            write_desc(
                &mut mem,
                desc_table,
                4,
                payload2_addr,
                payload_bytes as u32,
                VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                5,
            );
            write_desc(
                &mut mem,
                desc_table,
                5,
                resp2_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            // Poison payload buffers so we can ensure the device writes deterministic output.
            mem.get_slice_mut(payload1_addr, payload_bytes)
                .unwrap()
                .fill(0xAA);
            mem.get_slice_mut(payload2_addr, payload_bytes)
                .unwrap()
                .fill(0xAA);

            write_u16_le(&mut mem, avail, 0).unwrap();
            write_u16_le(&mut mem, avail + 2, 2).unwrap();
            write_u16_le(&mut mem, avail + 4, 0).unwrap();
            write_u16_le(&mut mem, avail + 6, 3).unwrap();
            write_u16_le(&mut mem, used, 0).unwrap();
            write_u16_le(&mut mem, used + 2, 0).unwrap();

            // First RX consumes all queued non-zero mic samples.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
                .unwrap();

            assert_eq!(
                snd.capture_source.remaining_samples(),
                0,
                "capture source should be drained after first RX (capture_rate_hz={capture_rate_hz})"
            );
            assert!(
                snd.capture_resampler.required_source_frames(1) > 1,
                "precondition: first RX should leave fractional resampler state (capture_rate_hz={capture_rate_hz})"
            );
            assert!(
                !snd.capture_frames_scratch.is_empty()
                    && !snd.capture_interleaved_scratch.is_empty()
                    && !snd.capture_samples_scratch.is_empty(),
                "precondition: capture scratch buffers should contain resampler state"
            );

            // Stop and restart capture, then issue another RX request. With an empty capture
            // source, the response should be pure silence; any non-zero samples indicate stale
            // resampler state leaking across STOP/START.
            control_simple(&mut snd, VIRTIO_SND_R_PCM_STOP, CAPTURE_STREAM_ID);
            assert_eq!(snd.capture.state, StreamState::Prepared);
            assert!(
                snd.capture.params.is_some(),
                "PCM_STOP should preserve capture params"
            );
            assert_eq!(
                snd.capture_resampler.queued_source_frames(),
                0,
                "PCM_STOP should clear queued capture resampler frames"
            );
            assert!(
                snd.capture_frames_scratch.is_empty()
                    && snd.capture_interleaved_scratch.is_empty()
                    && snd.capture_samples_scratch.is_empty(),
                "PCM_STOP should clear capture scratch buffers"
            );
            assert_eq!(
                snd.capture_resampler.src_rate_hz(),
                capture_rate_hz,
                "PCM_STOP should preserve capture resampler src rate"
            );
            assert_eq!(
                snd.capture_resampler.dst_rate_hz(),
                PCM_SAMPLE_RATE_HZ,
                "PCM_STOP should preserve capture resampler dst rate"
            );
            assert_eq!(
                snd.capture_resampler.required_source_frames(1),
                1,
                "PCM_STOP should reset capture resampler fractional position"
            );
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, CAPTURE_STREAM_ID);

            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
                .unwrap();

            let payload2 = mem.get_slice(payload2_addr, payload_bytes).unwrap();
            assert!(
                payload2.iter().all(|&b| b == 0),
                "capture samples after STOP/START should not include stale queued audio (capture_rate_hz={capture_rate_hz})"
            );
        }
    }

    #[test]
    fn playback_release_clears_resampler_state_to_avoid_stale_audio() {
        // Exercise both upsampling and downsampling resampler paths. Both can leave queued source
        // frames/fractional position, which must be cleared on RELEASE.
        for host_rate_hz in [96_000u32, 44_100u32] {
            let mut snd = VirtioSnd::new_with_host_sample_rate(
                aero_audio::ring::AudioRingBuffer::new_stereo(32),
                host_rate_hz,
            );
            let host_rate_hz = snd.host_sample_rate_hz();
            assert_ne!(host_rate_hz, PCM_SAMPLE_RATE_HZ);

            control_set_params(&mut snd, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID);

            let mut mem = GuestRam::new(0x10000);
            let desc_table = 0x1000;
            let avail = 0x2000;
            let used = 0x3000;

            let qsize = 8u16;
            let mut queue = VirtQueue::new(
                VirtQueueConfig {
                    size: qsize,
                    desc_addr: desc_table,
                    avail_addr: avail,
                    used_addr: used,
                },
                false,
            )
            .unwrap();

            let req1_addr = 0x4000;
            let resp1_addr = 0x5000;
            let req2_addr = 0x6000;
            let resp2_addr = 0x7000;

            // First TX: two non-zero stereo frames (ensures downsampling can leave a queued frame).
            let sample = i16::MAX;
            let mut tx1 = Vec::new();
            tx1.extend_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
            tx1.extend_from_slice(&0u32.to_le_bytes());
            for _ in 0..2 {
                tx1.extend_from_slice(&sample.to_le_bytes());
                tx1.extend_from_slice(&sample.to_le_bytes());
            }
            mem.write(req1_addr, &tx1).unwrap();

            // Second TX: one silent stereo frame. Any non-zero output indicates stale queued
            // resampler state leaking across RELEASE.
            let mut tx2 = Vec::new();
            tx2.extend_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
            tx2.extend_from_slice(&0u32.to_le_bytes());
            tx2.extend_from_slice(&0i16.to_le_bytes());
            tx2.extend_from_slice(&0i16.to_le_bytes());
            mem.write(req2_addr, &tx2).unwrap();

            write_desc(
                &mut mem,
                desc_table,
                0,
                req1_addr,
                tx1.len() as u32,
                VIRTQ_DESC_F_NEXT,
                1,
            );
            write_desc(
                &mut mem,
                desc_table,
                1,
                resp1_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );
            write_desc(
                &mut mem,
                desc_table,
                2,
                req2_addr,
                tx2.len() as u32,
                VIRTQ_DESC_F_NEXT,
                3,
            );
            write_desc(
                &mut mem,
                desc_table,
                3,
                resp2_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            write_u16_le(&mut mem, avail, 0).unwrap();
            write_u16_le(&mut mem, avail + 2, 2).unwrap();
            write_u16_le(&mut mem, avail + 4, 0).unwrap();
            write_u16_le(&mut mem, avail + 6, 2).unwrap();
            write_u16_le(&mut mem, used, 0).unwrap();
            write_u16_le(&mut mem, used + 2, 0).unwrap();

            // Process the first TX and drain the ring buffer.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
                .unwrap();
            let frames = snd.output_mut().available_frames();
            let first_out = snd.output_mut().pop_interleaved_stereo(frames);
            assert!(
                first_out.iter().any(|&s| s.abs() > 1e-6),
                "first TX should produce non-zero audio (host_rate_hz={host_rate_hz})"
            );
            assert!(
                snd.resampler.queued_source_frames() > 0,
                "precondition: first TX should leave queued resampler state (host_rate_hz={host_rate_hz})"
            );
            assert!(
                snd.resampler.required_source_frames(1) > 1,
                "precondition: first TX should leave fractional resampler state (host_rate_hz={host_rate_hz})"
            );

            // Release and restart.
            control_simple(&mut snd, VIRTIO_SND_R_PCM_RELEASE, PLAYBACK_STREAM_ID);
            assert_eq!(snd.playback.state, StreamState::Idle);
            assert!(
                snd.playback.params.is_none(),
                "PCM_RELEASE should clear playback params"
            );
            assert_eq!(
                snd.resampler.queued_source_frames(),
                0,
                "PCM_RELEASE should clear queued playback resampler frames"
            );
            assert!(
                snd.decoded_frames_scratch.is_empty() && snd.resampled_scratch.is_empty(),
                "PCM_RELEASE should clear playback scratch buffers"
            );
            assert_eq!(
                snd.resampler.src_rate_hz(),
                PCM_SAMPLE_RATE_HZ,
                "PCM_RELEASE should preserve playback resampler src rate"
            );
            assert_eq!(
                snd.resampler.dst_rate_hz(),
                host_rate_hz,
                "PCM_RELEASE should preserve playback resampler dst rate"
            );
            assert_eq!(
                snd.resampler.required_source_frames(1),
                1,
                "PCM_RELEASE should reset playback resampler fractional position"
            );
            control_set_params(&mut snd, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, PLAYBACK_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, PLAYBACK_STREAM_ID);

            // Process the second TX. Output must be pure silence.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut mem)
                .unwrap();
            let frames = snd.output_mut().available_frames();
            assert!(frames > 0, "second TX should produce at least one frame");
            let out = snd.output_mut().pop_interleaved_stereo(frames);
            assert!(
                out.iter().all(|&s| s.abs() <= 1e-6),
                "audio after RELEASE/START should not contain stale queued samples (host_rate_hz={host_rate_hz})"
            );
        }
    }

    #[test]
    fn capture_release_clears_resampler_state_to_avoid_stale_samples() {
        // Exercise both upsampling and downsampling capture resampler paths, including the
        // RELEASE -> SET_PARAMS -> PREPARE -> START lifecycle.
        for capture_rate_hz in [44_100u32, 88_200u32] {
            let mut snd = VirtioSnd::new_with_capture(
                aero_audio::ring::AudioRingBuffer::new_stereo(32),
                TestCaptureSource::default(),
            );
            snd.set_capture_sample_rate_hz(capture_rate_hz);
            let capture_rate_hz = snd.capture_sample_rate_hz();
            assert_ne!(capture_rate_hz, PCM_SAMPLE_RATE_HZ);

            control_set_params(&mut snd, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, CAPTURE_STREAM_ID);

            let samples_needed = 10usize;
            let required_src = snd.capture_resampler.required_source_frames(samples_needed);
            snd.capture_source.push_samples(&vec![1.0f32; required_src]);

            let payload_bytes = samples_needed * 2;

            let mut mem = GuestRam::new(0x10000);
            let desc_table = 0x1000;
            let avail = 0x2000;
            let used = 0x3000;

            let qsize = 8u16;
            let mut queue = VirtQueue::new(
                VirtQueueConfig {
                    size: qsize,
                    desc_addr: desc_table,
                    avail_addr: avail,
                    used_addr: used,
                },
                false,
            )
            .unwrap();

            let hdr1_addr = 0x4000;
            let payload1_addr = 0x5000;
            let resp1_addr = 0x5800;
            let hdr2_addr = 0x6000;
            let payload2_addr = 0x7000;
            let resp2_addr = 0x7800;

            let mut hdr = [0u8; 8];
            hdr[0..4].copy_from_slice(&CAPTURE_STREAM_ID.to_le_bytes());
            mem.get_slice_mut(hdr1_addr, 8)
                .unwrap()
                .copy_from_slice(&hdr);
            mem.get_slice_mut(hdr2_addr, 8)
                .unwrap()
                .copy_from_slice(&hdr);

            write_desc(&mut mem, desc_table, 0, hdr1_addr, 8, VIRTQ_DESC_F_NEXT, 1);
            write_desc(
                &mut mem,
                desc_table,
                1,
                payload1_addr,
                payload_bytes as u32,
                VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                2,
            );
            write_desc(
                &mut mem,
                desc_table,
                2,
                resp1_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            write_desc(&mut mem, desc_table, 3, hdr2_addr, 8, VIRTQ_DESC_F_NEXT, 4);
            write_desc(
                &mut mem,
                desc_table,
                4,
                payload2_addr,
                payload_bytes as u32,
                VIRTQ_DESC_F_NEXT | VIRTQ_DESC_F_WRITE,
                5,
            );
            write_desc(
                &mut mem,
                desc_table,
                5,
                resp2_addr,
                8,
                VIRTQ_DESC_F_WRITE,
                0,
            );

            mem.get_slice_mut(payload1_addr, payload_bytes)
                .unwrap()
                .fill(0xAA);
            mem.get_slice_mut(payload2_addr, payload_bytes)
                .unwrap()
                .fill(0xAA);

            write_u16_le(&mut mem, avail, 0).unwrap();
            write_u16_le(&mut mem, avail + 2, 2).unwrap();
            write_u16_le(&mut mem, avail + 4, 0).unwrap();
            write_u16_le(&mut mem, avail + 6, 3).unwrap();
            write_u16_le(&mut mem, used, 0).unwrap();
            write_u16_le(&mut mem, used + 2, 0).unwrap();

            // First RX consumes the queued non-zero mic samples.
            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
                .unwrap();
            assert_eq!(
                snd.capture_source.remaining_samples(),
                0,
                "capture source should be drained after first RX (capture_rate_hz={capture_rate_hz})"
            );
            assert!(
                snd.capture_resampler.required_source_frames(1) > 1,
                "precondition: first RX should leave fractional resampler state (capture_rate_hz={capture_rate_hz})"
            );

            // Release and restart. With an empty capture source, the next RX response must be pure
            // silence.
            control_simple(&mut snd, VIRTIO_SND_R_PCM_RELEASE, CAPTURE_STREAM_ID);
            assert_eq!(snd.capture.state, StreamState::Idle);
            assert!(
                snd.capture.params.is_none(),
                "PCM_RELEASE should clear capture params"
            );
            assert_eq!(
                snd.capture_resampler.queued_source_frames(),
                0,
                "PCM_RELEASE should clear queued capture resampler frames"
            );
            assert!(
                snd.capture_frames_scratch.is_empty()
                    && snd.capture_interleaved_scratch.is_empty()
                    && snd.capture_samples_scratch.is_empty(),
                "PCM_RELEASE should clear capture scratch buffers"
            );
            assert_eq!(
                snd.capture_resampler.src_rate_hz(),
                capture_rate_hz,
                "PCM_RELEASE should preserve capture resampler src rate"
            );
            assert_eq!(
                snd.capture_resampler.dst_rate_hz(),
                PCM_SAMPLE_RATE_HZ,
                "PCM_RELEASE should preserve capture resampler dst rate"
            );
            assert_eq!(
                snd.capture_resampler.required_source_frames(1),
                1,
                "PCM_RELEASE should reset capture resampler fractional position"
            );
            control_set_params(&mut snd, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_PREPARE, CAPTURE_STREAM_ID);
            control_simple(&mut snd, VIRTIO_SND_R_PCM_START, CAPTURE_STREAM_ID);

            let chain = pop_chain(&mut queue, &mem);
            snd.process_queue(VIRTIO_SND_QUEUE_RX, chain, &mut queue, &mut mem)
                .unwrap();

            let payload2 = mem.get_slice(payload2_addr, payload_bytes).unwrap();
            assert!(
                payload2.iter().all(|&b| b == 0),
                "capture samples after RELEASE/START should not include stale queued audio (capture_rate_hz={capture_rate_hz})"
            );
        }
    }
}
