#![cfg_attr(
    all(target_arch = "wasm32", feature = "wasm-threaded"),
    feature(thread_local)
)]

use wasm_bindgen::prelude::*;

// Re-export Aero IPC SharedArrayBuffer ring helpers so the generated `aero-wasm`
// wasm-pack package exposes them to JS (both threaded + single builds).
#[cfg(target_arch = "wasm32")]
pub use aero_ipc::wasm::{SharedRingBuffer, open_ring_by_kind};

#[cfg(target_arch = "wasm32")]
mod guest_layout;

#[cfg(target_arch = "wasm32")]
mod runtime_alloc;

#[cfg(target_arch = "wasm32")]
mod vm;

#[cfg(target_arch = "wasm32")]
pub use vm::WasmVm;

#[cfg(target_arch = "wasm32")]
mod tiered_vm;
#[cfg(target_arch = "wasm32")]
pub use tiered_vm::WasmTieredVm;

#[cfg(target_arch = "wasm32")]
mod vm_snapshot_builder;
#[cfg(target_arch = "wasm32")]
pub use vm_snapshot_builder::{
    vm_snapshot_restore, vm_snapshot_restore_from_opfs, vm_snapshot_save, vm_snapshot_save_to_opfs,
};

#[cfg(any(target_arch = "wasm32", test))]
mod demo_renderer;

#[cfg(target_arch = "wasm32")]
mod webusb_uhci_passthrough_harness;

#[cfg(target_arch = "wasm32")]
pub use webusb_uhci_passthrough_harness::WebUsbUhciPassthroughHarness;
#[cfg(target_arch = "wasm32")]
mod uhci_runtime;
#[cfg(target_arch = "wasm32")]
pub use uhci_runtime::UhciRuntime;

#[cfg(target_arch = "wasm32")]
mod worker_vm_snapshot;
#[cfg(target_arch = "wasm32")]
pub use worker_vm_snapshot::WorkerVmSnapshot;

#[cfg(target_arch = "wasm32")]
mod uhci_controller_bridge;

#[cfg(target_arch = "wasm32")]
pub use uhci_controller_bridge::UhciControllerBridge;

#[cfg(target_arch = "wasm32")]
mod e1000_bridge;
#[cfg(target_arch = "wasm32")]
pub use e1000_bridge::E1000Bridge;

#[cfg(target_arch = "wasm32")]
mod webusb_uhci_bridge;

#[cfg(target_arch = "wasm32")]
pub use webusb_uhci_bridge::WebUsbUhciBridge;

mod virtio_input_bridge;
pub use virtio_input_bridge::VirtioInputPciDeviceCore;
#[cfg(target_arch = "wasm32")]
pub use virtio_input_bridge::VirtioInputPciDevice;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::worklet_bridge::WorkletBridge;

#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
use aero_shared::shared_framebuffer::{
    SharedFramebuffer, SharedFramebufferLayout, SharedFramebufferWriter,
};

#[cfg(target_arch = "wasm32")]
use aero_opfs::OpfsSyncFile;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::mic_bridge::MicBridge;

#[cfg(target_arch = "wasm32")]
use js_sys::{Object, Reflect, SharedArrayBuffer, Uint8Array};

#[cfg(target_arch = "wasm32")]
use aero_audio::hda::HdaController;

#[cfg(target_arch = "wasm32")]
use aero_audio::mem::{GuestMemory, MemoryAccess};

#[cfg(target_arch = "wasm32")]
use aero_audio::pcm::{LinearResampler, StreamFormat, decode_pcm_to_stereo_f32_into};

#[cfg(target_arch = "wasm32")]
use aero_audio::sink::AudioSink;

#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(target_arch = "wasm32")]
use aero_usb::{
    SetupPacket, UsbDeviceModel, UsbInResult,
    hid::passthrough::UsbHidPassthroughHandle,
    hid::webhid,
    hid::{GamepadReport, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse},
};

#[cfg(any(target_arch = "wasm32", test))]
use aero_usb::passthrough::{
    ControlResponse, SetupPacket as HostSetupPacket, UsbHostAction, UsbHostCompletion,
    UsbHostCompletionIn, UsbPassthroughDevice,
};

#[cfg(target_arch = "wasm32")]
use aero_usb::passthrough::PendingSummary as UsbPassthroughPendingSummary;

// wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
// `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
// by the linker when there is at least one TLS variable. We keep a tiny TLS
// slot behind a cargo feature enabled only for the threaded build.
#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
#[thread_local]
static TLS_DUMMY: u8 = 0;

