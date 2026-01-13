//! End-to-end browser demo: drive the real virtio-snd device model and stream its output
//! directly into the canonical Web Audio `AudioWorkletProcessor` ring buffer.
//!
//! This exists purely for the web demo harness + Playwright E2E tests; it is not intended to be a
//! stable public API.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::SharedArrayBuffer;

use aero_audio::sink::AudioSink;
use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_virtio::devices::VirtioDevice;
use aero_virtio::devices::snd::{
    PCM_SAMPLE_RATE_HZ, PLAYBACK_STREAM_ID, VIRTIO_SND_PCM_FMT_S16, VIRTIO_SND_PCM_RATE_48000,
    VIRTIO_SND_QUEUE_CONTROL, VIRTIO_SND_QUEUE_TX, VIRTIO_SND_R_PCM_PREPARE,
    VIRTIO_SND_R_PCM_SET_PARAMS, VIRTIO_SND_R_PCM_START, VIRTIO_SND_S_OK, VirtioSnd,
};
use aero_virtio::memory::{GuestMemory, GuestRam, read_u32_le, write_u16_le, write_u32_le, write_u64_le};
use aero_virtio::queue::{
    PoppedDescriptorChain, VirtQueue, VirtQueueConfig, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
};

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

const QUEUE_SIZE: u16 = 8;

// Guest RAM layout for the minimal virtio-snd harness.
const CONTROL_DESC_TABLE: u64 = 0x1000;
const CONTROL_AVAIL: u64 = 0x2000;
const CONTROL_USED: u64 = 0x3000;
const CONTROL_REQ_BUF: u64 = 0x4000;
const CONTROL_RESP_BUF: u64 = 0x5000;

const TX_DESC_TABLE: u64 = 0x6000;
const TX_AVAIL: u64 = 0x7000;
const TX_USED: u64 = 0x8000;
const TX_HDR_BUF: u64 = 0x9000;
const TX_PAYLOAD_BUF: u64 = 0xA000;
const TX_RESP_BUF: u64 = 0xB000;

/// Maximum PCM payload bytes in a single virtio-snd TX request (must match the device contract).
///
/// This is currently 256KiB in the device model (`aero-virtio`); keep this in sync so the demo
/// never constructs a request that the device will reject.
const MAX_TX_PAYLOAD_BYTES: usize = 256 * 1024;
const BYTES_PER_GUEST_FRAME: usize = 4; // stereo S16_LE
const MAX_TX_FRAMES_PER_REQ: usize = MAX_TX_PAYLOAD_BYTES / BYTES_PER_GUEST_FRAME;
/// Maximum number of TX requests to submit per `tick()` call.
///
/// The demo API is JS-callable and `frames` may be untrusted. Bounding the number of TX requests per
/// tick prevents pathological `tick()` calls from running unbounded loops.
const MAX_TX_REQUESTS_PER_TICK: usize = 4;

/// Bound per-tick work/allocations. The demo is JS-callable; avoid constructing multi-second PCM
/// buffers in a single call if a caller passes an absurd `frames` value.
const MAX_TICK_HOST_FRAMES: u32 = 16_384;

fn write_desc(
    mem: &mut GuestRam,
    table: u64,
    index: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) -> Result<(), JsValue> {
    let base = table + u64::from(index) * 16;
    write_u64_le(mem, base, addr).map_err(|e| js_error(format!("write desc.addr failed: {e:?}")))?;
    write_u32_le(mem, base + 8, len).map_err(|e| js_error(format!("write desc.len failed: {e:?}")))?;
    write_u16_le(mem, base + 12, flags).map_err(|e| js_error(format!("write desc.flags failed: {e:?}")))?;
    write_u16_le(mem, base + 14, next).map_err(|e| js_error(format!("write desc.next failed: {e:?}")))?;
    Ok(())
}

