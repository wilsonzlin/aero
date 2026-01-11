#![cfg_attr(feature = "wasm-threaded", feature(thread_local))]

use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::worklet_bridge::WorkletBridge;

#[cfg(target_arch = "wasm32")]
use aero_opfs::OpfsSyncFile;

#[cfg(target_arch = "wasm32")]
use js_sys::SharedArrayBuffer;

#[cfg(target_arch = "wasm32")]
use aero_audio::pcm::{decode_pcm_to_stereo_f32, LinearResampler, StreamFormat};

// wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
// `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
// by the linker when there is at least one TLS variable. We keep a tiny TLS
// slot behind a cargo feature enabled only for the threaded build.
#[cfg(feature = "wasm-threaded")]
#[thread_local]
static TLS_DUMMY: u8 = 0;

#[wasm_bindgen(start)]
pub fn wasm_start() {
    #[cfg(feature = "wasm-threaded")]
    {
        // Ensure the TLS dummy is not optimized away.
        let _ = &TLS_DUMMY as *const u8;
    }
}

/// Placeholder API exported to JS. Both the threaded and single WASM variants
/// are built from this crate and must expose an identical surface.
#[wasm_bindgen]
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

#[wasm_bindgen]
pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

/// Tiny numeric API used by the worker harness (`web/src/runtime/wasm_context.ts`).
///
/// NOTE: This coexists with `AeroApi::version()` (string) and is intentionally
/// cheap to call (no allocations).
#[wasm_bindgen]
pub fn version() -> u32 {
    1
}