#[wasm_bindgen(start)]
pub fn wasm_start() {
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
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
    let guest_base = guest_layout::align_up(
        guest_layout::RUNTIME_RESERVED_BYTES,
        guest_layout::WASM_PAGE_BYTES,
    );
    let base_pages = guest_base / guest_layout::WASM_PAGE_BYTES;

    // `desired_bytes` is u32 so it cannot represent 4GiB; align up safely in u64.
    let desired_bytes_aligned =
        guest_layout::align_up(desired_bytes as u64, guest_layout::WASM_PAGE_BYTES);
    let desired_pages = desired_bytes_aligned / guest_layout::WASM_PAGE_BYTES;

    let total_pages = (base_pages + desired_pages).min(guest_layout::MAX_WASM32_PAGES);
    let guest_pages = total_pages.saturating_sub(base_pages);
    let guest_size = guest_pages * guest_layout::WASM_PAGE_BYTES;

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

// -------------------------------------------------------------------------------------------------
// WebHID report descriptor synthesis
// -------------------------------------------------------------------------------------------------

/// Synthesize a USB HID report descriptor (binary bytes) from WebHID-normalized collection metadata.
///
/// This is the Rust-side core implementation used by the WASM export
/// [`synthesize_webhid_report_descriptor`] and by native unit tests in this crate.
pub fn synthesize_webhid_report_descriptor_bytes(
    collections: &[aero_usb::hid::webhid::HidCollectionInfo],
) -> Result<Vec<u8>, aero_usb::hid::webhid::HidDescriptorSynthesisError> {
    aero_usb::hid::webhid::synthesize_report_descriptor(collections)
}

#[cfg(target_arch = "wasm32")]
fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

/// WASM export: synthesize a HID report descriptor from WebHID-normalized metadata.
///
/// `collections_json` must be the normalized output of `normalizeCollections()` from
/// `web/src/hid/webhid_normalize.ts` (array of objects with camelCase fields).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn synthesize_webhid_report_descriptor(
    collections_json: JsValue,
) -> Result<Uint8Array, JsValue> {
    let collections_json_str = js_sys::JSON::stringify(&collections_json)
        .map_err(|err| {
            js_error(&format!(
                "Invalid WebHID collection schema (stringify failed): {err:?}"
            ))
        })?
        .as_string()
        .ok_or_else(|| {
            js_error("Invalid WebHID collection schema (stringify returned non-string)")
        })?;

    // Improve deserialization errors with a precise path into the collections metadata.
    // Example: `at [0].inputReports[0].items[0].reportSize: invalid type ...`.
    let mut deserializer = serde_json::Deserializer::from_str(&collections_json_str);
    let collections: Vec<aero_usb::hid::webhid::HidCollectionInfo> =
        serde_path_to_error::deserialize(&mut deserializer)
            .map_err(|err| js_error(&format!("Invalid WebHID collection schema: {err}")))?;

    let bytes = synthesize_webhid_report_descriptor_bytes(&collections).map_err(|err| {
        js_error(&format!(
            "Failed to synthesize HID report descriptor: {err}"
        ))
    })?;

    Ok(Uint8Array::from(bytes.as_slice()))
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
        if width == 0 || height == 0 {
            return 0;
        }

        // Convert the current memory size (in 64KiB pages) into a byte length.
        // Use `u64` so `65536 pages * 64KiB` doesn't overflow on wasm32.
        let pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);
        let dst_offset_u64 = dst_offset as u64;
        if dst_offset_u64 >= mem_bytes {
            return 0;
        }

        // Bound the mutable slice to *only* the region we may write to, rather than
        // aliasing the rest of linear memory.
        let mem_len = (mem_bytes - dst_offset_u64).min(usize::MAX as u64) as usize;
        let row_bytes = match (width as usize).checked_mul(4) {
            Some(v) => v,
            None => return 0,
        };

        let mut stride = stride_bytes as usize;
        if stride < row_bytes {
            stride = row_bytes;
        }
        if stride == 0 {
            return 0;
        }

        let max_height = mem_len / stride;
        let draw_height = (height as usize).min(max_height);
        if draw_height == 0 {
            return 0;
        }

        let slice_len = stride * draw_height;
        unsafe {
            let dst = core::slice::from_raw_parts_mut(dst_offset as *mut u8, slice_len);
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
        fn configure(dev: &mut impl UsbDeviceModel) {
            // Our HID device models behave like real USB devices and only produce
            // interrupt IN data after the host sets a non-zero configuration.
            //
            // The web runtime uses `UsbHidBridge` as a lightweight "report
            // generator" in tests and in the I/O worker, so configure the devices
            // eagerly to make `drain_next_*_report()` immediately usable.
            let _ = dev.handle_control_request(
                SetupPacket {
                    bm_request_type: 0x00,
                    b_request: 0x09, // SET_CONFIGURATION
                    w_value: 1,
                    w_index: 0,
                    w_length: 0,
                },
                None,
            );
        }

        let mut keyboard = UsbHidKeyboard::new();
        configure(&mut keyboard);

        let mut mouse = UsbHidMouse::new();
        configure(&mut mouse);

        let mut gamepad = UsbHidGamepad::new();
        configure(&mut gamepad);

        Self {
            keyboard,
            mouse,
            gamepad,
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
        match self.keyboard.handle_in_transfer(0x81, 8) {
            UsbInResult::Data(data) if !data.is_empty() => Uint8Array::from(data.as_slice()).into(),
            _ => JsValue::NULL,
        }
    }

    /// Drain the next mouse report (or return `null` if none).
    ///
    /// In report protocol this is 4 bytes: buttons, dx, dy, wheel.
    pub fn drain_next_mouse_report(&mut self) -> JsValue {
        match self.mouse.handle_in_transfer(0x81, 4) {
            UsbInResult::Data(data) if !data.is_empty() => Uint8Array::from(data.as_slice()).into(),
            _ => JsValue::NULL,
        }
    }

    /// Drain the next 8-byte gamepad report (or return `null` if none).
    pub fn drain_next_gamepad_report(&mut self) -> JsValue {
        match self.gamepad.handle_in_transfer(0x81, 8) {
            UsbInResult::Data(data) if !data.is_empty() => Uint8Array::from(data.as_slice()).into(),
            _ => JsValue::NULL,
        }
    }
}

// -------------------------------------------------------------------------------------------------
// WebHID passthrough (physical HID devices -> guest-visible USB HID model)
// -------------------------------------------------------------------------------------------------