fn pop_chain(queue: &mut VirtQueue, mem: &GuestRam) -> Result<aero_virtio::queue::DescriptorChain, JsValue> {
    match queue
        .pop_descriptor_chain(mem)
        .map_err(|e| js_error(format!("virtqueue pop_descriptor_chain failed: {e:?}")))?
        .ok_or_else(|| js_error("virtqueue had no available descriptor chains"))?
    {
        PoppedDescriptorChain::Chain(chain) => Ok(chain),
        PoppedDescriptorChain::Invalid { error, .. } => Err(js_error(format!(
            "virtqueue descriptor chain parse error: {error:?}"
        ))),
    }
}

struct WorkletBridgeStatsSink {
    bridge: WorkletBridge,
    channel_count: u32,

    total_frames_produced: u32,
    total_frames_written: u32,
    total_frames_dropped: u32,

    last_tick_produced_frames: u32,
    last_tick_written_frames: u32,
    last_tick_dropped_frames: u32,
}

impl WorkletBridgeStatsSink {
    fn new(bridge: WorkletBridge) -> Result<Self, JsValue> {
        let channel_count = bridge.channel_count();
        if channel_count != 2 {
            return Err(js_error(
                "WorkletBridge channel_count must be 2 for virtio-snd playback demo (stereo)",
            ));
        }
        Ok(Self {
            bridge,
            channel_count,
            total_frames_produced: 0,
            total_frames_written: 0,
            total_frames_dropped: 0,
            last_tick_produced_frames: 0,
            last_tick_written_frames: 0,
            last_tick_dropped_frames: 0,
        })
    }

    fn reset_tick_stats(&mut self) {
        self.last_tick_produced_frames = 0;
        self.last_tick_written_frames = 0;
        self.last_tick_dropped_frames = 0;
    }

    fn bridge(&self) -> &WorkletBridge {
        &self.bridge
    }
}

impl AudioSink for WorkletBridgeStatsSink {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        let produced = (samples.len() as u32) / self.channel_count;
        if produced == 0 {
            return;
        }
        let written = self.bridge.write_f32_interleaved(samples);
        let dropped = produced.saturating_sub(written);

        self.total_frames_produced = self.total_frames_produced.wrapping_add(produced);
        self.total_frames_written = self.total_frames_written.wrapping_add(written);
        self.total_frames_dropped = self.total_frames_dropped.wrapping_add(dropped);

        self.last_tick_produced_frames = self.last_tick_produced_frames.wrapping_add(produced);
        self.last_tick_written_frames = self.last_tick_written_frames.wrapping_add(written);
        self.last_tick_dropped_frames = self.last_tick_dropped_frames.wrapping_add(dropped);
    }
}

/// End-to-end virtio-snd playback demo for the web UI / Playwright E2E tests.
#[wasm_bindgen]
pub struct VirtioSndPlaybackDemo {
    dev: VirtioSnd<WorkletBridgeStatsSink>,
    mem: GuestRam,
    host_sample_rate_hz: u32,

    total_frames_produced: u32,
    total_frames_written: u32,
    total_frames_dropped: u32,

    last_tick_produced_frames: u32,
    last_tick_written_frames: u32,
    last_tick_dropped_frames: u32,

    freq_hz: f32,
    gain: f32,
    /// Phase accumulator in cycles (0..1).
    phase: f32,

    pcm_scratch: Vec<u8>,

    last_tick_requested_frames: u32,
}

#[wasm_bindgen]
impl VirtioSndPlaybackDemo {
    #[wasm_bindgen(constructor)]
    pub fn new(
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
        host_sample_rate_hz: u32,
    ) -> Result<Self, JsValue> {
        if capacity_frames == 0 {
            return Err(js_error("capacityFrames must be non-zero"));
        }
        if channel_count != 2 {
            return Err(js_error(
                "channelCount must be 2 for virtio-snd playback demo output (stereo)",
            ));
        }
        if host_sample_rate_hz == 0 {
            return Err(js_error("hostSampleRate must be non-zero"));
        }

        // Defensive clamp: avoid absurd sample rates triggering large resampler buffers.
        let host_sample_rate_hz = host_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);

        let bridge = WorkletBridge::from_shared_buffer(ring_sab, capacity_frames, channel_count)?;
        let sink = WorkletBridgeStatsSink::new(bridge)?;

