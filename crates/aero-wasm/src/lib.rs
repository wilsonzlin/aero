#![cfg_attr(feature = "wasm-threaded", feature(thread_local))]

use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::worklet_bridge::WorkletBridge;

#[cfg(target_arch = "wasm32")]
use js_sys::SharedArrayBuffer;

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