/// Generic USB HID passthrough device wrapper.
///
/// This is the low-level building block: callers provide a fully-formed HID
/// report descriptor (`report_descriptor_bytes`) and specify whether the device
/// needs an interrupt OUT endpoint (`has_interrupt_out`).
///
/// Higher-level helpers (like [`WebHidPassthroughBridge`]) can synthesize the
/// report descriptor from WebHID metadata and then construct this device.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct UsbHidPassthroughBridge {
    device: UsbHidPassthroughHandle,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl UsbHidPassthroughBridge {
    #[allow(clippy::too_many_arguments)]
    #[wasm_bindgen(constructor)]
    pub fn new(
        vendor_id: u16,
        product_id: u16,
        manufacturer: Option<String>,
        product: Option<String>,
        serial: Option<String>,
        report_descriptor_bytes: Vec<u8>,
        has_interrupt_out: bool,
        interface_subclass: Option<u8>,
        interface_protocol: Option<u8>,
    ) -> Self {
        let device = UsbHidPassthroughHandle::new(
            vendor_id,
            product_id,
            manufacturer.unwrap_or_else(|| "WebHID".to_string()),
            product.unwrap_or_else(|| "WebHID HID Device".to_string()),
            serial,
            report_descriptor_bytes,
            has_interrupt_out,
            None,
            interface_subclass,
            interface_protocol,
        );
        Self { device }
    }

    pub fn push_input_report(&mut self, report_id: u32, data: &[u8]) -> Result<(), JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        self.device.push_input_report(report_id, data);
        Ok(())
    }

    /// Drain the next pending guest -> device HID report request.
    ///
    /// Returns `null` when no report is pending.
    pub fn drain_next_output_report(&mut self) -> JsValue {
        let Some(report) = self.device.pop_output_report() else {
            return JsValue::NULL;
        };

        let report_type = match report.report_type {
            2 => "output",
            3 => "feature",
            _ => "output",
        };

        let obj = Object::new();
        // These Reflect::set calls should be infallible for a fresh object with string keys.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportType"),
            &JsValue::from_str(report_type),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportId"),
            &JsValue::from_f64(f64::from(report.report_id)),
        );
        let data = Uint8Array::from(report.data.as_slice());
        let _ = Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref());
        obj.into()
    }

    /// Whether the guest has configured the USB device (SET_CONFIGURATION != 0).
    pub fn configured(&self) -> bool {
        self.device.configured()
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct WebHidPassthroughBridge {
    device: UsbHidPassthroughHandle,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl WebHidPassthroughBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(
        vendor_id: u16,
        product_id: u16,
        manufacturer: Option<String>,
        product: Option<String>,
        serial: Option<String>,
        collections: JsValue,
    ) -> Result<Self, JsValue> {
        let collections: Vec<webhid::HidCollectionInfo> =
            serde_path_to_error::deserialize(serde_wasm_bindgen::Deserializer::from(collections))
                .map_err(|err| js_error(&format!("Invalid WebHID collection schema: {err}")))?;

        let report_descriptor = webhid::synthesize_report_descriptor(&collections)
            .map_err(|e| js_error(&format!("failed to synthesize HID report descriptor: {e}")))?;

        let has_interrupt_out = collections_have_output_reports(&collections);

        let device = UsbHidPassthroughHandle::new(
            vendor_id,
            product_id,
            manufacturer.unwrap_or_else(|| "WebHID".to_string()),
            product.unwrap_or_else(|| "WebHID HID Device".to_string()),
            serial,
            report_descriptor,
            has_interrupt_out,
            None,
            None,
            None,
        );

        Ok(Self { device })
    }

    pub fn push_input_report(&mut self, report_id: u32, data: &[u8]) -> Result<(), JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        self.device.push_input_report(report_id, data);
        Ok(())
    }

    /// Drain the next pending guest -> device HID report request.
    ///
    /// Returns `null` when no report is pending.
    pub fn drain_next_output_report(&mut self) -> JsValue {
        let Some(report) = self.device.pop_output_report() else {
            return JsValue::NULL;
        };

        let report_type = match report.report_type {
            2 => "output",
            3 => "feature",
            _ => "output",
        };

        let obj = Object::new();
        // These Reflect::set calls should be infallible for a fresh object with string keys.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportType"),
            &JsValue::from_str(report_type),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportId"),
            &JsValue::from_f64(f64::from(report.report_id)),
        );
        let data = Uint8Array::from(report.data.as_slice());
        let _ = Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref());
        obj.into()
    }

    /// Whether the guest has configured the USB device (SET_CONFIGURATION != 0).
    pub fn configured(&self) -> bool {
        self.device.configured()
    }
}

#[cfg(target_arch = "wasm32")]
impl WebHidPassthroughBridge {
    pub(crate) fn as_usb_device(&self) -> UsbHidPassthroughHandle {
        self.device.clone()
    }
}

#[cfg(target_arch = "wasm32")]
impl UsbHidPassthroughBridge {
    pub(crate) fn as_usb_device(&self) -> UsbHidPassthroughHandle {
        self.device.clone()
    }
}

#[cfg(target_arch = "wasm32")]
fn collections_have_output_reports(collections: &[webhid::HidCollectionInfo]) -> bool {
    fn walk(col: &webhid::HidCollectionInfo) -> bool {
        if !col.output_reports.is_empty() {
            return true;
        }
        col.children.iter().any(walk)
    }

    collections.iter().any(walk)
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

/// WASM export for the WebUSB passthrough device model.
///
/// This object owns a [`UsbPassthroughDevice`] instance, drains queued host actions for the
/// TypeScript WebUSB executor, and accepts completions back from the host.
///
/// The canonical host action/completion wire contract is defined by `aero_usb::passthrough`
/// and documented in `docs/adr/0015-canonical-usb-stack.md` + `docs/webusb-passthrough.md`.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct UsbPassthroughBridge {
    inner: UsbPassthroughDevice,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl UsbPassthroughBridge {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: UsbPassthroughDevice::new(),
        }
    }

    /// Drain all queued host actions as plain JS objects.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions = self.inner.drain_actions();
        if actions.is_empty() {
            // Avoid allocating a fresh empty JS array on every poll tick when there are no
            // pending actions (the worker runtime treats `null`/`undefined` as "no work").
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Push a single host completion into the passthrough device.
    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner.push_completion(completion);
        Ok(())
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn pending_summary(&self) -> Result<JsValue, JsValue> {
        let summary: UsbPassthroughPendingSummary = self.inner.pending_summary();
        serde_wasm_bindgen::to_value(&summary).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

// -------------------------------------------------------------------------------------------------
// WebUSB passthrough demo driver
// -------------------------------------------------------------------------------------------------

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Default)]
struct UsbPassthroughDemoCore {
    device: UsbPassthroughDevice,
    pending_control: Option<HostSetupPacket>,
    ready_result: Option<UsbHostCompletionIn>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl UsbPassthroughDemoCore {
    fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        self.device.reset();
        self.pending_control = None;
        self.ready_result = None;
    }

    fn queue_get_device_descriptor(&mut self, len: u16) {
        let setup = HostSetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: len,
        };
        self.queue_control_in(setup);
    }

    fn queue_get_config_descriptor(&mut self, len: u16) {
        let setup = HostSetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: len,
        };
        self.queue_control_in(setup);
    }

    fn queue_control_in(&mut self, setup: HostSetupPacket) {
        self.pending_control = Some(setup);
        match self.device.handle_control_request(setup, None) {
            ControlResponse::Nak => {}
            ControlResponse::Data(data) => {
                self.pending_control = None;
                self.ready_result = Some(UsbHostCompletionIn::Success { data });
            }
            ControlResponse::Stall => {
                self.pending_control = None;
                self.ready_result = Some(UsbHostCompletionIn::Stall);
            }
            ControlResponse::Timeout => {
                self.pending_control = None;
                self.ready_result = Some(UsbHostCompletionIn::Error {
                    message: "timeout".to_string(),
                });
            }
            ControlResponse::Ack => {
                self.pending_control = None;
                self.ready_result = Some(UsbHostCompletionIn::Error {
                    message: "unexpected ACK for control-in request".to_string(),
                });
            }
        }
    }

    fn drain_actions(&mut self) -> Vec<UsbHostAction> {
        self.device.drain_actions()
    }

    fn push_completion(&mut self, completion: UsbHostCompletion) {
        self.device.push_completion(completion);
    }

    fn poll_last_result(&mut self) -> Option<UsbHostCompletionIn> {
        if let Some(result) = self.ready_result.take() {
            return Some(result);
        }

        let setup = self.pending_control?;
        match self.device.handle_control_request(setup, None) {
            ControlResponse::Nak => None,
            ControlResponse::Data(data) => {
                self.pending_control = None;
                Some(UsbHostCompletionIn::Success { data })
            }
            ControlResponse::Stall => {
                self.pending_control = None;
                Some(UsbHostCompletionIn::Stall)
            }
            ControlResponse::Timeout => {
                self.pending_control = None;
                Some(UsbHostCompletionIn::Error {
                    message: "timeout".to_string(),
                })
            }
            ControlResponse::Ack => {
                self.pending_control = None;
                Some(UsbHostCompletionIn::Error {
                    message: "unexpected ACK for control-in request".to_string(),
                })
            }
        }
    }
}