        let mut dev = VirtioSnd::new_with_host_sample_rate(sink, host_sample_rate_hz);
        // Mirror normal virtio feature negotiation (best-effort).
        dev.set_features(dev.device_features());

        // Allocate a small private guest RAM backing store. Keep this bounded; the demo only needs
        // enough space for descriptor rings + a single 256KiB TX payload buffer.
        //
        // 512KiB is plenty and avoids multi-megabyte allocations in Playwright.
        let mut mem = GuestRam::new(0x80000);

        // Ensure the TX payload region exists inside guest RAM.
        let end = TX_PAYLOAD_BUF
            .checked_add(MAX_TX_PAYLOAD_BYTES as u64)
            .ok_or_else(|| js_error("TX payload layout overflow"))?;
        if end > mem.len() {
            return Err(js_error("Guest RAM allocation is too small for TX payload buffer"));
        }

        // Bring the playback stream to RUNNING via the control queue.
        drive_playback_to_running(&mut dev, &mut mem)?;

        let mut demo = Self {
            dev,
            mem,
            host_sample_rate_hz,
            total_frames_produced: 0,
            total_frames_written: 0,
            total_frames_dropped: 0,
            last_tick_produced_frames: 0,
            last_tick_written_frames: 0,
            last_tick_dropped_frames: 0,
            freq_hz: 440.0,
            gain: 0.1,
            phase: 0.0,
            pcm_scratch: Vec::new(),
            last_tick_requested_frames: 0,
        };

        // Prime with a default tone so the demo produces audio immediately.
        demo.set_sine_wave(440.0, 0.1);

