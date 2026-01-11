#![cfg_attr(feature = "wasm-threaded", feature(thread_local))]

use wasm_bindgen::prelude::*;

#[cfg(any(target_arch = "wasm32", test))]
mod demo_renderer;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::worklet_bridge::WorkletBridge;

#[cfg(target_arch = "wasm32")]
use aero_opfs::OpfsSyncFile;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::mic_bridge::MicBridge;

#[cfg(target_arch = "wasm32")]
use js_sys::{SharedArrayBuffer, Uint8Array};

#[cfg(target_arch = "wasm32")]
use aero_audio::pcm::{LinearResampler, StreamFormat, decode_pcm_to_stereo_f32};

#[cfg(target_arch = "wasm32")]
use aero_usb::{
    hid::{GamepadReport, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse},
    usb::{UsbDevice, UsbHandshake},
};

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

// -------------------------------------------------------------------------------------------------
// Guest RAM vs runtime layout contract
// -------------------------------------------------------------------------------------------------

/// WebAssembly linear memory page size (wasm32 / wasm64).
#[cfg(target_arch = "wasm32")]
const WASM_PAGE_BYTES: u64 = 64 * 1024;

/// Max pages addressable by wasm32 (2^32 bytes).
#[cfg(target_arch = "wasm32")]
const MAX_WASM32_PAGES: u64 = 65536;

/// Bytes reserved at the bottom of the linear memory for the Rust/WASM runtime.
///
/// Guest physical address 0 maps to `guest_base = align_up(RUNTIME_RESERVED_BYTES, 64KiB)`.
///
/// NOTE: Keep this in sync with `web/src/runtime/shared_layout.ts` (`RUNTIME_RESERVED_BYTES`).
#[cfg(target_arch = "wasm32")]
const RUNTIME_RESERVED_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

#[cfg(target_arch = "wasm32")]
fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    let rem = value % alignment;
    if rem == 0 {
        value
    } else {
        value + (alignment - rem)
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct GuestRamLayout {
    guest_base: u32,
    guest_size: u32,
    runtime_reserved: u32,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl GuestRamLayout {
    #[wasm_bindgen(getter)]
    pub fn guest_base(&self) -> u32 {
        self.guest_base
    }

    #[wasm_bindgen(getter)]
    pub fn guest_size(&self) -> u32 {
        self.guest_size
    }

    #[wasm_bindgen(getter)]
    pub fn runtime_reserved(&self) -> u32 {
        self.runtime_reserved
    }
}

/// Compute the in-memory guest RAM mapping for a desired guest size.
///
/// This must stay deterministic and stable across the single-threaded + threaded WASM builds.
///
/// Note: `desired_bytes` is a `u32`, so callers must clamp values to `<= 0xFFFF_FFFF`
/// (4GiB-1). (4GiB itself does not fit in a `u32`.)
///
/// The contract is:
/// - Bytes `[0, guest_base)` are reserved for the Rust/WASM runtime (stack, heap, TLS, etc.).
/// - Guest physical address 0 maps to byte offset `guest_base` inside the wasm linear memory.
/// - Guest RAM occupies `[guest_base, guest_base + guest_size)`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn guest_ram_layout(desired_bytes: u32) -> GuestRamLayout {
    let guest_base = align_up(RUNTIME_RESERVED_BYTES, WASM_PAGE_BYTES);
    let base_pages = guest_base / WASM_PAGE_BYTES;

    // `desired_bytes` is u32 so it cannot represent 4GiB; align up safely in u64.
    let desired_bytes_aligned = align_up(desired_bytes as u64, WASM_PAGE_BYTES);
    let desired_pages = desired_bytes_aligned / WASM_PAGE_BYTES;

    let total_pages = (base_pages + desired_pages).min(MAX_WASM32_PAGES);
    let guest_pages = total_pages.saturating_sub(base_pages);
    let guest_size = guest_pages * WASM_PAGE_BYTES;

    GuestRamLayout {
        guest_base: guest_base as u32,
        guest_size: guest_size as u32,
        runtime_reserved: guest_base as u32,
    }
}

#[wasm_bindgen]
pub fn sum(a: i32, b: i32) -> i32 {
    a + b
}

/// Store a `u32` directly into the module's linear memory at `offset`.
///
/// This is a tiny, allocation-free ABI surface used by the web runtime to
/// sanity-check that a provided `WebAssembly.Memory` is actually wired as the
/// WASM instance's linear memory (imported+exported memory builds).
#[wasm_bindgen]
pub fn mem_store_u32(offset: u32, value: u32) {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::ptr::write_unaligned(offset as *mut u32, value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (offset, value);
    }
}

/// Load a `u32` directly from the module's linear memory at `offset`.
///
/// See [`mem_store_u32`].
#[wasm_bindgen]
pub fn mem_load_u32(offset: u32) -> u32 {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        core::ptr::read_unaligned(offset as *const u32)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = offset;
        0
    }
}

/// Render an animated RGBA8888 test pattern into the module's linear memory.
///
/// The web runtime uses this as a cheap "CPU demo" hot loop: JS drives the frame
/// cadence, WASM writes pixels into shared `guestMemory`, and JS performs a
/// single bulk copy into the presentation framebuffer.
#[wasm_bindgen]
pub fn demo_render_rgba8888(
    dst_offset: u32,
    width: u32,
    height: u32,
    stride_bytes: u32,
    now_ms: f64,
) -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        if dst_offset == 0 {
            return 0;
        }

        // Convert the current memory size (in 64KiB pages) into a byte length.
        // Use `u64` so `65536 pages * 64KiB` doesn't overflow on wasm32.
        let pages = unsafe { core::arch::wasm32::memory_size(0) } as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);
        let dst_offset_u64 = dst_offset as u64;
        if dst_offset_u64 >= mem_bytes {
            return 0;
        }

        let max_len = (mem_bytes - dst_offset_u64).min(usize::MAX as u64) as usize;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(dst_offset as *mut u8, max_len);
            demo_renderer::render_rgba8888(dst, width, height, stride_bytes, now_ms)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (dst_offset, width, height, stride_bytes, now_ms);
        0
    }
}