/// WASM export: minimal driver that queues a handful of standard GET_DESCRIPTOR requests to prove
/// the WebUSB action↔completion contract end-to-end.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct UsbPassthroughDemo {
    inner: UsbPassthroughDemoCore,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl UsbPassthroughDemo {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            inner: UsbPassthroughDemoCore::new(),
        }
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn queue_get_device_descriptor(&mut self, len: u16) {
        self.inner.queue_get_device_descriptor(len);
    }

    pub fn queue_get_config_descriptor(&mut self, len: u16) {
        self.inner.queue_get_config_descriptor(len);
    }

    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions = self.inner.drain_actions();
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.inner.push_completion(completion);
        Ok(())
    }

    pub fn poll_last_result(&mut self) -> Result<JsValue, JsValue> {
        let result = self.inner.poll_last_result();
        serde_wasm_bindgen::to_value(&result).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct SineTone {
    phase: f32,
    scratch: Vec<f32>,
}

#[cfg(test)]
mod usb_passthrough_demo_tests {
    use super::*;

    #[test]
    fn get_device_descriptor_queues_one_control_in_action() {
        let mut demo = UsbPassthroughDemoCore::new();
        demo.queue_get_device_descriptor(18);

        let actions = demo.drain_actions();
        assert_eq!(actions.len(), 1);

        let action = actions.into_iter().next().unwrap();
        match action {
            UsbHostAction::ControlIn { setup, .. } => {
                assert_eq!(setup.bm_request_type, 0x80);
                assert_eq!(setup.b_request, 0x06);
                assert_eq!(setup.w_value, 0x0100);
                assert_eq!(setup.w_index, 0);
                assert_eq!(setup.w_length, 18);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn pushing_completion_then_polling_returns_bytes() {
        let mut demo = UsbPassthroughDemoCore::new();
        demo.queue_get_device_descriptor(4);

        let actions = demo.drain_actions();
        let action = actions.into_iter().next().expect("queued action");
        let id = match action {
            UsbHostAction::ControlIn { id, .. } => id,
            other => panic!("unexpected action: {other:?}"),
        };

        demo.push_completion(UsbHostCompletion::ControlIn {
            id,
            result: UsbHostCompletionIn::Success {
                data: vec![1, 2, 3, 4, 5, 6],
            },
        });

        let result = demo.poll_last_result();
        assert_eq!(
            result,
            Some(UsbHostCompletionIn::Success {
                data: vec![1, 2, 3, 4],
            })
        );
        assert_eq!(demo.poll_last_result(), None);
    }

    #[test]
    fn reset_then_queue_get_config_descriptor_emits_control_in() {
        let mut demo = UsbPassthroughDemoCore::new();
        demo.queue_get_device_descriptor(18);
        assert!(!demo.drain_actions().is_empty());

        demo.reset();
        demo.queue_get_config_descriptor(9);

        let actions = demo.drain_actions();
        assert_eq!(actions.len(), 1);
        let action = actions.into_iter().next().unwrap();
        match action {
            UsbHostAction::ControlIn { setup, .. } => {
                assert_eq!(setup.w_value, 0x0200);
                assert_eq!(setup.w_length, 9);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }
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
    decode_scratch: Vec<[f32; 2]>,
    resample_out_scratch: Vec<f32>,
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
            decode_scratch: Vec::new(),
            resample_out_scratch: Vec::new(),
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

        decode_pcm_to_stereo_f32_into(pcm_bytes, fmt, &mut self.decode_scratch);
        if self.decode_scratch.is_empty() {
            return Ok(0);
        }
        self.resampler.push_source_frames(&self.decode_scratch);

        let capacity = bridge.capacity_frames();
        let level = bridge.buffer_level_frames();
        let free_frames = capacity.saturating_sub(level);
        if free_frames == 0 {
            return Ok(0);
        }

        self.resampler
            .produce_interleaved_stereo_into(free_frames as usize, &mut self.resample_out_scratch);
        Ok(bridge.write_f32_interleaved(&self.resample_out_scratch))
    }
}

/// End-to-end browser demo: drive the real HDA device model and stream its output
/// directly into a Web Audio `AudioWorkletProcessor` ring buffer.
///
/// This wrapper exists purely for the web demo harness; it is not intended to be
/// a stable public API.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct HdaPlaybackDemo {
    hda: HdaController,
    mem: GuestMemory,
    bridge: WorkletBridge,
    host_sample_rate_hz: u32,

    total_frames_produced: u32,
    total_frames_written: u32,
    total_frames_dropped: u32,

    last_tick_requested_frames: u32,
    last_tick_produced_frames: u32,
    last_tick_written_frames: u32,
    last_tick_dropped_frames: u32,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl HdaPlaybackDemo {
    #[wasm_bindgen(constructor)]
    pub fn new(
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
        host_sample_rate: u32,
    ) -> Result<Self, JsValue> {
        if capacity_frames == 0 {
            return Err(JsValue::from_str("capacityFrames must be non-zero"));
        }
        if channel_count != 2 {
            return Err(JsValue::from_str(
                "channelCount must be 2 for HDA demo output (stereo)",
            ));
        }
        if host_sample_rate == 0 {
            return Err(JsValue::from_str("hostSampleRate must be non-zero"));
        }

        let bridge = WorkletBridge::from_shared_buffer(ring_sab, capacity_frames, channel_count)?;

        let hda = HdaController::new_with_output_rate(host_sample_rate);

        // Allocate a small guest-physical memory backing store. The demo programs
        // a short BDL + PCM buffer and loops it forever.
        let mem = GuestMemory::new(0x20_000);

        // Default to a 440Hz tone so the demo works even if callers don't invoke
        // `init_sine_dma()` explicitly.
        let mut demo = Self {
            hda,
            mem,
            bridge,
            host_sample_rate_hz: host_sample_rate,
            total_frames_produced: 0,
            total_frames_written: 0,
            total_frames_dropped: 0,
            last_tick_requested_frames: 0,
            last_tick_produced_frames: 0,
            last_tick_written_frames: 0,
            last_tick_dropped_frames: 0,
        };
        demo.init_sine_dma(440.0, 0.1);
        Ok(demo)
    }

    #[wasm_bindgen(getter)]
    pub fn host_sample_rate_hz(&self) -> u32 {
        self.host_sample_rate_hz
    }

    /// Program a looping DMA buffer containing a simple sine wave.
    pub fn init_sine_dma(&mut self, freq_hz: f32, gain: f32) {
        // Bring controller out of reset (GCTL.CRST).
        self.hda.mmio_write(0x08, 4, 0x1);

        // Configure the codec converter to listen on stream 1, channel 0.
        // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
        let set_stream_ch = (0x706u32 << 8) | 0x10;
        self.hda.codec_mut().execute_verb(2, set_stream_ch);

        // Stream format: 48kHz, 16-bit, 2ch.
        let fmt_raw: u16 = (1 << 4) | 0x1;
        // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
        let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
        self.hda.codec_mut().execute_verb(2, set_fmt);

        // Guest buffer layout.
        let bdl_base = 0x1000u64;
        let pcm_base = 0x2000u64;
        let frames = 48_000usize / 5; // 200ms at 48kHz
        let bytes_per_frame = 4usize; // 16-bit stereo
        let pcm_len_bytes = frames * bytes_per_frame;

        // Fill PCM buffer with a sine wave.
        let sr_hz = 48_000.0f32;
        for n in 0..frames {
            let t = n as f32 / sr_hz;
            let s = (2.0 * core::f32::consts::PI * freq_hz * t).sin() * gain;
            let v = (s * i16::MAX as f32) as i16;
            let off = pcm_base + (n * bytes_per_frame) as u64;
            self.mem.write_u16(off, v as u16);
            self.mem.write_u16(off + 2, v as u16);
        }

        // One BDL entry pointing at the PCM buffer, IOC=1.
        self.mem.write_u64(bdl_base + 0, pcm_base);
        self.mem.write_u32(bdl_base + 8, pcm_len_bytes as u32);
        self.mem.write_u32(bdl_base + 12, 1);

        // Configure stream descriptor 0.
        {
            let sd = self.hda.stream_mut(0);
            sd.bdpl = bdl_base as u32;
            sd.bdpu = 0;
            sd.cbl = pcm_len_bytes as u32;
            sd.lvi = 0;
            sd.fmt = fmt_raw;
            // SRST | RUN | IOCE | stream number 1.
            sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
        }

        // Enable stream interrupts (best-effort; not currently surfaced to JS).
        self.hda.mmio_write(0x20, 4, (1u64 << 31) | 1u64); // INTCTL.GIE + stream0 enable
    }

    /// Advance the HDA device by `frames` worth of host time and push any rendered
    /// samples into the shared AudioWorklet ring buffer.
    ///
    /// Returns the current ring buffer fill level (frames).
    pub fn tick(&mut self, frames: u32) -> u32 {
        self.last_tick_requested_frames = frames;
        self.last_tick_produced_frames = 0;
        self.last_tick_written_frames = 0;
        self.last_tick_dropped_frames = 0;

        if frames == 0 {
            return self.bridge.buffer_level_frames();
        }

        let mut sink = WorkletBridgeStatsSink {
            bridge: &self.bridge,
            channel_count: self.bridge.channel_count(),
            produced_frames: 0,
            written_frames: 0,
            dropped_frames: 0,
        };
        self.hda
            .process_into(&mut self.mem, frames as usize, &mut sink);

        self.last_tick_produced_frames = sink.produced_frames;
        self.last_tick_written_frames = sink.written_frames;
        self.last_tick_dropped_frames = sink.dropped_frames;

        self.total_frames_produced = self
            .total_frames_produced
            .wrapping_add(sink.produced_frames);
        self.total_frames_written = self.total_frames_written.wrapping_add(sink.written_frames);
        self.total_frames_dropped = self.total_frames_dropped.wrapping_add(sink.dropped_frames);

        self.bridge.buffer_level_frames()
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

#[cfg(target_arch = "wasm32")]
struct WorkletBridgeStatsSink<'a> {
    bridge: &'a WorkletBridge,
    channel_count: u32,
    produced_frames: u32,
    written_frames: u32,
    dropped_frames: u32,
}

#[cfg(target_arch = "wasm32")]
impl<'a> AudioSink for WorkletBridgeStatsSink<'a> {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        let requested_frames = (samples.len() as u32) / self.channel_count;
        if requested_frames == 0 {
            return;
        }
        let written = self.bridge.write_f32_interleaved(samples);
        self.produced_frames = self.produced_frames.wrapping_add(requested_frames);
        self.written_frames = self.written_frames.wrapping_add(written);
        self.dropped_frames = self
            .dropped_frames
            .wrapping_add(requested_frames.saturating_sub(written));
    }
}

#[wasm_bindgen]
pub struct AeroApi {
    version: String,
}

impl Default for AeroApi {
    fn default() -> Self {
        Self::new()
    }
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

// -------------------------------------------------------------------------------------------------
// Legacy demo VM (snapshotting) API
// -------------------------------------------------------------------------------------------------

// `#[deprecated]` is on the public `DemoVm` WASM export. `wasm-bindgen` expands that
// into helper items that reference the deprecated type, which triggers
// `deprecated` warnings when compiling this crate. Keep the deprecation for
// downstream users while silencing the internal macro expansion.
#[allow(deprecated)]
mod legacy_demo_vm {
    use wasm_bindgen::prelude::*;

    #[cfg(target_arch = "wasm32")]
    use super::OpfsSyncFile;
    /// Deterministic stub VM used by the snapshot demo panels.
    ///
    /// Deprecated in favor of the canonical full-system VM (`Machine`).
    #[wasm_bindgen]
    #[deprecated(
        note = "DemoVm is a legacy wrapper kept for snapshot demos; use `Machine` (aero_machine::Machine) instead"
    )]
    pub struct DemoVm {
        inner: aero_machine::Machine,
    }

    #[wasm_bindgen]
    impl DemoVm {
        fn demo_boot_sector() -> [u8; 512] {
            let mut sector = [0u8; 512];
            let mut i = 0usize;

            // Real-mode loop that continuously writes bytes to COM1 and stores the same bytes into RAM.
            //
            // This is intentionally deterministic and self-contained: it does not rely on BIOS
            // interrupts, timers, or external input.

            // mov dx, 0x3f8
            sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
            i += 3;
            // xor ax, ax
            sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
            i += 2;
            // mov di, 0x0500
            sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
            i += 3;
            // cld (ensure stosb increments DI)
            sector[i] = 0xFC;
            i += 1;

            let loop_off = i;

            // out dx, al
            sector[i] = 0xEE;
            i += 1;
            // stosb
            sector[i] = 0xAA;
            i += 1;
            // inc al
            sector[i..i + 2].copy_from_slice(&[0xFE, 0xC0]);
            i += 2;
            // cmp di, 0x7000
            sector[i..i + 4].copy_from_slice(&[0x81, 0xFF, 0x00, 0x70]);
            i += 4;
            // jne loop
            let jne_ip = i + 2;
            let rel = loop_off as i32 - jne_ip as i32;
            sector[i..i + 2].copy_from_slice(&[0x75, rel as i8 as u8]);
            i += 2;
            // mov di, 0x0500
            sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
            i += 3;
            // jmp loop
            let jmp_ip = i + 2;
            let rel = loop_off as i32 - jmp_ip as i32;
            sector[i..i + 2].copy_from_slice(&[0xEB, rel as i8 as u8]);

            sector[510] = 0x55;
            sector[511] = 0xAA;
            sector
        }

        #[wasm_bindgen(constructor)]
        pub fn new(ram_size_bytes: u32) -> Self {
            // The BIOS expects to use the EBDA at 0x9F000, so enforce a minimum RAM size.
            let ram_size_bytes = (ram_size_bytes as u64).clamp(2 * 1024 * 1024, 64 * 1024 * 1024);

            let mut inner = aero_machine::Machine::new(aero_machine::MachineConfig {
                ram_size_bytes,
                // Keep the demo VM minimal and deterministic: only serial is required.
                enable_i8042: false,
                enable_a20_gate: false,
                enable_reset_ctrl: false,
                ..Default::default()
            })
            .expect("DemoVm machine init should succeed");

            inner
                .set_disk_image(Self::demo_boot_sector().to_vec())
                .expect("DemoVm boot sector is always valid");
            inner.reset();

            Self { inner }
        }

        pub fn run_steps(&mut self, steps: u32) {
            let _ = self.inner.run_slice(steps as u64);
        }

        pub fn serial_output(&mut self) -> Vec<u8> {
            self.inner.serial_output_bytes()
        }

        /// Return the current serial output length without copying the bytes into JS.
        ///
        /// The demo VM accumulates serial output as it runs; callers that only need
        /// a byte count should prefer this over `serial_output()` to avoid large
        /// allocations in JS.
        pub fn serial_output_len(&mut self) -> u32 {
            self.inner.serial_output_len().min(u64::from(u32::MAX)) as u32
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

            file.close()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
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

            file.close()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            Ok(())
        }

        #[cfg(target_arch = "wasm32")]
        pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
            let mut file = OpfsSyncFile::open(&path, false)
                .await
                .map_err(|e| JsValue::from_str(&e.to_string()))?;

            self.inner
                .restore_snapshot_from_checked(&mut file)
                .map_err(|e| JsValue::from_str(&e.to_string()))?;

            file.close()
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            Ok(())
        }
    }
}

#[allow(deprecated)]
pub use legacy_demo_vm::DemoVm;

// -----------------------------------------------------------------------------
// CPU worker demo harness (WASM-side render + counters)
// -----------------------------------------------------------------------------

/// A tiny WASM-side harness intended to be driven by `web/src/workers/cpu.worker.ts`.
///
/// This is a stepping stone towards moving the full emulator stepping loop into
/// WASM: it exercises shared imported `WebAssembly.Memory`, the shared
/// framebuffer publish protocol, and an Atomics-visible counter in guest memory.
#[wasm_bindgen]
pub struct CpuWorkerDemo {
    guest_counter_offset_bytes: u32,

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    framebuffer: SharedFramebufferWriter,
}

#[wasm_bindgen]
impl CpuWorkerDemo {
    /// Create a new CPU worker demo harness.
    ///
    /// The framebuffer region must live inside the module's linear memory
    /// (imported `WebAssembly.Memory`), starting at `framebuffer_offset_bytes`.
    ///
    /// `ram_size_bytes` is used for a bounds sanity check. Note that wasm32 can
    /// have up to 4GiB of memory; `4GiB` cannot be represented in a `u32`, so a
    /// value of `0` is treated as `4GiB`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        ram_size_bytes: u32,
        framebuffer_offset_bytes: u32,
        width: u32,
        height: u32,
        tile_size: u32,
        guest_counter_offset_bytes: u32,
    ) -> Result<Self, JsValue> {
        let _ = (
            framebuffer_offset_bytes,
            width,
            height,
            tile_size,
            guest_counter_offset_bytes,
        );

        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            if guest_counter_offset_bytes == 0 || guest_counter_offset_bytes % 4 != 0 {
                return Err(JsValue::from_str(
                    "guest_counter_offset_bytes must be a non-zero multiple of 4",
                ));
            }

            // wasm32 can address at most 4GiB; represent that as 0 in the u32 ABI.
            let ram_size_bytes_u64 = if ram_size_bytes == 0 {
                4u64 * 1024 * 1024 * 1024
            } else {
                ram_size_bytes as u64
            };

            if framebuffer_offset_bytes == 0 || framebuffer_offset_bytes % 4 != 0 {
                return Err(JsValue::from_str(
                    "framebuffer_offset_bytes must be a non-zero multiple of 4",
                ));
            }

            let counter_end = guest_counter_offset_bytes as u64 + 4;
            if counter_end > ram_size_bytes_u64 {
                return Err(JsValue::from_str(&format!(
                    "guest_counter_offset_bytes out of bounds: offset=0x{guest_counter_offset_bytes:x} ram_size_bytes=0x{ram_size_bytes_u64:x}"
                )));
            }

            let layout =
                SharedFramebufferLayout::new_rgba8(width, height, tile_size).map_err(|e| {
                    JsValue::from_str(&format!("Invalid shared framebuffer layout: {e}"))
                })?;
            let total_bytes = layout.total_byte_len() as u64;
            let end = framebuffer_offset_bytes as u64 + total_bytes;
            if end > ram_size_bytes_u64 {
                return Err(JsValue::from_str(&format!(
                    "Shared framebuffer region out of bounds: offset=0x{framebuffer_offset_bytes:x} size=0x{total_bytes:x} end=0x{end:x} ram_size_bytes=0x{ram_size_bytes_u64:x}"
                )));
            }

            // Safety: the caller provides an in-bounds region in linear memory.
            let shared = unsafe {
                SharedFramebuffer::from_raw_parts(framebuffer_offset_bytes as *mut u8, layout)
            }
            .map_err(|e| JsValue::from_str(&format!("Invalid shared framebuffer base: {e}")))?;
            // The JS runtime may have already initialized the header (and in the
            // CPU worker we may start publishing frames via a JS fallback while
            // the threaded WASM module initializes asynchronously). Only
            // overwrite the header when it is uninitialized or incompatible with
            // the requested layout.
            let header = shared.header();
            let needs_init = {
                let snap = header.snapshot();
                snap.magic != aero_shared::shared_framebuffer::SHARED_FRAMEBUFFER_MAGIC
                    || snap.version != aero_shared::shared_framebuffer::SHARED_FRAMEBUFFER_VERSION
                    || snap.width != layout.width
                    || snap.height != layout.height
                    || snap.stride_bytes != layout.stride_bytes
                    || snap.format != layout.format as u32
                    || snap.tile_size != layout.tile_size
                    || snap.tiles_x != layout.tiles_x
                    || snap.tiles_y != layout.tiles_y
                    || snap.dirty_words_per_buffer != layout.dirty_words_per_buffer
            };
            if needs_init {
                header.init(layout);
            }

            // Reset the demo guest counter so tests can make deterministic assertions.
            unsafe {
                let counter_ptr = guest_counter_offset_bytes as *mut AtomicU32;
                (*counter_ptr).store(0, Ordering::SeqCst);
            }

            Ok(Self {
                guest_counter_offset_bytes,
                framebuffer: SharedFramebufferWriter::new(shared),
            })
        }

        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            let _ = ram_size_bytes;
            Err(JsValue::from_str(
                "CpuWorkerDemo requires the threaded WASM build (+atomics + shared memory).",
            ))
        }
    }

    /// Increment a shared guest-memory counter (Atomics-visible from JS).
    ///
    /// Returns the incremented value.
    pub fn tick(&self, _now_ms: f64) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        unsafe {
            let counter_ptr = self.guest_counter_offset_bytes as *const AtomicU32;
            (*counter_ptr)
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1)
        }

        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            let _ = self.guest_counter_offset_bytes;
            0
        }
    }

    /// Render a moving RGB test pattern into the shared framebuffer and publish it.
    ///
    /// Returns the published `frame_seq`.
    pub fn render_frame(&self, _frame_seq: u32, now_ms: f64) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            self.framebuffer.write_frame(|buf, dirty, layout| {
                let width = layout.width as usize;
                let height = layout.height as usize;
                let stride = layout.stride_bytes as usize;

                let dx = (now_ms * 0.06) as u32;
                let dy = (now_ms * 0.035) as u32;
                let dz = (now_ms * 0.02) as u32;

                for y in 0..height {
                    let base = y * stride;
                    let y_u32 = y as u32;
                    for x in 0..width {
                        let i = base + x * 4;
                        let x_u32 = x as u32;
                        buf[i] = x_u32.wrapping_add(dx) as u8;
                        buf[i + 1] = y_u32.wrapping_add(dy) as u8;
                        buf[i + 2] = (x_u32 ^ y_u32).wrapping_add(dz) as u8;
                        buf[i + 3] = 0xFF;
                    }
                }

                if let Some(words) = dirty {
                    // Demo uses full-frame dirty tracking.
                    words.fill(u32::MAX);
                }
            })
        }

        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            let _ = now_ms;
            0
        }
    }
}