        Ok(demo)
    }

    #[wasm_bindgen(getter)]
    pub fn host_sample_rate_hz(&self) -> u32 {
        self.host_sample_rate_hz
    }

    /// Configure the sine wave generator. This does not reprogram any virtio-snd device state; it
    /// only changes the PCM payload written into the TX queue.
    pub fn set_sine_wave(&mut self, freq_hz: f32, gain: f32) {
        self.freq_hz = if freq_hz.is_finite() && freq_hz > 0.0 {
            freq_hz
        } else {
            440.0
        };
        self.gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.1
        };
        self.phase = 0.0;
    }

    /// Advance the demo by generating up to `frames` frames of audio at the configured host sample
    /// rate and submitting them through the virtio-snd TX virtqueue.
    ///
    /// Returns the current AudioWorklet ring buffer fill level (frames).
    pub fn tick(&mut self, frames: u32) -> u32 {
        self.last_tick_requested_frames = frames;
        self.last_tick_produced_frames = 0;
        self.last_tick_written_frames = 0;
        self.last_tick_dropped_frames = 0;

        // Reset per-tick counters in the sink.
        {
            let sink = self.dev.output_mut();
            sink.reset_tick_stats();
        }

        let (level, capacity) = {
            let sink = self.dev.output_mut();
            (sink.bridge().buffer_level_frames(), sink.bridge().capacity_frames())
        };
        let free = capacity.saturating_sub(level);
        // Keep a small safety margin so rounding in the resampler cannot overflow the ring and
        // increment `overrunCount` in CI smoke tests.
        let free_safe = free.saturating_sub(4);
        if free_safe == 0 || frames == 0 {
            return level;
        }

        let want_host_frames = frames.min(free_safe).min(MAX_TICK_HOST_FRAMES);
        if want_host_frames == 0 {
            return level;
        }

        // Convert desired host frames into guest (48kHz) frames.
        //
        // We want to keep the AudioWorklet ring from overrunning, so avoid grossly oversupplying
        // source frames (which can expand significantly when upsampling, e.g. 48kHz -> 192kHz).
        //
        // `LinearResampler::required_source_frames` is not exposed from the device, so approximate
        // it with a conservative "minimal to produce N frames" formula based on the same math:
        //
        //   src_needed ≈ ceil((dst_frames - 1) * src_rate / dst_rate) + 1
        //
        // This yields the smallest number of 48kHz source frames that should allow the resampler
        // to emit `dst_frames` output frames when starting from `src_pos=0.0`. In practice the
        // resampler may carry a fractional position across calls; the demo uses a ring-space safety
        // margin so minor rounding differences do not overflow the buffer.
        let dst_rate = self.host_sample_rate_hz.max(1) as u64;
        let src_rate = PCM_SAMPLE_RATE_HZ as u64;
        let want_host_frames_u64 = want_host_frames as u64;
        let mut want_src_frames = if want_host_frames_u64 <= 1 {
            1
        } else {
            let dst_minus1 = want_host_frames_u64 - 1;
            ((dst_minus1.saturating_mul(src_rate) + dst_rate - 1) / dst_rate).saturating_add(1)
        };
        let max_src_per_tick = (MAX_TX_FRAMES_PER_REQ as u64).saturating_mul(MAX_TX_REQUESTS_PER_TICK as u64);
        want_src_frames = want_src_frames.min(max_src_per_tick);

        // Avoid constructing requests larger than the virtio-snd contract limit.
        let mut remaining_src = want_src_frames as usize;
        let mut submitted = 0usize;

        while remaining_src > 0 && submitted < MAX_TX_REQUESTS_PER_TICK {
            // Re-check ring space between chunks. This keeps us robust if the AudioWorklet consumes
            // frames while we're generating.
            let level = self.dev.output_mut().bridge().buffer_level_frames();
            let free = capacity.saturating_sub(level);
            if free <= 4 {
                break;
            }

            let chunk_frames = remaining_src.min(MAX_TX_FRAMES_PER_REQ);
            if chunk_frames == 0 {
                break;
            }

            if self.submit_tx_chunk(chunk_frames).is_err() {
                // Best-effort: if the demo fails to submit a TX request (e.g. out-of-bounds guest
                // layout), stop generating for this tick. The demo is for diagnostics; panicking
                // here would abort the entire WASM module.
                break;
            }
            remaining_src -= chunk_frames;
            submitted += 1;
        }

        // Copy counters out of the sink so JS can query them via `&self` getters.
        {
            let sink = self.dev.output_mut();
            self.total_frames_produced = sink.total_frames_produced;
            self.total_frames_written = sink.total_frames_written;
            self.total_frames_dropped = sink.total_frames_dropped;
            self.last_tick_produced_frames = sink.last_tick_produced_frames;
            self.last_tick_written_frames = sink.last_tick_written_frames;
            self.last_tick_dropped_frames = sink.last_tick_dropped_frames;
        }

        self.dev.output_mut().bridge().buffer_level_frames()
    }

    #[wasm_bindgen(getter)]
    pub fn total_frames_produced(&self) -> u32 {
        self.total_frames_produced
    }

    #[wasm_bindgen(getter)]
    pub fn total_frames_written(&self) -> u32 {
        self.total_frames_written
    }

    #[wasm_bindgen(getter)]
    pub fn total_frames_dropped(&self) -> u32 {
        self.total_frames_dropped
    }

    #[wasm_bindgen(getter)]
    pub fn last_tick_requested_frames(&self) -> u32 {
        self.last_tick_requested_frames
    }

    #[wasm_bindgen(getter)]
    pub fn last_tick_produced_frames(&self) -> u32 {
        self.last_tick_produced_frames
    }

    #[wasm_bindgen(getter)]
    pub fn last_tick_written_frames(&self) -> u32 {
        self.last_tick_written_frames
    }

    #[wasm_bindgen(getter)]
    pub fn last_tick_dropped_frames(&self) -> u32 {
        self.last_tick_dropped_frames
    }
}

