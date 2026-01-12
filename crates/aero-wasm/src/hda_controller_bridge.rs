//! WASM-side bridge for exposing a guest-visible Intel HD Audio (HDA) controller.
//!
//! The browser I/O worker exposes this as a PCI function with an MMIO BAR; MMIO reads/writes are
//! forwarded into this bridge which updates the Rust device model (`aero_audio::hda::HdaController`).
//!
//! The HDA controller performs DMA reads/writes (BDL, PCM buffers, position buffer, CORB/RIRB, ...)
//! directly against guest RAM. In the browser runtime, guest physical address 0 maps to
//! `guest_base` within the module's linear memory (see `guest_ram_layout`); this bridge uses the
//! OOB-safe guest memory adapter (`HdaGuestMemory`) from `lib.rs` to provide that memory interface.
//!
//! Snapshot/restore:
//! - HDA controller state is serialized as an `aero-io-snapshot` TLV blob (`HdaControllerState`).
//! - The snapshot includes `AudioWorkletRingState` (read/write indices + capacity) for determinism.
//! - Host audio samples are not serialized; on restore we clear the AudioWorklet ring samples to
//!   silence via `WorkletBridge::restore_state`.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::{SharedArrayBuffer, Uint8Array};

use aero_audio::hda::HdaController;
use aero_audio::sink::AudioSink;
use aero_io_snapshot::io::audio::state::{AudioWorkletRingState, HdaControllerState};
use aero_io_snapshot::io::state::IoSnapshot as _;

use aero_platform::audio::mic_bridge::MicBridge;
use aero_platform::audio::worklet_bridge::WorkletBridge;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

fn validate_mmio_size(size: u32) -> Option<usize> {
    match size {
        1 | 2 | 4 => Some(size as usize),
        _ => None,
    }
}

struct WorkletBridgeSink<'a> {
    bridge: &'a WorkletBridge,
    channel_count: u32,
}

impl<'a> AudioSink for WorkletBridgeSink<'a> {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        if samples.is_empty() || self.channel_count == 0 {
            return;
        }
        let _ = self.bridge.write_f32_interleaved(samples);
    }
}

/// WASM export: reusable HDA controller model for the browser I/O worker.
///
/// The controller reads/writes guest RAM directly from the module's linear memory (shared across
/// workers in the threaded build) using `guest_base` and `guest_size` from the `guest_ram_layout`
/// contract.
#[wasm_bindgen]
pub struct HdaControllerBridge {
    hda: HdaController,
    mem: crate::HdaGuestMemory,

    audio_ring: Option<WorkletBridge>,
    mic_ring: Option<MicBridge>,

    pending_audio_ring_state: Option<AudioWorkletRingState>,
}