#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunExitKind {
    Completed,
    Halted,
    ResetRequested,
    Assist,
    Exception,
    CpuExit,
}

#[wasm_bindgen]
pub struct RunExit {
    kind: RunExitKind,
    executed: u32,
    detail: String,
}

#[wasm_bindgen]
impl RunExit {
    #[wasm_bindgen(getter)]
    pub fn kind(&self) -> RunExitKind {
        self.kind
    }

    #[wasm_bindgen(getter)]
    pub fn executed(&self) -> u32 {
        self.executed
    }

    #[wasm_bindgen(getter)]
    pub fn detail(&self) -> String {
        self.detail.clone()
    }
}

impl RunExit {
    fn from_native(exit: aero_machine::RunExit) -> Self {
        let executed = exit.executed().min(u64::from(u32::MAX)) as u32;
        match exit {
            aero_machine::RunExit::Completed { .. } => Self {
                kind: RunExitKind::Completed,
                executed,
                detail: String::new(),
            },
            aero_machine::RunExit::Halted { .. } => Self {
                kind: RunExitKind::Halted,
                executed,
                detail: String::new(),
            },
            aero_machine::RunExit::ResetRequested { kind, .. } => Self {
                kind: RunExitKind::ResetRequested,
                executed,
                detail: format!("{kind:?}"),
            },
            aero_machine::RunExit::Assist { reason, .. } => Self {
                kind: RunExitKind::Assist,
                executed,
                detail: format!("{reason:?}"),
            },
            aero_machine::RunExit::Exception { exception, .. } => Self {
                kind: RunExitKind::Exception,
                executed,
                detail: exception.to_string(),
            },
            aero_machine::RunExit::CpuExit { exit, .. } => Self {
                kind: RunExitKind::CpuExit,
                executed,
                detail: format!("{exit:?}"),
            },
        }
    }
}