impl VirtioSndPlaybackDemo {
    fn submit_tx_chunk(&mut self, frames: usize) -> Result<(), JsValue> {
        // Generate PCM payload into scratch buffer (stereo S16_LE at 48kHz).
        let bytes = frames
            .checked_mul(BYTES_PER_GUEST_FRAME)
            .ok_or_else(|| js_error("TX payload length overflow"))?;
        if bytes > MAX_TX_PAYLOAD_BYTES {
            return Err(js_error("TX payload exceeds MAX_TX_PAYLOAD_BYTES"));
        }

        self.pcm_scratch.resize(bytes, 0);
        let phase_inc = self.freq_hz / (PCM_SAMPLE_RATE_HZ as f32);
        for i in 0..frames {
            let s = (2.0 * core::f32::consts::PI * self.phase).sin() * self.gain;
            let v = (s * i16::MAX as f32) as i16;
            let off = i * BYTES_PER_GUEST_FRAME;
            self.pcm_scratch[off..off + 2].copy_from_slice(&v.to_le_bytes());
            self.pcm_scratch[off + 2..off + 4].copy_from_slice(&v.to_le_bytes());
            self.phase += phase_inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
        }

        // Write TX header (stream_id + padding) and payload into guest RAM.
        let mut hdr = [0u8; 8];
        hdr[0..4].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
        self.mem
            .write(TX_HDR_BUF, &hdr)
            .map_err(|e| js_error(format!("write TX header failed: {e:?}")))?;
        self.mem
            .write(TX_PAYLOAD_BUF, &self.pcm_scratch)
            .map_err(|e| js_error(format!("write TX payload failed: {e:?}")))?;
        // Clear response buffer.
        self.mem
            .get_slice_mut(TX_RESP_BUF, 8)
            .map_err(|e| js_error(format!("map TX resp failed: {e:?}")))?
            .fill(0);

        // Descriptor chain: OUT header -> OUT payload -> IN status.
        write_desc(
            &mut self.mem,
            TX_DESC_TABLE,
            0,
            TX_HDR_BUF,
            8,
            VIRTQ_DESC_F_NEXT,
            1,
        )?;
        write_desc(
            &mut self.mem,
            TX_DESC_TABLE,
            1,
            TX_PAYLOAD_BUF,
            self.pcm_scratch.len() as u32,
            VIRTQ_DESC_F_NEXT,
            2,
        )?;
        write_desc(
            &mut self.mem,
            TX_DESC_TABLE,
            2,
            TX_RESP_BUF,
            8,
            VIRTQ_DESC_F_WRITE,
            0,
        )?;

        // Publish the chain in the TX avail ring.
        write_u16_le(&mut self.mem, TX_AVAIL, 0).map_err(|e| js_error(format!("write TX avail.flags failed: {e:?}")))?;
        write_u16_le(&mut self.mem, TX_AVAIL + 2, 1).map_err(|e| js_error(format!("write TX avail.idx failed: {e:?}")))?;
        write_u16_le(&mut self.mem, TX_AVAIL + 4, 0).map_err(|e| js_error(format!("write TX avail.ring failed: {e:?}")))?;
        write_u16_le(&mut self.mem, TX_USED, 0).map_err(|e| js_error(format!("write TX used.flags failed: {e:?}")))?;
        write_u16_le(&mut self.mem, TX_USED + 2, 0).map_err(|e| js_error(format!("write TX used.idx failed: {e:?}")))?;

        let mut queue = VirtQueue::new(
            VirtQueueConfig {
                size: QUEUE_SIZE,
                desc_addr: TX_DESC_TABLE,
                avail_addr: TX_AVAIL,
                used_addr: TX_USED,
            },
            false,
        )
        .map_err(|e| js_error(format!("create TX virtqueue failed: {e:?}")))?;

        let chain = pop_chain(&mut queue, &self.mem)?;
        self.dev
            .process_queue(VIRTIO_SND_QUEUE_TX, chain, &mut queue, &mut self.mem)
            .map_err(|e| js_error(format!("virtio-snd TX failed: {e:?}")))?;

        // Sanity-check response status so failures are visible in the UI/test logs.
        let status = read_u32_le(&self.mem, TX_RESP_BUF)
            .map_err(|e| js_error(format!("read TX status failed: {e:?}")))?;
        if status != VIRTIO_SND_S_OK {
            return Err(js_error(format!("virtio-snd TX returned status 0x{status:08x}")));
        }

        Ok(())
    }
}