/// Tiny WASM-side USB HID glue used by the browser I/O worker.
///
/// This object is intentionally self-contained: it exposes stateful "input
/// injection" methods (`keyboard_event`, `mouse_move`, ...) and optional debug
/// drains that return the raw boot-protocol reports for tests.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct UsbHidBridge {
    keyboard: UsbHidKeyboard,
    mouse: UsbHidMouse,
    gamepad: UsbHidGamepad,
    mouse_buttons: u8,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl UsbHidBridge {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            keyboard: UsbHidKeyboard::new(),
            mouse: UsbHidMouse::new(),
            gamepad: UsbHidGamepad::new(),
            mouse_buttons: 0,
        }
    }

    /// Inject a single HID keyboard usage event.
    pub fn keyboard_event(&mut self, usage: u8, pressed: bool) {
        self.keyboard.key_event(usage, pressed);
    }

    /// Inject a relative mouse movement event.
    ///
    /// `dy` uses HID convention: positive is down.
    pub fn mouse_move(&mut self, dx: i32, dy: i32) {
        self.mouse.movement(dx, dy);
    }

    /// Set mouse button state as a bitmask (bit0=left, bit1=right, bit2=middle).
    pub fn mouse_buttons(&mut self, buttons: u8) {
        let next = buttons & 0x07;
        let prev = self.mouse_buttons;
        let delta = prev ^ next;

        for bit in [0x01, 0x02, 0x04] {
            if (delta & bit) != 0 {
                self.mouse.button_event(bit, (next & bit) != 0);
            }
        }

        self.mouse_buttons = next;
    }

    /// Inject a mouse wheel movement (positive = wheel up).
    pub fn mouse_wheel(&mut self, delta: i32) {
        self.mouse.wheel(delta);
    }

    /// Inject an 8-byte USB HID gamepad report (packed into two 32-bit words).
    ///
    /// The packed format matches `web/src/input/gamepad.ts`:
    /// - `packed_lo`: bytes 0..3 (little-endian)
    /// - `packed_hi`: bytes 4..7 (little-endian)
    pub fn gamepad_report(&mut self, packed_lo: u32, packed_hi: u32) {
        let b0 = (packed_lo & 0xff) as u8;
        let b1 = ((packed_lo >> 8) & 0xff) as u8;
        let b2 = ((packed_lo >> 16) & 0xff) as u8;
        let b3 = ((packed_lo >> 24) & 0xff) as u8;
        let b4 = (packed_hi & 0xff) as u8;
        let b5 = ((packed_hi >> 8) & 0xff) as u8;
        let b6 = ((packed_hi >> 16) & 0xff) as u8;

        self.gamepad.set_report(GamepadReport {
            buttons: u16::from_le_bytes([b0, b1]),
            hat: b2,
            x: b3 as i8,
            y: b4 as i8,
            rx: b5 as i8,
            ry: b6 as i8,
        });
    }

    /// Drain the next 8-byte boot keyboard report (or return `null` if none).
    pub fn drain_next_keyboard_report(&mut self) -> JsValue {
        let mut buf = [0u8; 8];
        match self.keyboard.handle_in(1, &mut buf) {
            UsbHandshake::Ack { bytes } if bytes > 0 => Uint8Array::from(&buf[..bytes]).into(),
            _ => JsValue::NULL,
        }
    }

    /// Drain the next mouse report (or return `null` if none).
    ///
    /// In report protocol this is 4 bytes: buttons, dx, dy, wheel.
    pub fn drain_next_mouse_report(&mut self) -> JsValue {
        let mut buf = [0u8; 4];
        match self.mouse.handle_in(1, &mut buf) {
            UsbHandshake::Ack { bytes } if bytes > 0 => Uint8Array::from(&buf[..bytes]).into(),
            _ => JsValue::NULL,
        }
    }

    /// Drain the next 8-byte gamepad report (or return `null` if none).
    pub fn drain_next_gamepad_report(&mut self) -> JsValue {
        let mut buf = [0u8; 8];
        match self.gamepad.handle_in(1, &mut buf) {
            UsbHandshake::Ack { bytes } if bytes > 0 => Uint8Array::from(&buf[..bytes]).into(),
            _ => JsValue::NULL,
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn create_worklet_bridge(
    capacity_frames: u32,
    channel_count: u32,
) -> Result<WorkletBridge, JsValue> {
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
pub fn attach_mic_bridge(sab: SharedArrayBuffer) -> Result<MicBridge, JsValue> {
    MicBridge::from_shared_buffer(sab)
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

        if fmt.sample_rate_hz != self.resampler.src_rate_hz()
            || self.dst_sample_rate_hz != self.resampler.dst_rate_hz()
        {
            self.resampler
                .reset_rates(fmt.sample_rate_hz, self.dst_sample_rate_hz);
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

        self.inner
            .save_snapshot_full_to(&mut file)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn snapshot_dirty_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.inner
            .save_snapshot_dirty_to(&mut file)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::open(&path, false)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.inner
            .restore_snapshot_from(&mut file)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close().map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }
}