#[wasm_bindgen]
pub struct Machine {
    inner: aero_machine::Machine,
    // Tracks the last injected mouse button state (bit0=left, bit1=right, bit2=middle).
    //
    // This exists solely to support the ergonomic JS-side `inject_mouse_buttons_mask` API without
    // emitting redundant button transition packets.
    mouse_buttons: u8,
    mouse_buttons_known: bool,
}

#[wasm_bindgen]
impl Machine {
    #[wasm_bindgen(constructor)]
    pub fn new(ram_size_bytes: u32) -> Result<Self, JsValue> {
        let cfg = aero_machine::MachineConfig {
            ram_size_bytes: ram_size_bytes as u64,
            ..Default::default()
        };
        let inner =
            aero_machine::Machine::new(cfg).map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(Self {
            inner,
            mouse_buttons: 0,
            mouse_buttons_known: true,
        })
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.mouse_buttons = 0;
        self.mouse_buttons_known = true;
    }

    pub fn set_disk_image(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.inner
            .set_disk_image(bytes.to_vec())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    pub fn run_slice(&mut self, max_insts: u32) -> RunExit {
        RunExit::from_native(self.inner.run_slice(max_insts as u64))
    }

    /// Returns and clears any accumulated serial output.
    pub fn serial_output(&mut self) -> Vec<u8> {
        self.inner.take_serial_output()
    }

    /// Return the current serial output length without copying the bytes into JS.
    pub fn serial_output_len(&mut self) -> u32 {
        self.inner.serial_output_len().min(u64::from(u32::MAX)) as u32
    }

    /// Inject a browser-style keyboard event into the guest PS/2 i8042 controller.
    ///
    /// `code` must be a DOM `KeyboardEvent.code` string (e.g. `"KeyA"`, `"Enter"`, `"ArrowUp"`).
    /// Unknown codes are ignored.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        self.inner.inject_browser_key(code, pressed);
    }