#[wasm_bindgen]
pub fn sum(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn create_worklet_bridge(capacity_frames: u32, channel_count: u32) -> Result<WorkletBridge, JsValue> {
    WorkletBridge::new(capacity_frames, channel_count)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn attach_worklet_bridge(
    sab: SharedArrayBuffer,
    capacity_frames: u32,
    channel_count: u32,
) -> Result<WorkletBridge, JsValue> {
    WorkletBridge::from_shared_buffer(sab, capacity_frames, channel_count)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct SineTone {
    phase: f32,
    scratch: Vec<f32>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl SineTone {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            phase: 0.0,
            scratch: Vec::new(),
        }
    }

    /// Generate a sine wave and write it to the shared audio ring buffer.
    ///
    /// Returns the number of frames written (may be less than `frames` if the
    /// ring buffer is full).
    pub fn write(
        &mut self,
        bridge: &WorkletBridge,
        frames: u32,
        freq_hz: f32,
        sample_rate: f32,
        gain: f32,
    ) -> u32 {
        if frames == 0 || sample_rate <= 0.0 {
            return 0;
        }

        let channel_count = bridge.channel_count();
        if channel_count == 0 {
            return 0;
        }

        let total_samples = frames as usize * channel_count as usize;
        self.scratch.clear();
        self.scratch.resize(total_samples, 0.0);

        let phase_step = freq_hz / sample_rate;
        for frame in 0..frames as usize {
            let sample = (self.phase * core::f32::consts::TAU).sin() * gain;
            self.phase += phase_step;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }

            let base = frame * channel_count as usize;
            for ch in 0..channel_count as usize {
                self.scratch[base + ch] = sample;
            }
        }

        bridge.write_f32_interleaved(&self.scratch)
    }
}

/// Stateful converter for guest HDA PCM streams into the Web Audio ring buffer.
///
/// This is designed to be driven from JS: feed guest PCM bytes + HDA `SDnFMT`,
/// and it will decode to stereo `f32`, resample to the AudioContext rate, and
/// write into the shared ring buffer consumed by the AudioWorklet.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct HdaPcmWriter {
    dst_sample_rate_hz: u32,
    resampler: LinearResampler,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl HdaPcmWriter {
    #[wasm_bindgen(constructor)]
    pub fn new(dst_sample_rate_hz: u32) -> Result<Self, JsValue> {
        if dst_sample_rate_hz == 0 {
            return Err(JsValue::from_str("dst_sample_rate_hz must be non-zero"));
        }
        Ok(Self {
            dst_sample_rate_hz,
            resampler: LinearResampler::new(dst_sample_rate_hz, dst_sample_rate_hz),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn dst_sample_rate_hz(&self) -> u32 {
        self.dst_sample_rate_hz
    }

    pub fn set_dst_sample_rate_hz(&mut self, dst_sample_rate_hz: u32) -> Result<(), JsValue> {
        if dst_sample_rate_hz == 0 {
            return Err(JsValue::from_str("dst_sample_rate_hz must be non-zero"));
        }
        self.dst_sample_rate_hz = dst_sample_rate_hz;
        let src = self.resampler.src_rate_hz();
        self.resampler.reset_rates(src, dst_sample_rate_hz);
        Ok(())
    }

    pub fn reset(&mut self) {
        let src = self.resampler.src_rate_hz();
        self.resampler.reset_rates(src, self.dst_sample_rate_hz);
    }

    /// Decode HDA PCM bytes into stereo f32, resample, then write into the ring buffer.
    ///
    /// Returns the number of frames written to the ring buffer.
    pub fn push_hda_pcm_bytes(
        &mut self,
        bridge: &WorkletBridge,
        hda_format: u16,
        pcm_bytes: &[u8],
    ) -> Result<u32, JsValue> {
        if bridge.channel_count() != 2 {
            return Err(JsValue::from_str(
                "WorkletBridge channel_count must be 2 for HdaPcmWriter (stereo output)",
            ));
        }

        let fmt = StreamFormat::from_hda_format(hda_format);
        match fmt.bits_per_sample {
            8 | 16 | 20 | 24 | 32 => {}
            other => {
                return Err(JsValue::from_str(&format!(
                    "Unsupported bits_per_sample in HDA format: {other}"
                )));
            }
        }

        if fmt.sample_rate_hz == 0 || self.dst_sample_rate_hz == 0 {
            return Ok(0);
        }

        if fmt.sample_rate_hz != self.resampler.src_rate_hz() || self.dst_sample_rate_hz != self.resampler.dst_rate_hz()
        {
            self.resampler.reset_rates(fmt.sample_rate_hz, self.dst_sample_rate_hz);
        }

        let decoded = decode_pcm_to_stereo_f32(pcm_bytes, fmt);
        if decoded.is_empty() {
            return Ok(0);
        }
        self.resampler.push_source_frames(&decoded);

        let capacity = bridge.capacity_frames();
        let level = bridge.buffer_level_frames();
        let free_frames = capacity.saturating_sub(level);
        if free_frames == 0 {
            return Ok(0);
        }

        let out = self
            .resampler
            .produce_interleaved_stereo(free_frames as usize);
        Ok(bridge.write_f32_interleaved(&out))
    }
}

#[wasm_bindgen]
pub struct AeroApi {
    version: String,
}

#[wasm_bindgen]
impl AeroApi {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn version(&self) -> String {
        self.version.clone()
    }
}

#[wasm_bindgen]
pub struct DemoVm {
    inner: aero_vm::Vm,
}

#[wasm_bindgen]
impl DemoVm {
    #[wasm_bindgen(constructor)]
    pub fn new(ram_size_bytes: u32) -> Self {
        Self {
            inner: aero_vm::Vm::new(ram_size_bytes as usize),
        }
    }

    pub fn run_steps(&mut self, steps: u32) {
        self.inner.run_steps(steps as u64);
    }

    pub fn serial_output(&self) -> Vec<u8> {
        self.inner.serial_output().to_vec()
    }

    pub fn snapshot_full(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .take_snapshot_full()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn snapshot_dirty(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .take_snapshot_dirty()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.inner
            .restore_snapshot_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn snapshot_full_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        aero_snapshot::save_snapshot(&mut file, &mut self.inner, aero_snapshot::SaveOptions::default())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn snapshot_dirty_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        let mut options = aero_snapshot::SaveOptions::default();
        options.ram.mode = aero_snapshot::RamMode::Dirty;

        aero_snapshot::save_snapshot(&mut file, &mut self.inner, options)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::open(&path, false)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        aero_snapshot::restore_snapshot(&mut file, &mut self.inner)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }
}