fn drive_playback_to_running(dev: &mut VirtioSnd<WorkletBridgeStatsSink>, mem: &mut GuestRam) -> Result<(), JsValue> {
    // PCM_SET_PARAMS (24 bytes).
    let mut req = [0u8; 24];
    req[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_SET_PARAMS.to_le_bytes());
    req[4..8].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
    req[8..12].copy_from_slice(&4096u32.to_le_bytes()); // buffer_bytes
    req[12..16].copy_from_slice(&1024u32.to_le_bytes()); // period_bytes
    // features [16..20] = 0
    req[20] = 2; // channels
    req[21] = VIRTIO_SND_PCM_FMT_S16;
    req[22] = VIRTIO_SND_PCM_RATE_48000;
    // req[23] reserved

    let status = run_control_req(dev, mem, &req)?;
    if status != VIRTIO_SND_S_OK {
        return Err(js_error(format!(
            "virtio-snd PCM_SET_PARAMS failed with status 0x{status:08x}"
        )));
    }

    // PCM_PREPARE.
    let mut prepare = [0u8; 8];
    prepare[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_PREPARE.to_le_bytes());
    prepare[4..8].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
    let status = run_control_req(dev, mem, &prepare)?;
    if status != VIRTIO_SND_S_OK {
        return Err(js_error(format!(
            "virtio-snd PCM_PREPARE failed with status 0x{status:08x}"
        )));
    }

    // PCM_START.
    let mut start = [0u8; 8];
    start[0..4].copy_from_slice(&VIRTIO_SND_R_PCM_START.to_le_bytes());
    start[4..8].copy_from_slice(&PLAYBACK_STREAM_ID.to_le_bytes());
    let status = run_control_req(dev, mem, &start)?;
    if status != VIRTIO_SND_S_OK {
        return Err(js_error(format!(
            "virtio-snd PCM_START failed with status 0x{status:08x}"
        )));
    }

    Ok(())
}

fn run_control_req(
    dev: &mut VirtioSnd<WorkletBridgeStatsSink>,
    mem: &mut GuestRam,
    req: &[u8],
) -> Result<u32, JsValue> {
    // Write request + clear response.
    mem.write(CONTROL_REQ_BUF, req)
        .map_err(|e| js_error(format!("write control request failed: {e:?}")))?;
    mem.get_slice_mut(CONTROL_RESP_BUF, 64)
        .map_err(|e| js_error(format!("map control response failed: {e:?}")))?
        .fill(0);

    // Descriptor chain: OUT request -> IN response.
    write_desc(
        mem,
        CONTROL_DESC_TABLE,
        0,
        CONTROL_REQ_BUF,
        req.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    )?;
    write_desc(
        mem,
        CONTROL_DESC_TABLE,
        1,
        CONTROL_RESP_BUF,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    )?;

    // Populate avail ring with one entry.
    write_u16_le(mem, CONTROL_AVAIL, 0).map_err(|e| js_error(format!("write control avail.flags failed: {e:?}")))?;
    write_u16_le(mem, CONTROL_AVAIL + 2, 1).map_err(|e| js_error(format!("write control avail.idx failed: {e:?}")))?;
    write_u16_le(mem, CONTROL_AVAIL + 4, 0).map_err(|e| js_error(format!("write control avail.ring failed: {e:?}")))?;
    write_u16_le(mem, CONTROL_USED, 0).map_err(|e| js_error(format!("write control used.flags failed: {e:?}")))?;
    write_u16_le(mem, CONTROL_USED + 2, 0).map_err(|e| js_error(format!("write control used.idx failed: {e:?}")))?;

    let mut queue = VirtQueue::new(
        VirtQueueConfig {
            size: QUEUE_SIZE,
            desc_addr: CONTROL_DESC_TABLE,
            avail_addr: CONTROL_AVAIL,
            used_addr: CONTROL_USED,
        },
        false,
    )
    .map_err(|e| js_error(format!("create control virtqueue failed: {e:?}")))?;

    let chain = pop_chain(&mut queue, mem)?;
    dev.process_queue(VIRTIO_SND_QUEUE_CONTROL, chain, &mut queue, mem)
        .map_err(|e| js_error(format!("virtio-snd control failed: {e:?}")))?;

    read_u32_le(mem, CONTROL_RESP_BUF)
        .map_err(|e| js_error(format!("read control status failed: {e:?}")))
}