    /// Inject relative mouse movement into the guest PS/2 i8042 controller.
    ///
    /// - `dx`/`dy` use browser-style coordinates: +X is right, +Y is down.
    /// - `wheel` uses PS/2 convention: positive is wheel up.
    ///
    /// Note: the PS/2 mouse model only emits movement packets when reporting is enabled by the
    /// guest (typically via `0xF4` after i8042 command `0xD4`).
    pub fn inject_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        self.inner.inject_mouse_motion(dx, dy, wheel);
    }

    /// Inject a mouse button transition, using DOM `MouseEvent.button` mapping:
    /// - `0`: left
    /// - `1`: middle
    /// - `2`: right
    ///
    /// Other values are ignored.
    pub fn inject_mouse_button(&mut self, button: u8, pressed: bool) {
        match button {
            0 => self.inject_mouse_left(pressed),
            1 => self.inject_mouse_middle(pressed),
            2 => self.inject_mouse_right(pressed),
            _ => {}
        }
    }

    /// Set all mouse buttons at once using a bitmask matching DOM `MouseEvent.buttons`:
    /// - bit0 (`0x01`): left
    /// - bit1 (`0x02`): right
    /// - bit2 (`0x04`): middle
    ///
    /// Bits above `0x07` are ignored.
    pub fn inject_mouse_buttons_mask(&mut self, mask: u8) {
        let next = mask & 0x07;

        if self.mouse_buttons_known {
            let prev = self.mouse_buttons;
            let delta = prev ^ next;
            if (delta & 0x01) != 0 {
                self.inject_mouse_left((next & 0x01) != 0);
            }
            if (delta & 0x02) != 0 {
                self.inject_mouse_right((next & 0x02) != 0);
            }
            if (delta & 0x04) != 0 {
                self.inject_mouse_middle((next & 0x04) != 0);
            }
        } else {
            // We don't know what state the underlying machine restored (e.g. after snapshot
            // restore), so set every button explicitly.
            self.inject_mouse_left((next & 0x01) != 0);
            self.inject_mouse_right((next & 0x02) != 0);
            self.inject_mouse_middle((next & 0x04) != 0);
        }

        self.mouse_buttons = next;
        self.mouse_buttons_known = true;
    }

    /// Convenience wrapper: set the left mouse button state.
    pub fn inject_mouse_left(&mut self, pressed: bool) {
        self.inner.inject_mouse_left(pressed);
        if pressed {
            self.mouse_buttons |= 0x01;
        } else {
            self.mouse_buttons &= !0x01;
        }
        self.mouse_buttons_known = true;
    }

    /// Convenience wrapper: set the right mouse button state.
    pub fn inject_mouse_right(&mut self, pressed: bool) {
        self.inner.inject_mouse_right(pressed);
        if pressed {
            self.mouse_buttons |= 0x02;
        } else {
            self.mouse_buttons &= !0x02;
        }
        self.mouse_buttons_known = true;
    }

    /// Convenience wrapper: set the middle mouse button state.
    pub fn inject_mouse_middle(&mut self, pressed: bool) {
        self.inner.inject_mouse_middle(pressed);
        if pressed {
            self.mouse_buttons |= 0x04;
        } else {
            self.mouse_buttons &= !0x04;
        }
        self.mouse_buttons_known = true;
    }

    // -------------------------------------------------------------------------
    // Network (Option C L2 tunnel via NET_TX / NET_RX AIPC rings)
    // -------------------------------------------------------------------------

    /// Attach pre-opened NET_TX / NET_RX rings as an L2 tunnel backend for the canonical machine.
    ///
    /// The browser runtime is expected to open these rings from its `ioIpcSab` via
    /// [`open_ring_by_kind`] and pass them in.
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_rings(
        &mut self,
        tx: SharedRingBuffer,
        rx: SharedRingBuffer,
    ) -> Result<(), JsValue> {
        let backend = aero_net_backend::L2TunnelRingBackend::new(tx, rx);
        self.inner.set_network_backend(Box::new(backend));
        Ok(())
    }

    /// Convenience: open `NET_TX`/`NET_RX` rings from an `ioIpcSab` and attach them as an L2 tunnel.
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_from_io_ipc_sab(
        &mut self,
        io_ipc: SharedArrayBuffer,
    ) -> Result<(), JsValue> {
        let tx = open_ring_by_kind(io_ipc.clone(), aero_ipc::layout::io_ipc_queue_kind::NET_TX, 0)?;
        let rx = open_ring_by_kind(io_ipc, aero_ipc::layout::io_ipc_queue_kind::NET_RX, 0)?;
        self.attach_l2_tunnel_rings(tx, rx)
    }

    /// Detach (drop) any installed network backend.
    ///
    /// This is useful around snapshot/restore boundaries: network backends are external state and
    /// are intentionally not captured in snapshots.
    pub fn detach_network(&mut self) {
        self.inner.detach_network();
    }

    // -------------------------------------------------------------------------
    // Snapshots (canonical machine)
    // -------------------------------------------------------------------------

    /// Take a full snapshot of the canonical machine.
    ///
    /// The returned bytes can be persisted by the web runtime and later restored
    /// via [`Machine::restore_snapshot`]. (See also the incremental dirty-page
    /// snapshot APIs.)
    pub fn snapshot_full(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .take_snapshot_full()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Take an incremental dirty-page snapshot.
    ///
    /// This is only valid if restored onto a machine that has already applied
    /// the parent snapshot chain (the snapshot format enforces parent IDs).
    pub fn snapshot_dirty(&mut self) -> Result<Vec<u8>, JsValue> {
        self.inner
            .take_snapshot_dirty()
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Restore a snapshot previously produced by [`Machine::snapshot_full`] or
    /// [`Machine::snapshot_dirty`].
    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.inner
            .restore_snapshot_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // Restoring rewinds machine device state; we no longer know the current mouse buttons.
        self.mouse_buttons_known = false;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn snapshot_full_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.inner
            .save_snapshot_full_to(&mut file)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
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

        file.close()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::open(&path, false)
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        self.inner
            .restore_snapshot_from_checked(&mut file)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        file.close()
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        // Restoring rewinds machine device state; we no longer know the current mouse buttons.
        self.mouse_buttons_known = false;
        Ok(())
    }
}