#[wasm_bindgen]
impl HdaControllerBridge {
    /// Create a new HDA controller bound to the provided guest RAM mapping.
    ///
    /// - `guest_base` is the byte offset inside wasm linear memory where guest physical address 0
    ///   begins (see `guest_ram_layout`).
    /// - `guest_size` is the guest RAM size in bytes. Pass `0` to use "the remainder of linear
    ///   memory" as guest RAM (mirrors `WasmVm` and other device bridges).
    #[wasm_bindgen(constructor)]
    pub fn new(
        guest_base: u32,
        guest_size: u32,
        output_sample_rate_hz: Option<u32>,
    ) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(js_error("guest_base must be non-zero"));
        }

        let output_sample_rate_hz = match output_sample_rate_hz {
            Some(0) => return Err(js_error("outputSampleRateHz must be non-zero")),
            Some(v) => v,
            None => 48_000,
        };

        let mem_bytes = wasm_memory_byte_len();
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base as u64)
        } else {
            guest_size as u64
        };
        // Keep guest RAM below the PCI MMIO aperture (see `guest_ram_layout` contract).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);
        if guest_size_u64 == 0 {
            return Err(js_error("guest_size must be non-zero"));
        }

        let end = (guest_base as u64)
            .checked_add(guest_size_u64)
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            hda: HdaController::new_with_output_rate(output_sample_rate_hz),
            mem: crate::HdaGuestMemory {
                guest_base,
                guest_size: guest_size_u64,
            },
            audio_ring: None,
            mic_ring: None,
            pending_audio_ring_state: None,
        })
    }

    /// Return the host/output sample rate used by the controller when emitting audio.
    #[wasm_bindgen(getter)]
    pub fn output_sample_rate_hz(&self) -> u32 {
        self.hda.output_rate_hz()
    }

    /// Read from the HDA MMIO region.
    ///
    /// `size` must be 1, 2, or 4; invalid sizes return 0.
    pub fn mmio_read(&mut self, offset: u32, size: u32) -> u32 {
        let Some(size) = validate_mmio_size(size) else {
            return 0;
        };
        self.hda.mmio_read(offset as u64, size) as u32
    }

    /// Write to the HDA MMIO region.
    ///
    /// `size` must be 1, 2, or 4; invalid sizes are ignored.
    pub fn mmio_write(&mut self, offset: u32, size: u32, value: u32) {
        let Some(size) = validate_mmio_size(size) else {
            return;
        };
        self.hda.mmio_write(offset as u64, size, value as u64);
    }

    /// Attach the audio output ring buffer (producer side; AudioWorklet is the consumer).
    ///
    /// `channel_count` must be 2 (stereo).
    pub fn attach_audio_ring(
        &mut self,
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        if capacity_frames == 0 {
            return Err(js_error("capacityFrames must be non-zero"));
        }
        if channel_count != 2 {
            return Err(js_error(
                "channelCount must be 2 for HDA output (stereo)",
            ));
        }

        let bridge = WorkletBridge::from_shared_buffer(ring_sab, capacity_frames, channel_count)?;

        // Apply a deferred ring restore if `load_state` was called before the host reattached the
        // AudioWorklet ring.
        if let Some(mut state) = self.pending_audio_ring_state.take() {
            // Snapshot state stores the ring capacity for determinism/debugging, but restores must be
            // best-effort. If the host allocates a different capacity than what was captured in the
            // snapshot, clear the field so `WorkletBridge::restore_state` bypasses its debug-only
            // capacity assertion.
            if state.capacity_frames != 0
                && state.capacity_frames != bridge.capacity_frames()
            {
                state.capacity_frames = 0;
            }
            bridge.restore_state(&state);
        }
        self.audio_ring = Some(bridge);
        Ok(())
    }

    pub fn detach_audio_ring(&mut self) {
        self.audio_ring = None;
    }

    /// Convenience helper: attach/detach the audio ring buffer using an `Option`.
    ///
    /// This mirrors older JS call sites that use `set_*_ring_buffer(undefined)` to detach.
    pub fn set_audio_ring_buffer(
        &mut self,
        ring_sab: Option<SharedArrayBuffer>,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        match ring_sab {
            Some(sab) => self.attach_audio_ring(sab, capacity_frames, channel_count),
            None => {
                self.detach_audio_ring();
                Ok(())
            }
        }
    }

    /// Alias for [`Self::attach_audio_ring`] retained for older call sites/spec drafts.
    pub fn attach_output_ring(
        &mut self,
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        self.attach_audio_ring(ring_sab, capacity_frames, channel_count)
    }

    /// Alias for [`Self::detach_audio_ring`] retained for older call sites/spec drafts.
    pub fn detach_output_ring(&mut self) {
        self.detach_audio_ring();
    }

    /// Attach the microphone capture ring buffer (consumer side; AudioWorklet is the producer).
    pub fn attach_mic_ring(
        &mut self,
        ring_sab: SharedArrayBuffer,
        sample_rate: u32,
    ) -> Result<(), JsValue> {
        if sample_rate == 0 {
            return Err(js_error("sampleRate must be non-zero"));
        }

        let bridge = MicBridge::from_shared_buffer(ring_sab)?;
        self.hda.set_capture_sample_rate_hz(sample_rate);
        self.mic_ring = Some(bridge);
        Ok(())
    }

    pub fn detach_mic_ring(&mut self) {
        self.mic_ring = None;
    }

    /// Set the host/output sample rate used by the controller when emitting audio.
    pub fn set_output_rate_hz(&mut self, rate: u32) -> Result<(), JsValue> {
        if rate == 0 {
            return Err(js_error("rate must be non-zero"));
        }
        self.hda.set_output_rate_hz(rate);
        Ok(())
    }

    /// Alias for [`Self::set_output_rate_hz`] retained for older call sites/spec drafts.
    pub fn set_output_sample_rate_hz(&mut self, rate: u32) -> Result<(), JsValue> {
        self.set_output_rate_hz(rate)
    }

    /// Advance the HDA device by `frames` worth of host time.
    ///
    /// - If an audio ring is attached, produced audio is written into it.
    /// - If a microphone ring is attached, capture DMA consumes samples from it.
    /// - If no rings are attached, the device still advances so guest-visible state (DMA position,
    ///   interrupts, etc.) progresses; host audio is dropped.
    pub fn process(&mut self, frames: u32) {
        if frames == 0 {
            return;
        }

        let frames = frames as usize;

        let hda = &mut self.hda;
        let mem = &mut self.mem;

        match (&self.audio_ring, &mut self.mic_ring) {
            (Some(ring), Some(mic)) => {
                let mut sink = WorkletBridgeSink {
                    bridge: ring,
                    channel_count: ring.channel_count(),
                };
                hda.process_into_with_capture(mem, frames, &mut sink, mic);
            }
            (Some(ring), None) => {
                let mut sink = WorkletBridgeSink {
                    bridge: ring,
                    channel_count: ring.channel_count(),
                };
                hda.process_into(mem, frames, &mut sink);
            }
            (None, Some(mic)) => {
                hda.process_with_capture(mem, frames, mic);
            }
            (None, None) => {
                hda.process(mem, frames);
            }
        }
    }

    /// Alias for [`Self::process`] retained for older call sites.
    pub fn step_frames(&mut self, frames: u32) {
        self.process(frames);
    }

    /// Compatibility shim: attach/detach the mic ring buffer without setting a sample rate.
    ///
    /// Prefer [`Self::attach_mic_ring`] + [`Self::detach_mic_ring`] for new code.
    pub fn set_mic_ring_buffer(&mut self, sab: Option<SharedArrayBuffer>) -> Result<(), JsValue> {
        self.mic_ring = match sab {
            Some(sab) => Some(MicBridge::from_shared_buffer(sab)?),
            None => None,
        };
        Ok(())
    }

    /// Compatibility shim for older call sites: set the capture sample rate without attaching a ring.
    ///
    /// Prefer passing the rate to [`Self::attach_mic_ring`].
    pub fn set_capture_sample_rate_hz(&mut self, sample_rate_hz: u32) {
        if sample_rate_hz == 0 {
            return;
        }
        self.hda.set_capture_sample_rate_hz(sample_rate_hz);
    }

    /// Whether the guest-visible interrupt line should be asserted.
    pub fn irq_level(&self) -> bool {
        self.hda.irq_level()
    }

    /// If an audio ring is attached, returns its current buffered level (frames).
    ///
    /// Returns 0 if no ring is attached.
    pub fn buffer_level_frames(&self) -> u32 {
        self.audio_ring
            .as_ref()
            .map(|r| r.buffer_level_frames())
            .unwrap_or(0)
    }

    /// If an audio ring is attached, returns its total producer overrun counter (frames dropped).
    ///
    /// Returns 0 if no ring is attached.
    pub fn overrun_count(&self) -> u32 {
        self.audio_ring
            .as_ref()
            .map(|r| r.overrun_count())
            .unwrap_or(0)
    }

    /// Serialize the current HDA controller state into a deterministic snapshot blob.
    ///
    /// If an AudioWorklet ring is attached, its indices are included for determinism.
    pub fn save_state(&self) -> Vec<u8> {
        let ring_state = if let Some(ring) = self.audio_ring.as_ref() {
            ring.snapshot_state()
        } else if let Some(state) = self.pending_audio_ring_state.as_ref() {
            state.clone()
        } else {
            AudioWorkletRingState {
                capacity_frames: 0,
                write_pos: 0,
                read_pos: 0,
            }
        };

        let state = self.hda.snapshot_state(ring_state);
        state.save_state()
    }

    /// Restore HDA controller state from a snapshot blob produced by [`save_state`].
    ///
    /// If an AudioWorklet ring is attached, ring indices are restored immediately and samples are
    /// cleared to silence via `WorkletBridge::restore_state`. If not yet attached, the ring state
    /// is cached and applied when [`attach_audio_ring`] (or [`set_audio_ring_buffer`]) is called.
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut state = HdaControllerState::default();
        state
            .load_state(bytes)
            .map_err(|e| js_error(format!("Invalid HDA snapshot: {e}")))?;

        self.hda.restore_state(&state);

        let mut ring_state = state.worklet_ring;
        if let Some(ring) = self.audio_ring.as_ref() {
            if ring_state.capacity_frames != 0
                && ring_state.capacity_frames != ring.capacity_frames()
            {
                ring_state.capacity_frames = 0;
            }
            ring.restore_state(&ring_state);
            self.pending_audio_ring_state = None;
        } else {
            self.pending_audio_ring_state = Some(ring_state);
        }

        Ok(())
    }

    /// Snapshot the full device state as deterministic bytes.
    pub fn snapshot_state(&self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore device state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use aero_audio::capture::SilenceCaptureSource;
    use aero_audio::mem::MemoryAccess;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn step_frames_completes_on_oob_dma_pointer() {
        let mut guest = vec![0u8; 0x4000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

        // Bring controller out of reset.
        bridge.hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

        // Configure the codec converter to listen on stream 1, channel 0.
        // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
        let set_stream_ch = (0x706u32 << 8) | 0x10;
        bridge.hda.codec_mut().execute_verb(2, set_stream_ch);

        // Stream format: 48kHz, 16-bit, 2ch.
        let fmt_raw: u16 = (1 << 4) | 0x1;
        // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
        let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
        bridge.hda.codec_mut().execute_verb(2, set_fmt);

        // Guest buffer layout: BDL is in-bounds, but it points at an out-of-bounds PCM address.
        let bdl_base = 0x1000u64;
        let pcm_len_bytes = 512u32; // 128 frames @ 16-bit stereo
        let oob_pcm_base = u64::from(guest_size) + 0x1000;

        bridge.mem.write_u64(bdl_base, oob_pcm_base);
        bridge.mem.write_u32(bdl_base + 8, pcm_len_bytes);
        bridge.mem.write_u32(bdl_base + 12, 1); // IOC=1

        // Configure stream descriptor 0.
        {
            let sd = bridge.hda.stream_mut(0);
            sd.bdpl = bdl_base as u32;
            sd.bdpu = 0;
            sd.cbl = pcm_len_bytes;
            sd.lvi = 0;
            sd.fmt = fmt_raw;
            // SRST | RUN | IOCE | stream number 1.
            sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
        }

        // The call should complete without panicking even though the DMA address is invalid.
        bridge.step_frames(128);
    }

    #[wasm_bindgen_test]
    fn hda_snapshot_state_roundtrips() {
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();
        bridge.set_output_rate_hz(44_100).unwrap();
        // Mutate some guest-visible registers to ensure we aren't snapshotting the all-zero default.
        bridge.mmio_write(0x0c, 2, 0x1234); // WAKEEN
        bridge.mmio_write(0x20, 4, 0x0000_0001); // INTCTL

        let snap = bridge.save_state();

        let mut restored = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();
        restored.load_state(&snap).unwrap();
        let snap2 = restored.save_state();
        assert_eq!(snap2, snap);
    }

    #[wasm_bindgen_test]
    fn hda_guest_memory_zero_fills_on_addr_len_overflow() {
        let mut guest = vec![0u8; 16];
        guest[0] = 0xAB;
        let guest_base = guest.as_mut_ptr() as u32;

        // Construct the memory adapter directly so we can exercise the `addr+len` overflow path.
        let mut mem = crate::HdaGuestMemory {
            guest_base,
            // Allow almost any `addr` through the `end > guest_size` check so we can hit the
            // `checked_add` overflow path for `addr + len`.
            guest_size: u64::MAX,
        };

        // `addr + len` overflows u64, so the adapter must treat it as out-of-bounds and
        // return a zero-filled read without panicking.
        let mut buf = [0xAAu8; 8];
        mem.read_physical(u64::MAX - 1, &mut buf);
        assert_eq!(buf, [0u8; 8]);

        // Writes must also treat `addr+len` overflow as out-of-bounds and be ignored.
        mem.write_physical(u64::MAX - 1, &[0xCC; 8]);
        assert_eq!(guest[0], 0xAB);
    }

    #[wasm_bindgen_test]
    fn capture_process_completes_on_oob_dma_pointer() {
        let mut guest = vec![0u8; 0x4000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = HdaControllerBridge::new(guest_base, guest_size).unwrap();

        // Bring controller out of reset.
        bridge.hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

        // Configure the input converter to use stream ID 2, channel 0.
        // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
        let set_stream_ch = (0x706u32 << 8) | 0x20;
        bridge.hda.codec_mut().execute_verb(4, set_stream_ch);

        // Guest buffer layout: BDL is in-bounds, but it points at an out-of-bounds destination.
        let bdl_base = 0x1000u64;
        let buf_len = 0x1000u32;
        let oob_dst = u64::from(guest_size) + 0x1000;

        bridge.mem.write_u64(bdl_base, oob_dst);
        bridge.mem.write_u32(bdl_base + 8, buf_len);
        bridge.mem.write_u32(bdl_base + 12, 0);

        // Configure capture stream descriptor 1.
        {
            let sd = bridge.hda.stream_mut(1);
            sd.bdpl = bdl_base as u32;
            sd.bdpu = 0;
            sd.cbl = buf_len;
            sd.lvi = 0;
            sd.fmt = 0x0010; // 48kHz, 16-bit, mono
            // SRST | RUN | stream number 2.
            sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
        }

        let mut capture = SilenceCaptureSource;
        bridge.hda.process_with_capture(&mut bridge.mem, 128, &mut capture);
    }

    #[wasm_bindgen_test]
    fn snapshot_roundtrip_restores_mmio_registers() {
        let mut guest = vec![0u8; 0x4000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = HdaControllerBridge::new(guest_base, guest_size, None).unwrap();

        // Mutate a guest-visible register (GCTL).
        bridge.mmio_write(0x08, 4, 0x1);
        let before = bridge.mmio_read(0x08, 4);

        let bytes = bridge.save_state();

        // Change state again so restore has something to do.
        bridge.mmio_write(0x08, 4, 0x0);
        assert_ne!(bridge.mmio_read(0x08, 4), before);

        bridge.load_state(&bytes).unwrap();
        assert_eq!(bridge.mmio_read(0x08, 4), before);
    }
}
