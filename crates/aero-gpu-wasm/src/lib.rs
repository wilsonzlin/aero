#![forbid(unsafe_code)]
// Note: This crate is built for both the single-threaded WASM variant (pinned stable toolchain)
// and the threaded/shared-memory variant (pinned nightly toolchain for `-Z build-std`). Keep the
// Rust code free of unstable language features so both builds remain viable.

#[allow(dead_code)]
mod drain_queue;

#[cfg(target_arch = "wasm32")]
fn install_panic_hook_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            // Panics in wasm32 builds currently abort (trap with `unreachable`). Without a hook,
            // these show up as opaque `RuntimeError: unreachable` errors in browser/Playwright.
            //
            // Emit the panic message to the JS console so E2E failures can be diagnosed.
            let msg = info.to_string();
            web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&msg));
        }));
    });
}

// -----------------------------------------------------------------------------
// Guest physical address translation (PC/Q35 E820)
// -----------------------------------------------------------------------------
//
// The web runtime stores guest RAM as a flat `[0..guest_size)` byte array (backed by a shared
// `WebAssembly.Memory`). On the PC/Q35 platform, guest *physical* RAM is non-contiguous once the
// configured guest RAM exceeds the PCIe ECAM base (`aero_guest_phys::PCIE_ECAM_BASE`): the "high"
// portion is remapped above 4GiB, leaving an ECAM+PCI/MMIO hole below 4GiB.
//
// AeroGPU allocation tables (`alloc.gpa`) use guest physical addresses, so the browser GPU worker
// must translate GPAs back into this backing-store offset space before indexing `Uint8Array`.
//
// Keep this in sync with:
// - `crates/aero-guest-phys/src/lib.rs`
// - `web/src/arch/guest_ram_translate.ts` (and its wrapper `web/src/runtime/shared_layout.ts`)
#[cfg(any(test, target_arch = "wasm32"))]
mod guest_phys {
    pub use aero_guest_phys::translate_guest_paddr_range_to_offset as translate_guest_paddr_range;
    #[cfg(test)]
    pub use aero_guest_phys::{HIGH_RAM_START, LOW_RAM_END};

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn translate_guest_paddr_range_identity_for_small_ram() {
            let ram = 0x2000;
            assert_eq!(translate_guest_paddr_range(ram, 0, 0), Some(0));
            assert_eq!(translate_guest_paddr_range(ram, 0, 1), Some(0));
            assert_eq!(translate_guest_paddr_range(ram, 0x1234, 4), Some(0x1234));
            assert_eq!(translate_guest_paddr_range(ram, ram - 1, 1), Some(ram - 1));
            assert_eq!(translate_guest_paddr_range(ram, ram, 1), None);
            assert_eq!(translate_guest_paddr_range(ram, ram - 1, 2), None);
            // Empty slice at end is OK.
            assert_eq!(translate_guest_paddr_range(ram, ram, 0), Some(ram));
            assert_eq!(translate_guest_paddr_range(ram, ram + 1, 0), None);
        }

        #[test]
        fn translate_guest_paddr_range_rejects_hole_and_maps_high_ram() {
            let ram = LOW_RAM_END + 0x2000;

            // Low RAM is identity-mapped.
            assert_eq!(
                translate_guest_paddr_range(ram, LOW_RAM_END - 4, 4),
                Some(LOW_RAM_END - 4)
            );
            // Hole rejected.
            assert_eq!(translate_guest_paddr_range(ram, LOW_RAM_END, 4), None);
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START - 4, 4),
                None
            );
            // Empty slice at the low/high boundary is OK.
            assert_eq!(
                translate_guest_paddr_range(ram, LOW_RAM_END, 0),
                Some(LOW_RAM_END)
            );
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START, 0),
                Some(LOW_RAM_END)
            );
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START - 1, 0),
                None
            );
            // High RAM remaps above 4GiB.
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START, 4),
                Some(LOW_RAM_END)
            );
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START + 0x1FFC, 4),
                Some(LOW_RAM_END + 0x1FFC)
            );
            // Empty slice at the end of high RAM is OK.
            assert_eq!(
                translate_guest_paddr_range(ram, HIGH_RAM_START + 0x2000, 0),
                Some(ram)
            );
            // Cross-region rejected (low -> hole).
            assert_eq!(translate_guest_paddr_range(ram, LOW_RAM_END - 2, 4), None);
        }
    }
}

// Import the canonical wasm32 guest layout constants from `crates/aero-wasm`.
//
// We `#[path]`-include the module instead of depending on the full `aero-wasm` crate to avoid
// pulling its wasm-bindgen exports (which would collide at link time when building this separate
// wasm-pack module).
#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../aero-wasm/src/guest_layout.rs"]
mod guest_layout;

// The full implementation is only meaningful on wasm32.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    use crate::drain_queue::DrainQueue;
    use crate::guest_layout::PCI_MMIO_BASE;
    use aero_gpu::GpuErrorEvent;
    use aero_gpu::aerogpu_executor::{AllocEntry, AllocTable};
    use aero_gpu::shader_lib::{BuiltinShader, wgsl as builtin_wgsl};
    use aero_gpu::stats::GpuStats;
    use aero_gpu::{
        AeroGpuCommandProcessor, AeroGpuEvent, AeroGpuSubmissionAllocation, AerogpuD3d9Executor,
        FrameTimingsReport, GpuBackendKind, GpuProfiler, GuestMemory, GuestMemoryError,
    };
    use aero_protocol::aerogpu::aerogpu_cmd as cmd;
    use aero_protocol::aerogpu::aerogpu_ring as ring;
    use futures_intrusive::channel::shared::oneshot_channel;
    use js_sys::{Array, BigInt, Object, Reflect, Uint8Array};
    use wasm_bindgen::prelude::*;
    use web_sys::OffscreenCanvas;

    // wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
    // `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
    // by the linker when there is at least one TLS variable. We keep a tiny TLS
    // slot behind a cargo feature enabled only for the threaded build.
    #[cfg(feature = "wasm-threaded")]
    thread_local! {
        // wasm-bindgen's "threads" transform expects TLS metadata symbols (e.g.
        // `__tls_size`) to exist in shared-memory builds. Those symbols are only emitted
        // by the linker when there is at least one TLS variable.
        //
        // We use `thread_local!` instead of the unstable `#[thread_local]` attribute so
        // the threaded WASM build can compile on stable Rust.
        static TLS_DUMMY: u8 = const { 0 };
    }

    #[wasm_bindgen(start)]
    pub fn wasm_start() {
        #[cfg(feature = "wasm-threaded")]
        {
            // Ensure the TLS dummy is not optimized away.
            TLS_DUMMY.with(|v| core::hint::black_box(*v));
        }
    }

    // -------------------------------------------------------------------------
    // Diagnostics exports (optional; polled by `web/src/workers/gpu-worker.ts`)
    // -------------------------------------------------------------------------

    /// Returns a structured-cloneable object containing the current GPU telemetry counters.
    ///
    /// This is intentionally non-panicking and returns a valid object even if the GPU has not
    /// been initialized.
    #[wasm_bindgen]
    pub fn get_gpu_stats() -> JsValue {
        let snapshot = gpu_stats().snapshot();
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("presents_attempted"),
            &JsValue::from_f64(snapshot.presents_attempted as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("presents_succeeded"),
            &JsValue::from_f64(snapshot.presents_succeeded as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("recoveries_attempted"),
            &JsValue::from_f64(snapshot.recoveries_attempted as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("recoveries_succeeded"),
            &JsValue::from_f64(snapshot.recoveries_succeeded as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("surface_reconfigures"),
            &JsValue::from_f64(snapshot.surface_reconfigures as f64),
        );

        // Best-effort presentation upload bandwidth counters (RGBA8 source framebuffer).
        let rgba8_upload_full_bytes = RGBA8_UPLOAD_BYTES_FULL.load(Ordering::Relaxed);
        let rgba8_upload_dirty_bytes = RGBA8_UPLOAD_BYTES_DIRTY.load(Ordering::Relaxed);
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_bytes_full"),
            &JsValue::from_f64(rgba8_upload_full_bytes as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_bytes_dirty_rects"),
            &JsValue::from_f64(rgba8_upload_dirty_bytes as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_bytes"),
            &JsValue::from_f64((rgba8_upload_full_bytes + rgba8_upload_dirty_bytes) as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_full_calls"),
            &JsValue::from_f64(RGBA8_UPLOAD_FULL_CALLS.load(Ordering::Relaxed) as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_dirty_calls"),
            &JsValue::from_f64(RGBA8_UPLOAD_DIRTY_CALLS.load(Ordering::Relaxed) as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rgba8_upload_dirty_rects"),
            &JsValue::from_f64(RGBA8_UPLOAD_DIRTY_RECTS.load(Ordering::Relaxed) as f64),
        );

        // ---------------------------------------------------------------------
        // D3D9 shader translation + cache stats (WG-010)
        // ---------------------------------------------------------------------
        //
        // Expose both:
        // - the fully-qualified `d3d9_shader_*` keys (mirrors `aero-gpu` stats naming)
        // - legacy/short `translate_calls` / `persistent_hits` / `persistent_misses` keys used by
        //   the D3D9 shader-cache harness page.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_translate_calls"),
            &JsValue::from_f64(snapshot.d3d9_shader_translate_calls as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_sm3_fallbacks"),
            &JsValue::from_f64(snapshot.d3d9_shader_sm3_fallbacks as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_cache_persistent_hits"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_persistent_hits as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_cache_persistent_misses"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_persistent_misses as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_cache_memory_hits"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_memory_hits as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_shader_cache_disabled"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_disabled as f64),
        );

        // Back-compat keys consumed by `web/gpu-worker-d3d9-shader-cache.ts`.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("translate_calls"),
            &JsValue::from_f64(snapshot.d3d9_shader_translate_calls as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("persistent_hits"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_persistent_hits as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("persistent_misses"),
            &JsValue::from_f64(snapshot.d3d9_shader_cache_persistent_misses as f64),
        );

        // D3D9 translator cache version participating in persistent shader cache keys.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9_translator_cache_version"),
            &JsValue::from_f64(aero_d3d9::runtime::D3D9_TRANSLATOR_CACHE_VERSION as f64),
        );
        // CamelCase alias for convenience in browser tooling.
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("d3d9TranslatorCacheVersion"),
            &JsValue::from_f64(aero_d3d9::runtime::D3D9_TRANSLATOR_CACHE_VERSION as f64),
        );

        // Stable backend/device fingerprint used to partition persistent shader cache keys.
        if let Some(caps_hash) = D3D9_STATE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|state| state.shader_cache_caps_hash.clone())
        }) {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("d3d9_shader_cache_caps_hash"),
                &JsValue::from_str(&caps_hash),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("d3d9ShaderCacheCapsHash"),
                &JsValue::from_str(&caps_hash),
            );
        }

        obj.into()
    }

    /// CamelCase alias for callers that probe `getGpuStats`.
    #[wasm_bindgen(js_name = getGpuStats)]
    pub fn get_gpu_stats_alias() -> JsValue {
        get_gpu_stats()
    }

    /// Drain-and-clear any queued GPU runtime error events.
    ///
    /// Returns an array of events compatible with
    /// `GpuRuntimeErrorEvent` normalization in the browser GPU worker.
    #[wasm_bindgen]
    pub fn drain_gpu_events() -> JsValue {
        fn backend_kind_as_str(kind: GpuBackendKind) -> &'static str {
            match kind {
                GpuBackendKind::WebGpu => "webgpu",
                GpuBackendKind::WebGl2 => "webgl2",
            }
        }

        let events = gpu_event_queue().drain();
        let out = Array::new();
        for ev in &events {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("time_ms"),
                &JsValue::from_f64(ev.time_ms as f64),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("backend_kind"),
                &JsValue::from_str(backend_kind_as_str(ev.backend_kind)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("severity"),
                &JsValue::from_str(ev.severity.as_str()),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("category"),
                &JsValue::from_str(ev.category.as_str()),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("message"),
                &JsValue::from_str(&ev.message),
            );
            if let Some(details) = &ev.details {
                let details_obj = Object::new();
                for (k, v) in details {
                    let _ =
                        Reflect::set(&details_obj, &JsValue::from_str(k), &JsValue::from_str(v));
                }
                let _ = Reflect::set(&obj, &JsValue::from_str("details"), &details_obj.into());
            }
            out.push(&obj);
        }
        out.into()
    }

    // Additional aliases probed by the worker.
    #[wasm_bindgen]
    pub fn drain_gpu_error_events() -> JsValue {
        drain_gpu_events()
    }

    #[wasm_bindgen]
    pub fn take_gpu_events() -> JsValue {
        drain_gpu_events()
    }

    #[wasm_bindgen]
    pub fn take_gpu_error_events() -> JsValue {
        drain_gpu_events()
    }

    #[wasm_bindgen(js_name = drainGpuEvents)]
    pub fn drain_gpu_events_alias() -> JsValue {
        drain_gpu_events()
    }

    thread_local! {
        static PROCESSOR: RefCell<AeroGpuCommandProcessor> =
            RefCell::new(AeroGpuCommandProcessor::new());
    }

    static GPU_STATS: OnceLock<Arc<GpuStats>> = OnceLock::new();
    static GPU_EVENT_QUEUE: OnceLock<DrainQueue<GpuErrorEvent>> = OnceLock::new();
    const MAX_GPU_EVENTS: usize = 1024;

    // -----------------------------------------------------------------------------
    // Presentation upload bandwidth counters (best-effort)
    // -----------------------------------------------------------------------------
    //
    // These counters measure how many *tight* RGBA8 bytes are uploaded into the internal
    // framebuffer texture. This approximates the bandwidth we push over the WASM<->GPU boundary
    // and lets us quantify savings from dirty-rect uploads.
    //
    // Note: this intentionally does not include bytes-per-row padding inserted to satisfy
    // `COPY_BYTES_PER_ROW_ALIGNMENT`; those padding bytes are an implementation detail that may
    // vary across backends.
    static RGBA8_UPLOAD_BYTES_FULL: AtomicU64 = AtomicU64::new(0);
    static RGBA8_UPLOAD_BYTES_DIRTY: AtomicU64 = AtomicU64::new(0);
    static RGBA8_UPLOAD_FULL_CALLS: AtomicU64 = AtomicU64::new(0);
    static RGBA8_UPLOAD_DIRTY_CALLS: AtomicU64 = AtomicU64::new(0);
    static RGBA8_UPLOAD_DIRTY_RECTS: AtomicU64 = AtomicU64::new(0);

    fn gpu_stats() -> &'static Arc<GpuStats> {
        GPU_STATS.get_or_init(|| Arc::new(GpuStats::new()))
    }

    fn gpu_event_queue() -> &'static DrainQueue<GpuErrorEvent> {
        GPU_EVENT_QUEUE.get_or_init(DrainQueue::new)
    }

    fn push_gpu_event(event: GpuErrorEvent) {
        gpu_event_queue().push_bounded(event, MAX_GPU_EVENTS);
    }

    // Note: events are forwarded as JS objects/arrays. The browser GPU worker accepts either
    // JSON strings or objects, but objects avoid an extra JSON parse and are structured-cloneable.

    #[derive(Clone)]
    struct JsGuestMemory {
        ram: Uint8Array,
        vram: Option<Uint8Array>,
    }

    impl JsGuestMemory {
        fn resolve_range(
            &self,
            gpa: u64,
            len: usize,
        ) -> Result<(&Uint8Array, u32, u32), GuestMemoryError> {
            let len_u64 = u64::try_from(len).map_err(|_| GuestMemoryError { gpa, len })?;

            // ---------------------------------------------------------------------
            // Guest RAM (PC/Q35 hole/high-RAM remap layout).
            // ---------------------------------------------------------------------
            //
            // `gpa` is a guest physical address. The backing `Uint8Array` is a flat RAM byte store
            // of length `guest_size`; translate through the PC/Q35 hole/high-RAM remap layout.
            let ram_bytes = self.ram.length() as u64;
            if let Some(ram_offset) =
                crate::guest_phys::translate_guest_paddr_range(ram_bytes, gpa, len_u64)
            {
                let end = ram_offset
                    .checked_add(len_u64)
                    .ok_or(GuestMemoryError { gpa, len })?;
                let start_u32 =
                    u32::try_from(ram_offset).map_err(|_| GuestMemoryError { gpa, len })?;
                let end_u32 = u32::try_from(end).map_err(|_| GuestMemoryError { gpa, len })?;
                return Ok((&self.ram, start_u32, end_u32));
            }

            // ---------------------------------------------------------------------
            // VRAM aperture (BAR1) in the PCI/MMIO hole below 4GiB.
            // ---------------------------------------------------------------------
            if let Some(vram) = self.vram.as_ref() {
                let vram_len = vram.length() as u64;
                if let Some(vram_off) = gpa.checked_sub(PCI_MMIO_BASE) {
                    let end = vram_off
                        .checked_add(len_u64)
                        .ok_or(GuestMemoryError { gpa, len })?;
                    if end <= vram_len {
                        let start_u32 =
                            u32::try_from(vram_off).map_err(|_| GuestMemoryError { gpa, len })?;
                        let end_u32 =
                            u32::try_from(end).map_err(|_| GuestMemoryError { gpa, len })?;
                        return Ok((vram, start_u32, end_u32));
                    }
                }
            }

            Err(GuestMemoryError { gpa, len })
        }
    }

    impl GuestMemory for JsGuestMemory {
        fn read(&mut self, gpa: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
            let len = dst.len();
            let (view, start_u32, end_u32) = self.resolve_range(gpa, len)?;
            view.subarray(start_u32, end_u32).copy_to(dst);
            Ok(())
        }

        fn write(&mut self, gpa: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
            let len = src.len();
            let (view, start_u32, end_u32) = self.resolve_range(gpa, len)?;
            view.subarray(start_u32, end_u32).copy_from(src);
            Ok(())
        }
    }

    thread_local! {
        static GUEST_MEMORY: RefCell<Option<JsGuestMemory>> = const { RefCell::new(None) };
        static VRAM_MEMORY: RefCell<Option<Uint8Array>> = const { RefCell::new(None) };
    }

    /// Register a view of guest RAM for AeroGPU submissions.
    ///
    /// Contract: this is a flat view of the guest RAM *backing store* (length = `guest_size`).
    ///
    /// On PC/Q35, guest physical RAM is non-contiguous once `guest_size > LOW_RAM_END` due to the
    /// ECAM/PCI/MMIO hole below 4 GiB. The GPU executor translates guest physical addresses (GPAs)
    /// back into this backing store using the same low-RAM + hole + high-RAM remap layout as the
    /// rest of the wasm runtime.
    #[wasm_bindgen]
    pub fn set_guest_memory(guest_u8: Uint8Array) {
        let vram = VRAM_MEMORY.with(|slot| slot.borrow().clone());
        GUEST_MEMORY.with(|slot| {
            *slot.borrow_mut() = Some(JsGuestMemory {
                ram: guest_u8,
                vram,
            });
        });
    }

    #[wasm_bindgen]
    pub fn clear_guest_memory() {
        GUEST_MEMORY.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }

    #[wasm_bindgen]
    pub fn has_guest_memory() -> bool {
        GUEST_MEMORY.with(|slot| slot.borrow().is_some())
    }

    /// Register a view of the AeroGPU VRAM aperture (BAR1) for guest memory resolution.
    ///
    /// When configured, GPAs in the PCI/MMIO window `[PCI_MMIO_BASE, PCI_MMIO_BASE + vram_len)` can
    /// be resolved by the GPU executor for allocation uploads/writebacks.
    #[wasm_bindgen]
    pub fn set_vram_memory(vram_u8: Uint8Array) {
        let vram_for_guest = vram_u8.clone();
        VRAM_MEMORY.with(|slot| {
            *slot.borrow_mut() = Some(vram_u8);
        });
        GUEST_MEMORY.with(|slot| {
            if let Some(mem) = slot.borrow_mut().as_mut() {
                mem.vram = Some(vram_for_guest);
            }
        });
    }

    #[wasm_bindgen]
    pub fn clear_vram_memory() {
        VRAM_MEMORY.with(|slot| {
            *slot.borrow_mut() = None;
        });
        GUEST_MEMORY.with(|slot| {
            if let Some(mem) = slot.borrow_mut().as_mut() {
                mem.vram = None;
            }
        });
    }

    #[wasm_bindgen]
    pub fn has_vram_memory() -> bool {
        VRAM_MEMORY.with(|slot| slot.borrow().is_some())
    }

    /// Debug helper: copy bytes out of the registered guest RAM view.
    ///
    /// Note: this necessarily copies into wasm memory; it is intended for debugging/tests.
    ///
    /// `gpa` is a guest physical address (subject to the same low-RAM + hole + high-RAM remap
    /// layout as allocations and submissions).
    #[wasm_bindgen]
    pub fn read_guest_memory(gpa: u64, len: u32) -> Result<Uint8Array, JsValue> {
        let len = len as usize;
        let mut out = vec![0u8; len];
        GUEST_MEMORY.with(|slot| {
            let mut slot = slot.borrow_mut();
            let mem = slot.as_mut().ok_or_else(|| {
                JsValue::from_str(
                    "guest memory is not configured; call set_guest_memory(Uint8Array)",
                )
            })?;
            mem.read(gpa, &mut out)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            Ok::<(), JsValue>(())
        })?;
        Ok(Uint8Array::from(out.as_slice()))
    }

    #[cfg(test)]
    mod vram_tests {
        use super::*;
        use wasm_bindgen_test::*;

        wasm_bindgen_test_configure!(run_in_browser);

        #[wasm_bindgen_test]
        fn guest_memory_vram_aperture_read_write() {
            // Guest RAM is present but intentionally small; VRAM mappings should still work for
            // GPAs inside the BAR1 window.
            let guest = Uint8Array::new_with_length(0x100);
            let vram = Uint8Array::new_with_length(0x20);

            let mut mem = JsGuestMemory {
                ram: guest,
                vram: Some(vram.clone()),
            };

            let gpa = PCI_MMIO_BASE + 4;
            mem.write(gpa, &[1, 2, 3]).expect("write VRAM");

            let mut out = [0u8; 3];
            mem.read(gpa, &mut out).expect("read VRAM");
            assert_eq!(out, [1, 2, 3]);

            let mut vram_bytes = [0u8; 3];
            vram.subarray(4, 7).copy_to(&mut vram_bytes);
            assert_eq!(vram_bytes, [1, 2, 3]);
        }
    }

    const MAX_ALLOC_TABLE_SIZE_BYTES: usize = 16 * 1024 * 1024;
    const MAX_CMD_STREAM_SIZE_BYTES: usize = 64 * 1024 * 1024;

    fn copy_cmd_stream_bytes(cmd_stream: &Uint8Array) -> Result<Vec<u8>, JsValue> {
        let buf_len = cmd_stream.length() as usize;
        if buf_len < cmd::AerogpuCmdStreamHeader::SIZE_BYTES {
            return Err(JsValue::from_str(&format!(
                "cmd stream too small (got {buf_len} bytes, need {})",
                cmd::AerogpuCmdStreamHeader::SIZE_BYTES
            )));
        }

        let mut header_bytes = [0u8; cmd::AerogpuCmdStreamHeader::SIZE_BYTES];
        cmd_stream
            .subarray(0, cmd::AerogpuCmdStreamHeader::SIZE_BYTES as u32)
            .copy_to(&mut header_bytes);
        let header = cmd::decode_cmd_stream_header_le(&header_bytes).map_err(|err| {
            JsValue::from_str(&format!("failed to decode cmd stream header: {err:?}"))
        })?;

        let declared_size_bytes = header.size_bytes;
        let size_bytes = declared_size_bytes as usize;
        if size_bytes < cmd::AerogpuCmdStreamHeader::SIZE_BYTES || size_bytes > buf_len {
            return Err(JsValue::from_str(&format!(
                "invalid cmd stream header size_bytes={} (buffer_len={})",
                declared_size_bytes, buf_len
            )));
        }
        if size_bytes > MAX_CMD_STREAM_SIZE_BYTES {
            return Err(JsValue::from_str(&format!(
                "cmd stream header size_bytes too large (got {size_bytes}, max {MAX_CMD_STREAM_SIZE_BYTES})"
            )));
        }

        // Forward-compat: command stream buffers may include trailing bytes beyond the header's
        // `size_bytes` (capacity / page rounding). Only copy the declared prefix.
        let mut bytes = vec![0u8; size_bytes];
        cmd_stream
            .subarray(0, declared_size_bytes)
            .copy_to(&mut bytes);
        Ok(bytes)
    }

    fn decode_alloc_table_bytes_used_prefix(
        buf: &Uint8Array,
    ) -> Result<(ring::AerogpuAllocTableHeader, Vec<u8>), JsValue> {
        let buf_len = buf.length() as usize;
        if buf_len < ring::AerogpuAllocTableHeader::SIZE_BYTES {
            return Err(JsValue::from_str(&format!(
                "alloc table too small (got {buf_len} bytes, need {})",
                ring::AerogpuAllocTableHeader::SIZE_BYTES
            )));
        }

        let mut header_bytes = [0u8; ring::AerogpuAllocTableHeader::SIZE_BYTES];
        buf.subarray(0, ring::AerogpuAllocTableHeader::SIZE_BYTES as u32)
            .copy_to(&mut header_bytes);
        let header =
            ring::AerogpuAllocTableHeader::decode_from_le_bytes(&header_bytes).map_err(|err| {
                JsValue::from_str(&format!("failed to decode alloc table header: {err:?}"))
            })?;
        header
            .validate_prefix()
            .map_err(|err| JsValue::from_str(&format!("invalid alloc table header: {err:?}")))?;

        let size_bytes = usize::try_from(header.size_bytes).map_err(|_| {
            JsValue::from_str("alloc table header size_bytes does not fit in usize")
        })?;
        if size_bytes < ring::AerogpuAllocTableHeader::SIZE_BYTES || size_bytes > buf_len {
            return Err(JsValue::from_str(&format!(
                "invalid alloc table header size_bytes={} (buffer_len={})",
                header.size_bytes, buf_len
            )));
        }
        if size_bytes > MAX_ALLOC_TABLE_SIZE_BYTES {
            return Err(JsValue::from_str(&format!(
                "alloc table header size_bytes too large (got {size_bytes}, max {MAX_ALLOC_TABLE_SIZE_BYTES})"
            )));
        }

        // Forward-compat: alloc tables may include trailing bytes beyond the header's `size_bytes`.
        // Only copy the declared prefix.
        let mut bytes = vec![0u8; size_bytes];
        buf.subarray(0, header.size_bytes).copy_to(&mut bytes);
        Ok((header, bytes))
    }

    fn decode_submission_allocations(
        buf: &Uint8Array,
    ) -> Result<Vec<AeroGpuSubmissionAllocation>, JsValue> {
        let (header, bytes) = decode_alloc_table_bytes_used_prefix(buf)?;
        let size_bytes = bytes.len();

        let entry_count = usize::try_from(header.entry_count).map_err(|_| {
            JsValue::from_str("alloc table header entry_count does not fit in usize")
        })?;
        let entry_stride_bytes = usize::try_from(header.entry_stride_bytes).map_err(|_| {
            JsValue::from_str("alloc table header entry_stride_bytes does not fit in usize")
        })?;

        let required_bytes =
            ring::AerogpuAllocTableHeader::SIZE_BYTES
                .checked_add(entry_count.checked_mul(entry_stride_bytes).ok_or_else(|| {
                    JsValue::from_str("alloc table entry_count * stride overflows")
                })?)
                .ok_or_else(|| JsValue::from_str("alloc table size computation overflows"))?;
        if required_bytes > size_bytes {
            return Err(JsValue::from_str(&format!(
                "alloc table size_bytes too small for layout (size_bytes={} < required_bytes={required_bytes})",
                header.size_bytes,
            )));
        }

        let mut seen = HashSet::with_capacity(entry_count);
        let mut out = Vec::with_capacity(entry_count);

        for i in 0..entry_count {
            let base = ring::AerogpuAllocTableHeader::SIZE_BYTES
                .checked_add(
                    i.checked_mul(entry_stride_bytes)
                        .ok_or_else(|| JsValue::from_str("alloc table entry offset overflows"))?,
                )
                .ok_or_else(|| JsValue::from_str("alloc table entry offset overflows"))?;
            let end = base
                .checked_add(ring::AerogpuAllocEntry::SIZE_BYTES)
                .ok_or_else(|| JsValue::from_str("alloc table entry range overflows"))?;
            if end > size_bytes {
                return Err(JsValue::from_str(&format!(
                    "alloc table entry {i} is out of bounds (end={end}, size_bytes={size_bytes})"
                )));
            }

            let entry = ring::AerogpuAllocEntry::decode_from_le_bytes(&bytes[base..end]).map_err(
                |err| {
                    JsValue::from_str(&format!("failed to decode alloc table entry {i}: {err:?}"))
                },
            )?;

            let alloc_id = entry.alloc_id;
            if alloc_id == 0 {
                return Err(JsValue::from_str(&format!(
                    "alloc table entry {i} has alloc_id=0"
                )));
            }
            if !seen.insert(alloc_id) {
                return Err(JsValue::from_str(&format!(
                    "alloc table contains duplicate alloc_id={alloc_id}"
                )));
            }

            out.push(AeroGpuSubmissionAllocation {
                alloc_id,
                gpa: entry.gpa,
                size_bytes: entry.size_bytes,
            });
        }

        Ok(out)
    }

    fn decode_alloc_table_bytes(
        buf: &Uint8Array,
    ) -> Result<(AllocTable, Vec<AeroGpuSubmissionAllocation>), JsValue> {
        let (header, bytes) = decode_alloc_table_bytes_used_prefix(buf)?;

        // Match the native emulator decoder: `entry_stride_bytes` must be large enough to hold an
        // `aerogpu_alloc_entry`, but may be larger for forward-compatible extensions.
        if header.entry_stride_bytes < ring::AerogpuAllocEntry::SIZE_BYTES as u32 {
            return Err(JsValue::from_str(&format!(
                "alloc table entry_stride_bytes={} too small (min {})",
                header.entry_stride_bytes,
                ring::AerogpuAllocEntry::SIZE_BYTES
            )));
        }

        let size_bytes = bytes.len();

        let entry_count = usize::try_from(header.entry_count).map_err(|_| {
            JsValue::from_str("alloc table header entry_count does not fit in usize")
        })?;
        let entry_stride_bytes = usize::try_from(header.entry_stride_bytes).map_err(|_| {
            JsValue::from_str("alloc table header entry_stride_bytes does not fit in usize")
        })?;

        let required_bytes =
            ring::AerogpuAllocTableHeader::SIZE_BYTES
                .checked_add(entry_count.checked_mul(entry_stride_bytes).ok_or_else(|| {
                    JsValue::from_str("alloc table entry_count * stride overflows")
                })?)
                .ok_or_else(|| JsValue::from_str("alloc table size computation overflows"))?;
        if required_bytes > size_bytes {
            return Err(JsValue::from_str(&format!(
                "alloc table size_bytes too small for layout (size_bytes={} < required_bytes={required_bytes})",
                header.size_bytes,
            )));
        }

        let mut seen = HashSet::with_capacity(entry_count);
        let mut entries = Vec::with_capacity(entry_count);
        let mut allocations = Vec::with_capacity(entry_count);

        for i in 0..entry_count {
            let base = ring::AerogpuAllocTableHeader::SIZE_BYTES
                .checked_add(
                    i.checked_mul(entry_stride_bytes)
                        .ok_or_else(|| JsValue::from_str("alloc table entry offset overflows"))?,
                )
                .ok_or_else(|| JsValue::from_str("alloc table entry offset overflows"))?;
            let end = base
                .checked_add(ring::AerogpuAllocEntry::SIZE_BYTES)
                .ok_or_else(|| JsValue::from_str("alloc table entry range overflows"))?;
            if end > size_bytes {
                return Err(JsValue::from_str(&format!(
                    "alloc table entry {i} is out of bounds (end={end}, size_bytes={size_bytes})"
                )));
            }

            let entry = ring::AerogpuAllocEntry::decode_from_le_bytes(&bytes[base..end]).map_err(
                |err| {
                    JsValue::from_str(&format!("failed to decode alloc table entry {i}: {err:?}"))
                },
            )?;

            let alloc_id = entry.alloc_id;
            if alloc_id == 0 {
                return Err(JsValue::from_str(&format!(
                    "alloc table entry {i} has alloc_id=0"
                )));
            }
            if !seen.insert(alloc_id) {
                return Err(JsValue::from_str(&format!(
                    "alloc table contains duplicate alloc_id={alloc_id}"
                )));
            }

            entries.push((
                alloc_id,
                AllocEntry {
                    flags: entry.flags,
                    gpa: entry.gpa,
                    size_bytes: entry.size_bytes,
                },
            ));
            allocations.push(AeroGpuSubmissionAllocation {
                alloc_id,
                gpa: entry.gpa,
                size_bytes: entry.size_bytes,
            });
        }

        let table = AllocTable::new(entries).map_err(|err| JsValue::from_str(&err.to_string()))?;
        Ok((table, allocations))
    }

    #[wasm_bindgen]
    pub fn submit_aerogpu(
        cmd_stream: Uint8Array,
        signal_fence: u64,
        alloc_table: Option<Uint8Array>,
    ) -> Result<JsValue, JsValue> {
        let allocations = match alloc_table.as_ref() {
            Some(buf) => Some(decode_submission_allocations(buf)?),
            None => None,
        };
        let allocations = allocations.as_deref();

        let bytes = copy_cmd_stream_bytes(&cmd_stream)?;

        let present_count = PROCESSOR.with(|processor| {
            let mut processor = processor.borrow_mut();
            let events = processor
                .process_submission_with_allocations(&bytes, allocations, signal_fence)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;

            let mut had_present = false;
            for event in events {
                if matches!(event, AeroGpuEvent::PresentCompleted { .. }) {
                    had_present = true;
                }
            }

            Ok::<Option<u64>, JsValue>(had_present.then(|| processor.present_count()))
        })?;

        let out = Object::new();
        Reflect::set(
            &out,
            &JsValue::from_str("completedFence"),
            &BigInt::from(signal_fence).into(),
        )?;
        if let Some(present_count) = present_count {
            Reflect::set(
                &out,
                &JsValue::from_str("presentCount"),
                &BigInt::from(present_count).into(),
            )?;
        }

        Ok(out.into())
    }

    #[wasm_bindgen]
    pub async fn submit_aerogpu_d3d9(
        cmd_stream: Uint8Array,
        signal_fence: u64,
        context_id: u32,
        alloc_table: Option<Uint8Array>,
    ) -> Result<JsValue, JsValue> {
        let bytes = copy_cmd_stream_bytes(&cmd_stream)?;

        let (alloc_table, allocations) = match alloc_table.as_ref() {
            Some(buf) => {
                let (table, allocs) = decode_alloc_table_bytes(buf)?;
                (Some(table), Some(allocs))
            }
            None => (None, None),
        };

        let mut guest_memory = GUEST_MEMORY.with(|slot| slot.borrow().clone());
        let allocations = allocations.as_deref();
        let d3d9_state = D3D9_STATE
            .with(|slot| slot.borrow_mut().take())
            .ok_or_else(|| {
                JsValue::from_str(
                    "AeroGPU D3D9 executor not initialized. Call init_aerogpu_d3d9(...) first.",
                )
            })?;

        let mut d3d9_state = d3d9_state;

        let exec_result: Result<(), JsValue> = match (alloc_table.as_ref(), guest_memory.as_mut()) {
            (Some(_), None) => Err(JsValue::from_str(
                "guest memory is not configured; call set_guest_memory(Uint8Array) before executing submissions with alloc_table",
            )),
            (Some(table), Some(mem)) => d3d9_state
                .executor
                .execute_cmd_stream_with_guest_memory_for_context_async(
                    context_id,
                    &bytes,
                    mem,
                    Some(table),
                )
                .await
                .map_err(|err| JsValue::from_str(&err.to_string())),
            (None, Some(mem)) => d3d9_state
                .executor
                .execute_cmd_stream_with_guest_memory_for_context_async(
                    context_id, &bytes, mem, None,
                )
                .await
                .map_err(|err| JsValue::from_str(&err.to_string())),
            (None, None) => d3d9_state
                .executor
                .execute_cmd_stream_for_context_async(context_id, &bytes)
                .await
                .map_err(|err| JsValue::from_str(&err.to_string())),
        };

        let processor_result: Result<(Option<u64>, Option<u32>), JsValue> = if exec_result.is_ok() {
            PROCESSOR.with(|processor| {
                let mut processor = processor.borrow_mut();
                let events = processor
                    .process_submission_with_allocations(&bytes, allocations, signal_fence)
                    .map_err(|err| JsValue::from_str(&err.to_string()))?;

                let mut present_count: Option<u64> = None;
                let mut last_present_scanout: Option<u32> = None;
                for event in events {
                    if let AeroGpuEvent::PresentCompleted { scanout_id, .. } = event {
                        last_present_scanout = Some(scanout_id);
                        present_count = Some(processor.present_count());
                    }
                }

                Ok::<(Option<u64>, Option<u32>), JsValue>((present_count, last_present_scanout))
            })
        } else {
            Ok((None, None))
        };

        let present_result = (|| -> Result<(), JsValue> {
            let (_, last_present_scanout) = match processor_result.as_ref() {
                Ok(v) => *v,
                Err(_) => return Ok(()),
            };

            if exec_result.is_ok()
                && let Some(scanout_id) = last_present_scanout
            {
                d3d9_state.last_presented_scanout = Some(scanout_id);

                if let Some(presenter) = d3d9_state.presenter.as_mut() {
                    let device = d3d9_state.executor.device();
                    let queue = d3d9_state.executor.queue();
                    if let Some(scanout) = d3d9_state.executor.presented_scanout(scanout_id) {
                        presenter.present_texture_view(
                            device,
                            queue,
                            scanout.view,
                            scanout.width,
                            scanout.height,
                        )?;
                    } else {
                        presenter.present_clear(device, queue)?;
                    }
                }
            }

            Ok(())
        })();

        D3D9_STATE.with(|slot| {
            *slot.borrow_mut() = Some(d3d9_state);
        });

        exec_result?;
        let (present_count, _last_present_scanout) = processor_result?;
        present_result?;

        let out = Object::new();
        Reflect::set(
            &out,
            &JsValue::from_str("completedFence"),
            &BigInt::from(signal_fence).into(),
        )?;
        if let Some(present_count) = present_count {
            Reflect::set(
                &out,
                &JsValue::from_str("presentCount"),
                &BigInt::from(present_count).into(),
            )?;
        }

        Ok(out.into())
    }

    const FLAG_APPLY_SRGB_ENCODE: u32 = 1;
    const FLAG_PREMULTIPLY_ALPHA: u32 = 2;
    const FLAG_FORCE_OPAQUE_ALPHA: u32 = 4;
    const FLAG_FLIP_Y: u32 = 8;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct ViewportTransform {
        scale: [f32; 2],
        offset: [f32; 2],
    }

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct BlitParams {
        flags: u32,
        _pad: [u32; 3],
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ScaleMode {
        Stretch,
        Fit,
        Integer,
    }

    #[derive(Clone)]
    struct AdapterInfo {
        vendor: Option<String>,
        renderer: Option<String>,
        description: Option<String>,
    }

    fn backend_kind_str(kind: GpuBackendKind) -> &'static str {
        match kind {
            GpuBackendKind::WebGpu => "webgpu",
            GpuBackendKind::WebGl2 => "webgl2",
        }
    }

    fn compute_d3d9_shader_cache_caps_hash(kind: GpuBackendKind, info: &AdapterInfo) -> String {
        // Keep this short-ish: include the backend kind as a human-readable prefix and hash the
        // adapter fields (which can be long, e.g. ANGLE renderer strings).
        let backend = backend_kind_str(kind);
        let vendor = info.vendor.as_deref().unwrap_or("unknown");
        let renderer = info.renderer.as_deref().unwrap_or("unknown");
        let driver = info.description.as_deref().unwrap_or("unknown");
        let raw = format!("d3d9-wgpu-{backend}-{vendor}-{renderer}-{driver}");
        let hash = xxhash_rust::xxh3::xxh3_64(raw.as_bytes());
        format!("d3d9-wgpu-{backend}-{hash:016x}")
    }

    struct Presenter {
        backend_kind: GpuBackendKind,
        adapter_info: AdapterInfo,

        canvas: OffscreenCanvas,

        // Keep the `wgpu::Instance` alive for the lifetime of the surface/device.
        #[allow(dead_code)]
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        device: wgpu::Device,
        queue: wgpu::Queue,

        surface_format: wgpu::TextureFormat,
        alpha_mode: wgpu::CompositeAlphaMode,
        config: wgpu::SurfaceConfiguration,

        pipeline: wgpu::RenderPipeline,
        bind_group_layout: wgpu::BindGroupLayout,

        sampler: wgpu::Sampler,
        scale_mode: ScaleMode,
        clear_color: wgpu::Color,
        viewport_buffer: wgpu::Buffer,
        params_buffer: wgpu::Buffer,

        // Framebuffer texture (RGBA8, linear).
        src_size: (u32, u32),
        framebuffer_texture: wgpu::Texture,
        framebuffer_view: wgpu::TextureView,
        bind_group: wgpu::BindGroup,
        upload_scratch: Vec<u8>,
        upload_scratch_bytes_per_row: u32,

        // Best-effort timing report (CPU only for now).
        profiler: GpuProfiler,
    }

    impl Presenter {
        #[allow(clippy::too_many_arguments)]
        async fn new(
            canvas: OffscreenCanvas,
            backend_kind: GpuBackendKind,
            required_features: wgpu::Features,
            src_width: u32,
            src_height: u32,
            scale_mode: ScaleMode,
            filter_mode: wgpu::FilterMode,
            clear_color: wgpu::Color,
        ) -> Result<Self, JsValue> {
            let backends = match backend_kind {
                GpuBackendKind::WebGpu => wgpu::Backends::BROWSER_WEBGPU,
                // On wasm32, `wgpu`'s GL backend maps to WebGL2 when the `webgl`
                // feature is enabled.
                GpuBackendKind::WebGl2 => wgpu::Backends::GL,
            };

            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });

            let surface = instance
                .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone()))
                .map_err(|err| {
                    JsValue::from_str(&format!("Failed to create wgpu surface: {err:?}"))
                })?;

            let adapter = request_adapter_robust(&instance, &surface)
                .await
                .ok_or_else(|| JsValue::from_str("No suitable GPU adapter found"))?;

            let _supports_view_formats = adapter
                .get_downlevel_capabilities()
                .flags
                .contains(wgpu::DownlevelFlags::VIEW_FORMATS);

            let supported = adapter.features();
            if !supported.contains(required_features) {
                return Err(JsValue::from_str(&format!(
                    "Adapter does not support required features: {required_features:?}"
                )));
            }

            // Keep limits conservative to ensure WebGL2 fallback compatibility.
            let limits = wgpu::Limits::downlevel_webgl2_defaults();

            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aero-gpu-wasm device"),
                        required_features,
                        required_limits: limits,
                    },
                    None,
                )
                .await
                .map_err(|err| JsValue::from_str(&format!("Failed to request device: {err}")))?;

            aero_gpu::register_wgpu_uncaptured_error_handler(&device, backend_kind, push_gpu_event);

            let info = adapter.get_info();
            let adapter_info = AdapterInfo {
                // WebGPU doesn't expose stable vendor strings; surface best-effort info.
                vendor: Some(format!("0x{:04x}", info.vendor)),
                renderer: Some(info.name.clone()),
                description: if info.driver_info.is_empty() {
                    None
                } else {
                    Some(info.driver_info.clone())
                },
            };

            let surface_caps = surface.get_capabilities(&adapter);
            let surface_format = choose_surface_format(&surface_caps.formats);
            let alpha_mode = choose_alpha_mode(&surface_caps.alpha_modes);
            let present_mode = choose_present_mode(&surface_caps.present_modes);

            // Initial surface size is taken from the canvas (physical pixels).
            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: canvas.width().max(1),
                height: canvas.height().max(1),
                present_mode,
                alpha_mode,
                desired_maximum_frame_latency: 2,
                view_formats: vec![],
            };
            surface.configure(&device, &config);
            gpu_stats().inc_surface_reconfigures();

            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aero-gpu-wasm.blit.bind_group_layout"),
                    entries: &[
                        // viewport
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // input texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        // params
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aero-gpu-wasm.blit.pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aero-gpu-wasm.blit.shader"),
                source: wgpu::ShaderSource::Wgsl(builtin_wgsl(BuiltinShader::Blit).into()),
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-gpu-wasm.blit.pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("aero-gpu-wasm.blit.sampler"),
                mag_filter: filter_mode,
                min_filter: filter_mode,
                mipmap_filter: filter_mode,
                ..Default::default()
            });

            let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.blit.viewport_uniform"),
                size: std::mem::size_of::<ViewportTransform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.blit.params_uniform"),
                size: std::mem::size_of::<BlitParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            // Create initial source framebuffer texture at the *source* size (not the canvas size).
            let fb_w = src_width.max(1);
            let fb_h = src_height.max(1);
            let (framebuffer_texture, framebuffer_view) =
                create_framebuffer_texture(&device, fb_w, fb_h);

            // Default present policy (docs/04-graphics-subsystem.md):
            // - input framebuffer is linear RGBA8 (rgba8unorm)
            // - output is sRGB
            // - alpha is forced opaque
            let mut flags = FLAG_FORCE_OPAQUE_ALPHA;
            if needs_srgb_encode_in_shader(surface_format) {
                flags |= FLAG_APPLY_SRGB_ENCODE;
            }
            // Top-left UV origin is the default for the shared blit shader.
            flags &= !FLAG_FLIP_Y;
            flags &= !FLAG_PREMULTIPLY_ALPHA;

            let viewport_transform = compute_viewport_transform(
                canvas.width().max(1),
                canvas.height().max(1),
                fb_w,
                fb_h,
                scale_mode,
            );
            queue.write_buffer(&viewport_buffer, 0, bytemuck::bytes_of(&viewport_transform));
            queue.write_buffer(
                &params_buffer,
                0,
                bytemuck::bytes_of(&BlitParams {
                    flags,
                    _pad: [0; 3],
                }),
            );

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aero-gpu-wasm.blit.bind_group"),
                layout: &bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: viewport_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&framebuffer_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: params_buffer.as_entire_binding(),
                    },
                ],
            });

            Ok(Self {
                backend_kind,
                adapter_info,
                canvas,
                instance,
                surface,
                device,
                queue,
                surface_format,
                alpha_mode,
                config,
                pipeline,
                bind_group_layout,
                sampler,
                scale_mode,
                clear_color,
                viewport_buffer,
                params_buffer,
                src_size: (fb_w, fb_h),
                framebuffer_texture,
                framebuffer_view,
                bind_group,
                upload_scratch: Vec::new(),
                upload_scratch_bytes_per_row: 0,
                profiler: GpuProfiler::new_cpu_only(backend_kind),
            })
        }

        fn backend_kind_string(&self) -> &'static str {
            match self.backend_kind {
                GpuBackendKind::WebGpu => "webgpu",
                GpuBackendKind::WebGl2 => "webgl2",
            }
        }

        fn ensure_surface_matches_canvas(&mut self) {
            // If the canvas is resized externally (without calling `resize()`), the surface can
            // become outdated and `get_current_texture` may fail. Keep the configuration in sync
            // with the current canvas pixel size.
            let w = self.canvas.width().max(1);
            let h = self.canvas.height().max(1);
            if self.config.width != w || self.config.height != h {
                self.config.width = w;
                self.config.height = h;
                self.surface.configure(&self.device, &self.config);
                gpu_stats().inc_surface_reconfigures();
            }
        }

        fn set_canvas_size(&mut self, pixel_width: u32, pixel_height: u32) {
            self.canvas.set_width(pixel_width.max(1));
            self.canvas.set_height(pixel_height.max(1));
        }

        fn resize(
            &mut self,
            src_width: u32,
            src_height: u32,
            out_width_px: u32,
            out_height_px: u32,
        ) {
            let src_width = src_width.max(1);
            let src_height = src_height.max(1);
            let out_width_px = out_width_px.max(1);
            let out_height_px = out_height_px.max(1);

            self.set_canvas_size(out_width_px, out_height_px);

            self.config.width = out_width_px;
            self.config.height = out_height_px;
            self.surface.configure(&self.device, &self.config);
            gpu_stats().inc_surface_reconfigures();

            if self.src_size != (src_width, src_height) {
                let (tex, view) = create_framebuffer_texture(&self.device, src_width, src_height);
                self.framebuffer_texture = tex;
                self.framebuffer_view = view;
                self.src_size = (src_width, src_height);

                self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("aero-gpu-wasm.blit.bind_group"),
                    layout: &self.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: self.viewport_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&self.framebuffer_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: self.params_buffer.as_entire_binding(),
                        },
                    ],
                });
            }

            // Update viewport transform for letterboxing/pillarboxing.
            let viewport_transform = compute_viewport_transform(
                out_width_px,
                out_height_px,
                src_width,
                src_height,
                self.scale_mode,
            );
            self.queue.write_buffer(
                &self.viewport_buffer,
                0,
                bytemuck::bytes_of(&viewport_transform),
            );
        }

        fn upload_rgba8_strided(&mut self, rgba8: &[u8], stride_bytes: u32) -> Result<(), JsValue> {
            let (width, height) = self.src_size;
            if width == 0 || height == 0 {
                return Ok(());
            }

            let tight_row_bytes = width
                .checked_mul(4)
                .ok_or_else(|| JsValue::from_str("Framebuffer width overflow"))?;

            if stride_bytes < tight_row_bytes {
                return Err(JsValue::from_str(&format!(
                    "Invalid stride_bytes: got {stride_bytes}, expected at least {tight_row_bytes}",
                )));
            }

            let expected_len_u64 = u64::from(stride_bytes).saturating_mul(u64::from(height));
            let expected_len = usize::try_from(expected_len_u64)
                .map_err(|_| JsValue::from_str("Framebuffer byte size overflow"))?;
            if rgba8.len() < expected_len {
                return Err(JsValue::from_str(&format!(
                    "Frame data too small: got {}, expected at least {}",
                    rgba8.len(),
                    expected_len
                )));
            }

            let upload_bpr = if height <= 1 {
                // WebGPU's bytesPerRow alignment requirement does not apply when uploading a single
                // row; avoid unnecessary padding/copying for 1-row framebuffers.
                tight_row_bytes
            } else if stride_bytes.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) {
                stride_bytes
            } else {
                padded_bytes_per_row(tight_row_bytes)
            };

            let data: &[u8];
            if height <= 1 {
                data = &rgba8[..tight_row_bytes as usize];
            } else if upload_bpr == stride_bytes {
                data = &rgba8[..expected_len];
            } else {
                let total_u64 = u64::from(upload_bpr).saturating_mul(u64::from(height));
                let total = usize::try_from(total_u64)
                    .map_err(|_| JsValue::from_str("Framebuffer upload scratch size overflow"))?;
                if self.upload_scratch.len() != total
                    || self.upload_scratch_bytes_per_row != upload_bpr
                {
                    self.upload_scratch = vec![0u8; total];
                    self.upload_scratch_bytes_per_row = upload_bpr;
                }

                for y in 0..height as usize {
                    let src_off = y * stride_bytes as usize;
                    let dst_off = y * upload_bpr as usize;
                    let row = &mut self.upload_scratch[dst_off..dst_off + upload_bpr as usize];
                    row[..tight_row_bytes as usize]
                        .copy_from_slice(&rgba8[src_off..src_off + tight_row_bytes as usize]);
                    row[tight_row_bytes as usize..].fill(0);
                }
                data = &self.upload_scratch;
            }

            // The last row does not require padding out to `bytes_per_row`. Slice the input to the
            // minimum required length so wgpu doesn't need to clone/copy extra bytes.
            let required_len = u64::from(upload_bpr)
                .saturating_mul(u64::from(height.saturating_sub(1)))
                .saturating_add(u64::from(tight_row_bytes));
            let required_len = usize::try_from(required_len)
                .map_err(|_| JsValue::from_str("Upload size overflow"))?;

            self.queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.framebuffer_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data[..required_len],
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(upload_bpr),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            let bytes = u64::from(tight_row_bytes).saturating_mul(u64::from(height));
            RGBA8_UPLOAD_BYTES_FULL.fetch_add(bytes, Ordering::Relaxed);
            RGBA8_UPLOAD_FULL_CALLS.fetch_add(1, Ordering::Relaxed);

            Ok(())
        }

        fn upload_rgba8_strided_dirty_rects(
            &mut self,
            rgba8: &[u8],
            stride_bytes: u32,
            rects: &[u32],
        ) -> Result<(), JsValue> {
            let (width, height) = self.src_size;
            if width == 0 || height == 0 {
                return Ok(());
            }

            let tight_row_bytes = width
                .checked_mul(4)
                .ok_or_else(|| JsValue::from_str("Framebuffer width overflow"))?;

            if stride_bytes < tight_row_bytes {
                return Err(JsValue::from_str(&format!(
                    "Invalid stride_bytes: got {stride_bytes}, expected at least {tight_row_bytes}",
                )));
            }

            let expected_len_u64 = u64::from(stride_bytes).saturating_mul(u64::from(height));
            let expected_len = usize::try_from(expected_len_u64)
                .map_err(|_| JsValue::from_str("Framebuffer byte size overflow"))?;
            if rgba8.len() < expected_len {
                return Err(JsValue::from_str(&format!(
                    "Frame data too small: got {}, expected at least {}",
                    rgba8.len(),
                    expected_len
                )));
            }

            // Defensive: if the caller supplies no rects, fall back to full upload.
            if rects.is_empty() {
                return self.upload_rgba8_strided(rgba8, stride_bytes);
            }

            RGBA8_UPLOAD_DIRTY_CALLS.fetch_add(1, Ordering::Relaxed);

            let stride_aligned = stride_bytes.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let mut any_uploaded = false;

            for chunk in rects.chunks_exact(4) {
                let x = chunk[0];
                let y = chunk[1];
                let w = chunk[2];
                let h = chunk[3];

                if w == 0 || h == 0 {
                    continue;
                }

                // Clamp to framebuffer bounds.
                let x0 = x.min(width);
                let y0 = y.min(height);
                let x1 = x.saturating_add(w).min(width);
                let y1 = y.saturating_add(h).min(height);
                let w = x1.saturating_sub(x0);
                let h = y1.saturating_sub(y0);

                if w == 0 || h == 0 {
                    continue;
                }

                let row_bytes = w
                    .checked_mul(4)
                    .ok_or_else(|| JsValue::from_str("Dirty rect width overflow"))?;

                if h <= 1 {
                    // Single-row uploads do not require bytesPerRow alignment; point directly into
                    // the source buffer even when `stride_bytes` is not aligned.
                    let base_off = y0 as usize * stride_bytes as usize + x0 as usize * 4;
                    let end = base_off + row_bytes as usize;
                    let data = &rgba8[base_off..end];

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &self.framebuffer_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(row_bytes),
                            rows_per_image: Some(1),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                    );
                } else if stride_aligned {
                    // Fast path: point directly into the source buffer. WebGPU will only read
                    // `row_bytes` per row, even though `bytes_per_row` is the full stride.
                    let base_off = y0 as usize * stride_bytes as usize + x0 as usize * 4;
                    let required_len = u64::from(stride_bytes)
                        .saturating_mul(u64::from(h.saturating_sub(1)))
                        .saturating_add(u64::from(row_bytes));
                    let required_len = usize::try_from(required_len)
                        .map_err(|_| JsValue::from_str("Upload size overflow"))?;
                    let end = base_off + required_len;
                    let data = &rgba8[base_off..end];

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &self.framebuffer_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        data,
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(stride_bytes),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                } else {
                    // Slow path: pack the rect into an aligned staging buffer.
                    let upload_bpr = padded_bytes_per_row(row_bytes);
                    let total_u64 = u64::from(upload_bpr).saturating_mul(u64::from(h));
                    let total = usize::try_from(total_u64).map_err(|_| {
                        JsValue::from_str("Dirty rect upload scratch size overflow")
                    })?;

                    if self.upload_scratch_bytes_per_row != upload_bpr {
                        self.upload_scratch_bytes_per_row = upload_bpr;
                        self.upload_scratch = vec![0u8; total];
                    } else {
                        self.upload_scratch.resize(total, 0);
                    }

                    for row in 0..h as usize {
                        let src_off = (y0 as usize + row) * stride_bytes as usize + x0 as usize * 4;
                        let dst_off = row * upload_bpr as usize;
                        let dst_row =
                            &mut self.upload_scratch[dst_off..dst_off + upload_bpr as usize];
                        dst_row[..row_bytes as usize]
                            .copy_from_slice(&rgba8[src_off..src_off + row_bytes as usize]);
                        dst_row[row_bytes as usize..].fill(0);
                    }

                    let required_len = u64::from(upload_bpr)
                        .saturating_mul(u64::from(h.saturating_sub(1)))
                        .saturating_add(u64::from(row_bytes));
                    let required_len = usize::try_from(required_len)
                        .map_err(|_| JsValue::from_str("Upload size overflow"))?;

                    self.queue.write_texture(
                        wgpu::ImageCopyTexture {
                            texture: &self.framebuffer_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &self.upload_scratch[..required_len],
                        wgpu::ImageDataLayout {
                            offset: 0,
                            bytes_per_row: Some(upload_bpr),
                            rows_per_image: Some(h),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: h,
                            depth_or_array_layers: 1,
                        },
                    );
                }

                let bytes = u64::from(w).saturating_mul(4).saturating_mul(u64::from(h));
                RGBA8_UPLOAD_BYTES_DIRTY.fetch_add(bytes, Ordering::Relaxed);
                RGBA8_UPLOAD_DIRTY_RECTS.fetch_add(1, Ordering::Relaxed);

                any_uploaded = true;
            }

            if !any_uploaded {
                // Defensive fallback: if all rects were empty/invalid after clamping, do a full
                // upload so consumers don't keep presenting a stale frame.
                return self.upload_rgba8_strided(rgba8, stride_bytes);
            }

            Ok(())
        }

        fn present_with_result(&mut self) -> Result<bool, JsValue> {
            gpu_stats().inc_presents_attempted();
            self.ensure_surface_matches_canvas();

            let device = &self.device;
            let Some(frame) =
                acquire_surface_frame(&mut self.surface, device, &self.config, self.backend_kind)?
            else {
                // docs/04-graphics-subsystem.md: SurfaceError::Timeout drops the frame (warn) and
                // continues without throwing.
                return Ok(false);
            };

            // Start profiler tracking only after we know we're actually going to submit work for
            // this frame. This avoids leaving the profiler in an "in progress" state if we drop
            // the frame due to a surface timeout.
            self.profiler
                .begin_frame(Some(&self.device), Some(&self.queue));
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-wasm.present.encoder"),
            });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-gpu-wasm.present.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(self.clear_color),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }

            self.profiler.end_encode(&mut encoder);
            let command_buffer = encoder.finish();
            self.profiler.submit(&self.queue, command_buffer);
            frame.present();
            gpu_stats().inc_presents_succeeded();
            Ok(true)
        }

        fn present(&mut self) -> Result<(), JsValue> {
            let _ = self.present_with_result()?;
            Ok(())
        }

        async fn screenshot(&self) -> Result<Vec<u8>, JsValue> {
            let (width, height) = self.src_size;
            if width == 0 || height == 0 {
                return Ok(Vec::new());
            }

            let bytes_per_row = width
                .checked_mul(4)
                .ok_or_else(|| JsValue::from_str("Framebuffer width overflow"))?;
            let padded_bpr = padded_bytes_per_row(bytes_per_row);
            let buffer_size = padded_bpr as u64 * height as u64;

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.screenshot.readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });

            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("aero-gpu-wasm.screenshot.encoder"),
                });

            encoder.copy_texture_to_buffer(
                wgpu::ImageCopyTexture {
                    texture: &self.framebuffer_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::ImageCopyBuffer {
                    buffer: &readback,
                    layout: wgpu::ImageDataLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bpr),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );

            self.queue.submit([encoder.finish()]);

            let slice = readback.slice(..);
            let (sender, receiver) = oneshot_channel();
            slice.map_async(wgpu::MapMode::Read, move |res| {
                sender.send(res).ok();
            });

            // `map_async` completion is driven by `Device::poll`. `Maintain::Wait` avoids hangs on
            // some backends where a single `Poll` is insufficient to make progress.
            self.device.poll(wgpu::Maintain::Wait);

            match receiver.receive().await {
                Some(Ok(())) => {}
                Some(Err(err)) => {
                    return Err(JsValue::from_str(&format!(
                        "Failed to map screenshot buffer: {err}"
                    )));
                }
                None => {
                    return Err(JsValue::from_str(
                        "Screenshot map callback dropped unexpectedly",
                    ));
                }
            }

            let mapped = slice.get_mapped_range();
            let mut out = vec![0u8; (bytes_per_row * height) as usize];
            for y in 0..height as usize {
                let src_off = y * padded_bpr as usize;
                let dst_off = y * bytes_per_row as usize;
                out[dst_off..dst_off + bytes_per_row as usize]
                    .copy_from_slice(&mapped[src_off..src_off + bytes_per_row as usize]);
            }
            drop(mapped);
            readback.unmap();

            Ok(out)
        }

        fn latest_timings(&self) -> Option<FrameTimingsReport> {
            self.profiler.get_frame_timings()
        }

        fn adapter_info_js(&self) -> JsValue {
            let obj = Object::new();
            if let Some(vendor) = &self.adapter_info.vendor {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("vendor"),
                    &JsValue::from_str(vendor),
                );
            }
            if let Some(renderer) = &self.adapter_info.renderer {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("renderer"),
                    &JsValue::from_str(renderer),
                );
            }
            if let Some(description) = &self.adapter_info.description {
                let _ = Reflect::set(
                    &obj,
                    &JsValue::from_str("description"),
                    &JsValue::from_str(description),
                );
            }
            obj.into()
        }

        fn capabilities_js(
            &self,
            src_width: u32,
            src_height: u32,
            output_css_width: u32,
            output_css_height: u32,
            dpr: f64,
        ) -> JsValue {
            let pixel_width = self.canvas.width();
            let pixel_height = self.canvas.height();

            let obj = Object::new();
            let _ = Reflect::set(&obj, &JsValue::from_str("initialized"), &JsValue::TRUE);
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("backend"),
                &JsValue::from_str(self.backend_kind_string()),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("surfaceFormat"),
                &JsValue::from_str(&format!("{:?}", self.surface_format)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("alphaMode"),
                &JsValue::from_str(&format!("{:?}", self.alpha_mode)),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("cssSize"),
                &size_obj(output_css_width, output_css_height),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("srcSize"),
                &size_obj(src_width, src_height),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("devicePixelRatio"),
                &JsValue::from_f64(dpr),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("pixelSize"),
                &size_obj(pixel_width, pixel_height),
            );
            obj.into()
        }
    }

    /// Simple surface presenter used by the D3D9 AeroGPU executor.
    ///
    /// Unlike [`Presenter`], this does not own a `wgpu::Device`/`wgpu::Queue`. The executor owns
    /// them, and the presenter borrows them when it needs to blit a scanout to the surface.
    struct ScanoutPresenter {
        backend_kind: GpuBackendKind,
        canvas: OffscreenCanvas,

        // Keep the `wgpu::Instance` alive for the lifetime of the surface.
        #[allow(dead_code)]
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,

        pipeline: wgpu::RenderPipeline,
        bind_group_layout: wgpu::BindGroupLayout,

        sampler: wgpu::Sampler,
        scale_mode: ScaleMode,
        clear_color: wgpu::Color,
        viewport_buffer: wgpu::Buffer,
        params_buffer: wgpu::Buffer,
    }

    impl ScanoutPresenter {
        async fn new(
            canvas: OffscreenCanvas,
            backend_kind: GpuBackendKind,
            required_features: wgpu::Features,
            scale_mode: ScaleMode,
            filter_mode: wgpu::FilterMode,
            clear_color: wgpu::Color,
        ) -> Result<
            (
                Self,
                wgpu::Device,
                wgpu::Queue,
                AdapterInfo,
                wgpu::DownlevelFlags,
            ),
            JsValue,
        > {
            let backends = match backend_kind {
                GpuBackendKind::WebGpu => wgpu::Backends::BROWSER_WEBGPU,
                GpuBackendKind::WebGl2 => wgpu::Backends::GL,
            };

            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends,
                ..Default::default()
            });

            let surface = instance
                .create_surface(wgpu::SurfaceTarget::OffscreenCanvas(canvas.clone()))
                .map_err(|err| {
                    JsValue::from_str(&format!("Failed to create wgpu surface: {err:?}"))
                })?;

            let adapter = request_adapter_robust(&instance, &surface)
                .await
                .ok_or_else(|| JsValue::from_str("No suitable GPU adapter found"))?;

            let downlevel_flags = adapter.get_downlevel_capabilities().flags;

            let supported = adapter.features();
            if !supported.contains(required_features) {
                return Err(JsValue::from_str(&format!(
                    "Adapter does not support required features: {required_features:?}"
                )));
            }

            // Keep limits conservative to ensure WebGL2 fallback compatibility.
            let limits = wgpu::Limits::downlevel_webgl2_defaults();
            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aero-gpu-wasm scanout presenter"),
                        required_features,
                        required_limits: limits,
                    },
                    None,
                )
                .await
                .map_err(|err| JsValue::from_str(&format!("Failed to request device: {err}")))?;

            aero_gpu::register_wgpu_uncaptured_error_handler(&device, backend_kind, push_gpu_event);

            let info = adapter.get_info();
            let adapter_info = AdapterInfo {
                vendor: Some(format!("0x{:04x}", info.vendor)),
                renderer: Some(info.name.clone()),
                description: if info.driver_info.is_empty() {
                    None
                } else {
                    Some(info.driver_info.clone())
                },
            };

            let surface_caps = surface.get_capabilities(&adapter);
            let surface_format = choose_surface_format(&surface_caps.formats);
            let alpha_mode = choose_alpha_mode(&surface_caps.alpha_modes);
            let present_mode = choose_present_mode(&surface_caps.present_modes);

            let config = wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: surface_format,
                width: canvas.width().max(1),
                height: canvas.height().max(1),
                present_mode,
                alpha_mode,
                desired_maximum_frame_latency: 2,
                view_formats: vec![],
            };
            surface.configure(&device, &config);
            gpu_stats().inc_surface_reconfigures();

            let bind_group_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("aero-gpu-wasm.scanout.blit.bind_group_layout"),
                    entries: &[
                        // viewport
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        // input texture
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                multisampled: false,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            },
                            count: None,
                        },
                        // sampler
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        // params
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.pipeline_layout"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.shader"),
                source: wgpu::ShaderSource::Wgsl(builtin_wgsl(BuiltinShader::Blit).into()),
            });

            let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: surface_format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.sampler"),
                mag_filter: filter_mode,
                min_filter: filter_mode,
                mipmap_filter: filter_mode,
                ..Default::default()
            });

            let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.viewport_uniform"),
                size: std::mem::size_of::<ViewportTransform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.params_uniform"),
                size: std::mem::size_of::<BlitParams>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let mut flags = FLAG_FORCE_OPAQUE_ALPHA;
            if needs_srgb_encode_in_shader(surface_format) {
                flags |= FLAG_APPLY_SRGB_ENCODE;
            }
            flags &= !FLAG_FLIP_Y;
            flags &= !FLAG_PREMULTIPLY_ALPHA;

            let viewport_transform = compute_viewport_transform(
                canvas.width().max(1),
                canvas.height().max(1),
                1,
                1,
                scale_mode,
            );
            queue.write_buffer(&viewport_buffer, 0, bytemuck::bytes_of(&viewport_transform));
            queue.write_buffer(
                &params_buffer,
                0,
                bytemuck::bytes_of(&BlitParams {
                    flags,
                    _pad: [0; 3],
                }),
            );

            Ok((
                Self {
                    backend_kind,
                    canvas,
                    instance,
                    surface,
                    config,
                    pipeline,
                    bind_group_layout,
                    sampler,
                    scale_mode,
                    clear_color,
                    viewport_buffer,
                    params_buffer,
                },
                device,
                queue,
                adapter_info,
                downlevel_flags,
            ))
        }

        fn ensure_surface_matches_canvas(&mut self, device: &wgpu::Device) {
            let w = self.canvas.width().max(1);
            let h = self.canvas.height().max(1);
            if self.config.width != w || self.config.height != h {
                self.config.width = w;
                self.config.height = h;
                self.surface.configure(device, &self.config);
                gpu_stats().inc_surface_reconfigures();
            }
        }

        fn present_clear(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
        ) -> Result<(), JsValue> {
            gpu_stats().inc_presents_attempted();
            self.ensure_surface_matches_canvas(device);
            let Some(frame) =
                acquire_surface_frame(&mut self.surface, device, &self.config, self.backend_kind)?
            else {
                // SurfaceError::Timeout drops the frame (docs/04-graphics-subsystem.md).
                return Ok(());
            };
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-wasm.scanout.present_clear.encoder"),
            });

            {
                let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-gpu-wasm.scanout.present_clear.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(self.clear_color),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
            }

            queue.submit([encoder.finish()]);
            frame.present();
            gpu_stats().inc_presents_succeeded();
            Ok(())
        }

        fn present_texture_view(
            &mut self,
            device: &wgpu::Device,
            queue: &wgpu::Queue,
            src_view: &wgpu::TextureView,
            src_width: u32,
            src_height: u32,
        ) -> Result<(), JsValue> {
            gpu_stats().inc_presents_attempted();
            self.ensure_surface_matches_canvas(device);

            let out_width_px = self.config.width.max(1);
            let out_height_px = self.config.height.max(1);
            let src_width = src_width.max(1);
            let src_height = src_height.max(1);

            let viewport_transform = compute_viewport_transform(
                out_width_px,
                out_height_px,
                src_width,
                src_height,
                self.scale_mode,
            );
            queue.write_buffer(
                &self.viewport_buffer,
                0,
                bytemuck::bytes_of(&viewport_transform),
            );

            let Some(frame) =
                acquire_surface_frame(&mut self.surface, device, &self.config, self.backend_kind)?
            else {
                // SurfaceError::Timeout drops the frame (docs/04-graphics-subsystem.md).
                return Ok(());
            };
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("aero-gpu-wasm.scanout.blit.bind_group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.viewport_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(src_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.params_buffer.as_entire_binding(),
                    },
                ],
            });

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aero-gpu-wasm.scanout.present.encoder"),
            });

            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("aero-gpu-wasm.scanout.present.pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(self.clear_color),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..6, 0..1);
            }

            queue.submit([encoder.finish()]);
            frame.present();
            gpu_stats().inc_presents_succeeded();
            Ok(())
        }
    }

    fn create_framebuffer_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aero-gpu-wasm.framebuffer_rgba8"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn padded_bytes_per_row(bytes_per_row: u32) -> u32 {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        bytes_per_row.div_ceil(align) * align
    }

    fn compute_viewport_transform(
        canvas_width_px: u32,
        canvas_height_px: u32,
        src_width: u32,
        src_height: u32,
        mode: ScaleMode,
    ) -> ViewportTransform {
        let canvas_w = canvas_width_px.max(1) as f64;
        let canvas_h = canvas_height_px.max(1) as f64;
        let src_w = src_width.max(1) as f64;
        let src_h = src_height.max(1) as f64;

        let (x, y, w, h) = match mode {
            ScaleMode::Stretch => (0.0, 0.0, canvas_w, canvas_h),
            ScaleMode::Fit | ScaleMode::Integer => {
                let scale_fit = (canvas_w / src_w).min(canvas_h / src_h);
                let scale = if mode == ScaleMode::Integer {
                    let integer = scale_fit.floor();
                    if integer >= 1.0 { integer } else { scale_fit }
                } else {
                    scale_fit
                };

                let w = (src_w * scale).floor().max(1.0);
                let h = (src_h * scale).floor().max(1.0);
                let x = ((canvas_w - w) / 2.0).floor();
                let y = ((canvas_h - h) / 2.0).floor();
                (x, y, w, h)
            }
        };

        let scale_x = (w / canvas_w).clamp(0.0, 1.0);
        let scale_y = (h / canvas_h).clamp(0.0, 1.0);

        // Convert pixel-space viewport (top-left origin) to clip-space scale/offset.
        // X: left=-1, right=+1
        // Y: top=+1, bottom=-1
        let offset_x = ((2.0 * x + w) / canvas_w) - 1.0;
        let offset_y = 1.0 - ((2.0 * y + h) / canvas_h);

        ViewportTransform {
            scale: [scale_x as f32, scale_y as f32],
            offset: [offset_x as f32, offset_y as f32],
        }
    }

    fn needs_srgb_encode_in_shader(format: wgpu::TextureFormat) -> bool {
        // If the surface format is already sRGB, the GPU will encode automatically.
        !matches!(
            format,
            wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
        )
    }

    fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
        // Prefer an sRGB surface format (docs/04-graphics-subsystem.md).
        for &fmt in formats {
            if matches!(
                fmt,
                wgpu::TextureFormat::Bgra8UnormSrgb | wgpu::TextureFormat::Rgba8UnormSrgb
            ) {
                return fmt;
            }
        }
        formats
            .first()
            .copied()
            .unwrap_or(wgpu::TextureFormat::Bgra8Unorm)
    }

    fn choose_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
        // Default to opaque output.
        if modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            return wgpu::CompositeAlphaMode::Opaque;
        }
        modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Opaque)
    }

    fn choose_present_mode(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
        if modes.contains(&wgpu::PresentMode::Fifo) {
            return wgpu::PresentMode::Fifo;
        }
        modes.first().copied().unwrap_or(wgpu::PresentMode::Fifo)
    }

    async fn request_adapter_robust(
        instance: &wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> Option<wgpu::Adapter> {
        for (power, fallback) in [
            (wgpu::PowerPreference::HighPerformance, false),
            (wgpu::PowerPreference::LowPower, false),
            (wgpu::PowerPreference::LowPower, true),
        ] {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power,
                    compatible_surface: Some(surface),
                    force_fallback_adapter: fallback,
                })
                .await;
            if adapter.is_some() {
                return adapter;
            }
        }
        None
    }

    async fn request_adapter_headless(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
        for (power, fallback) in [
            (wgpu::PowerPreference::HighPerformance, false),
            (wgpu::PowerPreference::LowPower, false),
            (wgpu::PowerPreference::LowPower, true),
        ] {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: power,
                    compatible_surface: None,
                    force_fallback_adapter: fallback,
                })
                .await;
            if adapter.is_some() {
                return adapter;
            }
        }
        None
    }

    trait SurfaceFrameProvider {
        type Frame;
        fn acquire(&mut self) -> Result<Self::Frame, wgpu::SurfaceError>;
        fn reconfigure(&mut self);
    }

    fn handle_surface_acquire_error<F>(
        err: wgpu::SurfaceError,
        backend_kind: GpuBackendKind,
        after_reconfigure: bool,
    ) -> Result<Option<F>, JsValue> {
        match err {
            // docs/04-graphics-subsystem.md: timeouts should drop the frame (warn) and continue
            // without throwing. Do not reconfigure the surface.
            wgpu::SurfaceError::Timeout => {
                push_gpu_event(
                    GpuErrorEvent::now(
                        backend_kind,
                        aero_gpu::GpuErrorSeverityKind::Warning,
                        aero_gpu::GpuErrorCategory::Surface,
                        "Surface acquire timeout",
                    )
                    .with_detail("wgpu_surface_error", "Timeout")
                    .with_detail("after_reconfigure", after_reconfigure.to_string()),
                );
                Ok(None)
            }
            // Out of memory is fatal and should stop rendering.
            wgpu::SurfaceError::OutOfMemory => {
                push_gpu_event(
                    GpuErrorEvent::now(
                        backend_kind,
                        aero_gpu::GpuErrorSeverityKind::Fatal,
                        aero_gpu::GpuErrorCategory::OutOfMemory,
                        "Surface out of memory",
                    )
                    .with_detail("wgpu_surface_error", "OutOfMemory"),
                );
                Err(JsValue::from_str("Surface out of memory"))
            }
            // These are expected surface lifecycle events. If the recovery attempt fails, drop the
            // frame (warn) rather than throwing (docs/04-graphics-subsystem.md).
            wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                push_gpu_event(
                    GpuErrorEvent::now(
                        backend_kind,
                        aero_gpu::GpuErrorSeverityKind::Warning,
                        aero_gpu::GpuErrorCategory::Surface,
                        "Surface acquire failed after reconfigure",
                    )
                    .with_detail("wgpu_surface_error", format!("{err:?}")),
                );
                Ok(None)
            }
        }
    }

    fn acquire_surface_frame_inner<S: SurfaceFrameProvider>(
        surface: &mut S,
        backend_kind: GpuBackendKind,
    ) -> Result<Option<S::Frame>, JsValue> {
        match surface.acquire() {
            Ok(frame) => Ok(Some(frame)),
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                // Reconfigure and retry once (docs/04-graphics-subsystem.md).
                //
                // Note: This is *surface* recovery (reconfigure + reacquire), not device-lost
                // recovery. Only `surface_reconfigures` should be incremented here; the
                // `recoveries_*` counters are reserved for the device-lost recovery state machine.
                surface.reconfigure();
                gpu_stats().inc_surface_reconfigures();
                match surface.acquire() {
                    Ok(frame) => Ok(Some(frame)),
                    Err(err) => handle_surface_acquire_error(err, backend_kind, true),
                }
            }
            Err(err) => handle_surface_acquire_error(err, backend_kind, false),
        }
    }

    fn acquire_surface_frame(
        surface: &mut wgpu::Surface<'static>,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
        backend_kind: GpuBackendKind,
    ) -> Result<Option<wgpu::SurfaceTexture>, JsValue> {
        struct WgpuSurfaceFrameProvider<'a> {
            surface: &'a mut wgpu::Surface<'static>,
            device: &'a wgpu::Device,
            config: &'a wgpu::SurfaceConfiguration,
        }

        impl<'a> SurfaceFrameProvider for WgpuSurfaceFrameProvider<'a> {
            type Frame = wgpu::SurfaceTexture;

            fn acquire(&mut self) -> Result<Self::Frame, wgpu::SurfaceError> {
                self.surface.get_current_texture()
            }

            fn reconfigure(&mut self) {
                self.surface.configure(self.device, self.config);
            }
        }

        let mut provider = WgpuSurfaceFrameProvider {
            surface,
            device,
            config,
        };
        acquire_surface_frame_inner(&mut provider, backend_kind)
    }

    #[cfg(test)]
    mod surface_tests {
        use super::*;
        use wasm_bindgen_test::*;

        wasm_bindgen_test_configure!(run_in_browser);

        #[derive(Default)]
        struct FakeSurface {
            outcomes: std::collections::VecDeque<Result<u32, wgpu::SurfaceError>>,
            reconfigure_calls: u32,
        }

        impl FakeSurface {
            fn new(outcomes: impl IntoIterator<Item = Result<u32, wgpu::SurfaceError>>) -> Self {
                Self {
                    outcomes: outcomes.into_iter().collect(),
                    reconfigure_calls: 0,
                }
            }
        }

        impl FakeSurface {
            fn acquire(&mut self) -> Result<u32, wgpu::SurfaceError> {
                self.outcomes
                    .pop_front()
                    .unwrap_or(Err(wgpu::SurfaceError::Timeout))
            }

            fn reconfigure(&mut self) {
                self.reconfigure_calls += 1;
            }
        }

        impl SurfaceFrameProvider for FakeSurface {
            type Frame = u32;

            fn acquire(&mut self) -> Result<Self::Frame, wgpu::SurfaceError> {
                FakeSurface::acquire(self)
            }

            fn reconfigure(&mut self) {
                FakeSurface::reconfigure(self)
            }
        }

        #[wasm_bindgen_test]
        fn timeout_drops_frame_without_reconfigure_and_does_not_throw() {
            // Clear any previous events from other tests.
            let _ = gpu_event_queue().drain();

            let mut surface = FakeSurface::new([Err(wgpu::SurfaceError::Timeout)]);
            let result = acquire_surface_frame_inner(&mut surface, GpuBackendKind::WebGpu);
            assert!(
                result.is_ok(),
                "Timeout should not return Err/throw; got {result:?}"
            );
            assert_eq!(result.unwrap(), None);
            assert_eq!(
                surface.reconfigure_calls, 0,
                "Timeout should not trigger surface reconfigure"
            );

            let events = gpu_event_queue().drain();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].severity, aero_gpu::GpuErrorSeverityKind::Warning);
            assert_eq!(events[0].category, aero_gpu::GpuErrorCategory::Surface);
        }
    }

    fn size_obj(width: u32, height: u32) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("width"),
            &JsValue::from_f64(width as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("height"),
            &JsValue::from_f64(height as f64),
        );
        obj.into()
    }

    struct GpuState {
        src_width: u32,
        src_height: u32,
        output_width_override: Option<u32>,
        output_height_override: Option<u32>,
        device_pixel_ratio: f64,
        presenter: Presenter,
    }

    thread_local! {
        static STATE: RefCell<Option<GpuState>> = const { RefCell::new(None) };
    }

    struct AerogpuD3d9State {
        backend_kind: GpuBackendKind,
        adapter_info: AdapterInfo,
        shader_cache_caps_hash: String,
        executor: AerogpuD3d9Executor,
        presenter: Option<ScanoutPresenter>,
        last_presented_scanout: Option<u32>,
    }

    thread_local! {
        static D3D9_STATE: RefCell<Option<AerogpuD3d9State>> = const { RefCell::new(None) };
    }

    fn with_state<T>(f: impl FnOnce(&GpuState) -> Result<T, JsValue>) -> Result<T, JsValue> {
        STATE.with(|state| match state.borrow().as_ref() {
            Some(s) => f(s),
            None => Err(JsValue::from_str("GPU backend not initialized.")),
        })
    }

    fn with_state_mut<T>(
        f: impl FnOnce(&mut GpuState) -> Result<T, JsValue>,
    ) -> Result<T, JsValue> {
        STATE.with(|state| match state.borrow_mut().as_mut() {
            Some(s) => f(s),
            None => Err(JsValue::from_str("GPU backend not initialized.")),
        })
    }

    fn parse_bool(obj: &JsValue, key: &str) -> Option<bool> {
        if obj.is_undefined() || obj.is_null() {
            return None;
        }
        let value = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
        if value.is_undefined() || value.is_null() {
            return None;
        }
        value.as_bool()
    }

    fn parse_u32(obj: &JsValue, key: &str) -> Option<u32> {
        if obj.is_undefined() || obj.is_null() {
            return None;
        }
        let value = Reflect::get(obj, &JsValue::from_str(key)).ok()?;
        if value.is_undefined() || value.is_null() {
            return None;
        }
        let n = value.as_f64()?;
        if !n.is_finite() || n <= 0.0 {
            return None;
        }
        Some(n.round() as u32)
    }

    fn parse_filter_mode(obj: &JsValue) -> Result<wgpu::FilterMode, JsValue> {
        if obj.is_undefined() || obj.is_null() {
            return Ok(wgpu::FilterMode::Nearest);
        }
        let value = Reflect::get(obj, &JsValue::from_str("filter")).unwrap_or(JsValue::UNDEFINED);
        if value.is_undefined() || value.is_null() {
            return Ok(wgpu::FilterMode::Nearest);
        }
        let Some(mode) = value.as_string() else {
            return Err(JsValue::from_str(
                "Presenter filter must be a string ('nearest' | 'linear')",
            ));
        };
        match mode.as_str() {
            "nearest" => Ok(wgpu::FilterMode::Nearest),
            "linear" => Ok(wgpu::FilterMode::Linear),
            other => Err(JsValue::from_str(&format!(
                "Unsupported filter mode: {other} (expected 'nearest' or 'linear')"
            ))),
        }
    }

    fn parse_scale_mode(obj: &JsValue) -> Result<ScaleMode, JsValue> {
        if obj.is_undefined() || obj.is_null() {
            return Ok(ScaleMode::Fit);
        }
        let value =
            Reflect::get(obj, &JsValue::from_str("scaleMode")).unwrap_or(JsValue::UNDEFINED);
        if value.is_undefined() || value.is_null() {
            return Ok(ScaleMode::Fit);
        }
        let Some(mode) = value.as_string() else {
            return Err(JsValue::from_str(
                "scaleMode must be a string ('stretch' | 'fit' | 'integer')",
            ));
        };
        match mode.as_str() {
            "stretch" => Ok(ScaleMode::Stretch),
            "fit" => Ok(ScaleMode::Fit),
            "integer" => Ok(ScaleMode::Integer),
            other => Err(JsValue::from_str(&format!(
                "Unsupported scaleMode: {other} (expected 'stretch' | 'fit' | 'integer')"
            ))),
        }
    }

    fn parse_clear_color(obj: &JsValue) -> Result<wgpu::Color, JsValue> {
        let default = wgpu::Color {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };

        if obj.is_undefined() || obj.is_null() {
            return Ok(default);
        }
        let value =
            Reflect::get(obj, &JsValue::from_str("clearColor")).unwrap_or(JsValue::UNDEFINED);
        if value.is_undefined() || value.is_null() {
            return Ok(default);
        }
        if !Array::is_array(&value) {
            return Err(JsValue::from_str(
                "clearColor must be a 4-element array [r,g,b,a]",
            ));
        }
        let arr: Array = value.unchecked_into();
        if arr.length() < 4 {
            return Err(JsValue::from_str(
                "clearColor must have at least 4 elements [r,g,b,a]",
            ));
        }
        let to_num = |v: JsValue, idx: usize| -> Result<f64, JsValue> {
            v.as_f64()
                .ok_or_else(|| JsValue::from_str(&format!("clearColor[{idx}] must be a number")))
        };
        Ok(wgpu::Color {
            r: to_num(arr.get(0), 0)?,
            g: to_num(arr.get(1), 1)?,
            b: to_num(arr.get(2), 2)?,
            a: to_num(arr.get(3), 3)?,
        })
    }

    fn parse_required_features(obj: &JsValue) -> Result<wgpu::Features, JsValue> {
        if obj.is_undefined() || obj.is_null() {
            return Ok(wgpu::Features::empty());
        }
        let value =
            Reflect::get(obj, &JsValue::from_str("requiredFeatures")).unwrap_or(JsValue::UNDEFINED);
        if value.is_undefined() || value.is_null() {
            return Ok(wgpu::Features::empty());
        }
        if !Array::is_array(&value) {
            return Err(JsValue::from_str(
                "GpuWorkerInitOptions.requiredFeatures must be an array of strings",
            ));
        }
        let arr: Array = value.unchecked_into();
        let mut out = wgpu::Features::empty();
        for entry in arr.iter() {
            let Some(name) = entry.as_string() else {
                return Err(JsValue::from_str(
                    "GpuWorkerInitOptions.requiredFeatures must contain only strings",
                ));
            };
            out |= map_webgpu_feature(&name)?;
        }
        Ok(out)
    }

    fn map_webgpu_feature(name: &str) -> Result<wgpu::Features, JsValue> {
        match name {
            "texture-compression-bc" => Ok(wgpu::Features::TEXTURE_COMPRESSION_BC),
            "texture-compression-etc2" => Ok(wgpu::Features::TEXTURE_COMPRESSION_ETC2),
            // wgpu exposes ASTC via the `*_ASTC_HDR` flag; browsers treat this as
            // a single "texture-compression-astc" capability.
            "texture-compression-astc" => Ok(wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR),
            "timestamp-query" => {
                Ok(wgpu::Features::TIMESTAMP_QUERY
                    | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
            }
            other => Err(JsValue::from_str(&format!(
                "Unsupported WebGPU feature: {other}"
            ))),
        }
    }

    fn clamp_pixel_size(css: u32, dpr: f64) -> u32 {
        let ratio = if dpr.is_finite() && dpr > 0.0 {
            dpr
        } else {
            1.0
        };
        ((css as f64) * ratio).round().max(1.0) as u32
    }

    fn make_test_pattern(width: u32, height: u32) -> Result<Vec<u8>, JsValue> {
        let half_w = width / 2;
        let half_h = height / 2;
        let len_u64 = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|v| v.checked_mul(4))
            .ok_or_else(|| JsValue::from_str("Test pattern size overflow (width*height*4)"))?;
        let len =
            usize::try_from(len_u64).map_err(|_| JsValue::from_str("Test pattern too large"))?;
        let mut rgba = vec![0u8; len];

        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                let left = x < half_w;
                let top = y < half_h;

                // Top-left origin:
                // - top-left: red
                // - top-right: green
                // - bottom-left: blue
                // - bottom-right: white
                let (r, g, b) = match (top, left) {
                    (true, true) => (255, 0, 0),
                    (true, false) => (0, 255, 0),
                    (false, true) => (0, 0, 255),
                    (false, false) => (255, 255, 255),
                };

                rgba[i] = r;
                rgba[i + 1] = g;
                rgba[i + 2] = b;
                rgba[i + 3] = 255;
            }
        }
        Ok(rgba)
    }

    fn timings_to_js(report: &FrameTimingsReport) -> JsValue {
        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("frame_index"),
            &JsValue::from_f64(report.frame_index as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("backend"),
            &JsValue::from_str(match report.backend {
                GpuBackendKind::WebGpu => "webgpu",
                GpuBackendKind::WebGl2 => "webgl2",
            }),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("cpu_encode_us"),
            &JsValue::from_f64(report.cpu_encode_us as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("cpu_submit_us"),
            &JsValue::from_f64(report.cpu_submit_us as f64),
        );
        if let Some(gpu_us) = report.gpu_us {
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("gpu_us"),
                &JsValue::from_f64(gpu_us as f64),
            );
        }
        obj.into()
    }

    #[wasm_bindgen]
    pub async fn init_gpu(
        offscreen_canvas: OffscreenCanvas,
        width: u32,
        height: u32,
        dpr: f64,
        options: Option<JsValue>,
    ) -> Result<(), JsValue> {
        crate::install_panic_hook_once();
        let options = options.unwrap_or(JsValue::UNDEFINED);

        // Align default behavior with the TS runtime worker: try WebGPU unless explicitly
        // opted out (preferWebGpu === false) or disableWebGpu === true.
        let prefer_webgpu = parse_bool(&options, "preferWebGpu").unwrap_or(true);
        let disable_webgpu = parse_bool(&options, "disableWebGpu").unwrap_or(false);

        let src_width = width.max(1);
        let src_height = height.max(1);

        // Optional output size override (CSS pixels).
        let output_width_override = parse_u32(&options, "outputWidth");
        let output_height_override = parse_u32(&options, "outputHeight");

        let device_pixel_ratio = if dpr.is_finite() && dpr > 0.0 {
            dpr
        } else {
            1.0
        };

        let output_css_width = output_width_override.unwrap_or(src_width);
        let output_css_height = output_height_override.unwrap_or(src_height);

        let out_width_px = clamp_pixel_size(output_css_width, device_pixel_ratio);
        let out_height_px = clamp_pixel_size(output_css_height, device_pixel_ratio);
        offscreen_canvas.set_width(out_width_px);
        offscreen_canvas.set_height(out_height_px);

        let scale_mode = parse_scale_mode(&options)?;
        let filter_mode = parse_filter_mode(&options)?;
        let clear_color = parse_clear_color(&options)?;

        let backends = if disable_webgpu {
            vec![GpuBackendKind::WebGl2]
        } else if prefer_webgpu {
            vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
        } else {
            vec![GpuBackendKind::WebGl2, GpuBackendKind::WebGpu]
        };

        let mut last_err: Option<JsValue> = None;
        for backend_kind in backends {
            // Required WebGPU features are only meaningful for the WebGPU path. When
            // falling back to WebGL2, ignore them.
            let required_features = match backend_kind {
                GpuBackendKind::WebGpu => parse_required_features(&options)?,
                GpuBackendKind::WebGl2 => wgpu::Features::empty(),
            };

            match Presenter::new(
                offscreen_canvas.clone(),
                backend_kind,
                required_features,
                src_width,
                src_height,
                scale_mode,
                filter_mode,
                clear_color,
            )
            .await
            {
                Ok(mut presenter) => {
                    presenter.resize(src_width, src_height, out_width_px, out_height_px);

                    let state = GpuState {
                        src_width,
                        src_height,
                        output_width_override,
                        output_height_override,
                        device_pixel_ratio,
                        presenter,
                    };

                    STATE.with(|slot| {
                        *slot.borrow_mut() = Some(state);
                    });

                    return Ok(());
                }
                Err(err) => {
                    last_err = Some(err);
                }
            }
        }

        Err(last_err
            .unwrap_or_else(|| JsValue::from_str("No supported GPU backend could be initialized.")))
    }

    #[wasm_bindgen]
    pub async fn init_aerogpu_d3d9(
        offscreen_canvas: Option<OffscreenCanvas>,
        options: Option<JsValue>,
    ) -> Result<(), JsValue> {
        crate::install_panic_hook_once();
        let options = options.unwrap_or(JsValue::UNDEFINED);

        let prefer_webgpu = parse_bool(&options, "preferWebGpu").unwrap_or(true);
        let disable_webgpu = parse_bool(&options, "disableWebGpu").unwrap_or(false);

        let scale_mode = parse_scale_mode(&options)?;
        let filter_mode = parse_filter_mode(&options)?;
        let clear_color = parse_clear_color(&options)?;

        if let Some(canvas) = offscreen_canvas {
            let backends = if disable_webgpu {
                vec![GpuBackendKind::WebGl2]
            } else if prefer_webgpu {
                vec![GpuBackendKind::WebGpu, GpuBackendKind::WebGl2]
            } else {
                vec![GpuBackendKind::WebGl2, GpuBackendKind::WebGpu]
            };

            let mut last_err: Option<JsValue> = None;
            for backend_kind in backends {
                let required_features = match backend_kind {
                    GpuBackendKind::WebGpu => parse_required_features(&options)?,
                    GpuBackendKind::WebGl2 => wgpu::Features::empty(),
                };

                match ScanoutPresenter::new(
                    canvas.clone(),
                    backend_kind,
                    required_features,
                    scale_mode,
                    filter_mode,
                    clear_color,
                )
                .await
                {
                    Ok((presenter, device, queue, adapter_info, downlevel_flags)) => {
                        let stable_caps_hash =
                            compute_d3d9_shader_cache_caps_hash(backend_kind, &adapter_info);
                        let mut executor = AerogpuD3d9Executor::new(
                            device,
                            queue,
                            downlevel_flags,
                            gpu_stats().clone(),
                        );
                        let shader_cache_caps_hash = executor
                            .set_shader_cache_caps_hash(Some(stable_caps_hash))
                            .unwrap_or_default();

                        D3D9_STATE.with(|slot| {
                            *slot.borrow_mut() = Some(AerogpuD3d9State {
                                backend_kind,
                                adapter_info,
                                shader_cache_caps_hash,
                                executor,
                                presenter: Some(presenter),
                                last_presented_scanout: None,
                            });
                        });

                        return Ok(());
                    }
                    Err(err) => last_err = Some(err),
                }
            }

            Err(last_err.unwrap_or_else(|| {
                JsValue::from_str("No supported GPU backend could be initialized.")
            }))
        } else {
            if disable_webgpu {
                return Err(JsValue::from_str(
                    "Headless AeroGPU D3D9 executor requires WebGPU; disableWebGpu was set.",
                ));
            }

            let required_features = parse_required_features(&options)?;
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::BROWSER_WEBGPU,
                ..Default::default()
            });
            let adapter = request_adapter_headless(&instance)
                .await
                .ok_or_else(|| JsValue::from_str("No suitable GPU adapter found"))?;

            let supported = adapter.features();
            if !supported.contains(required_features) {
                return Err(JsValue::from_str(&format!(
                    "Adapter does not support required features: {required_features:?}"
                )));
            }

            let limits = wgpu::Limits::downlevel_webgl2_defaults();
            let (device, queue) = adapter
                .request_device(
                    &wgpu::DeviceDescriptor {
                        label: Some("aero-gpu-wasm AerogpuD3d9Executor (headless)"),
                        required_features,
                        required_limits: limits,
                    },
                    None,
                )
                .await
                .map_err(|err| JsValue::from_str(&format!("Failed to request device: {err}")))?;

            aero_gpu::register_wgpu_uncaptured_error_handler(
                &device,
                GpuBackendKind::WebGpu,
                push_gpu_event,
            );

            let info = adapter.get_info();
            let downlevel_flags = adapter.get_downlevel_capabilities().flags;
            let adapter_info = AdapterInfo {
                vendor: Some(format!("0x{:04x}", info.vendor)),
                renderer: Some(info.name.clone()),
                description: if info.driver_info.is_empty() {
                    None
                } else {
                    Some(info.driver_info.clone())
                },
            };

            let shader_cache_caps_hash =
                compute_d3d9_shader_cache_caps_hash(GpuBackendKind::WebGpu, &adapter_info);
            let mut executor =
                AerogpuD3d9Executor::new(device, queue, downlevel_flags, gpu_stats().clone());
            let shader_cache_caps_hash = executor
                .set_shader_cache_caps_hash(Some(shader_cache_caps_hash))
                .unwrap_or_default();
            D3D9_STATE.with(|slot| {
                *slot.borrow_mut() = Some(AerogpuD3d9State {
                    backend_kind: GpuBackendKind::WebGpu,
                    adapter_info,
                    shader_cache_caps_hash,
                    executor,
                    presenter: None,
                    last_presented_scanout: None,
                });
            });

            Ok(())
        }
    }

    #[wasm_bindgen]
    pub fn resize(
        width: u32,
        height: u32,
        dpr: f64,
        output_width_css: u32,
        output_height_css: u32,
    ) -> Result<(), JsValue> {
        with_state_mut(|state| {
            state.src_width = width.max(1);
            state.src_height = height.max(1);
            state.device_pixel_ratio = if dpr.is_finite() && dpr > 0.0 {
                dpr
            } else {
                1.0
            };

            // Backward-compatible: if callers omit the output size arguments (or pass 0),
            // keep whatever override was configured at init time.
            if output_width_css > 0 {
                state.output_width_override = Some(output_width_css);
            }
            if output_height_css > 0 {
                state.output_height_override = Some(output_height_css);
            }

            let output_css_width = state.output_width_override.unwrap_or(state.src_width);
            let output_css_height = state.output_height_override.unwrap_or(state.src_height);

            let out_width_px = clamp_pixel_size(output_css_width, state.device_pixel_ratio);
            let out_height_px = clamp_pixel_size(output_css_height, state.device_pixel_ratio);
            state.presenter.resize(
                state.src_width,
                state.src_height,
                out_width_px,
                out_height_px,
            );
            Ok(())
        })
    }

    #[wasm_bindgen]
    pub fn backend_kind() -> Result<String, JsValue> {
        if let Some(kind) = STATE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|state| state.presenter.backend_kind_string().to_string())
        }) {
            return Ok(kind);
        }

        if let Some(kind) = D3D9_STATE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|state| match state.backend_kind {
                    GpuBackendKind::WebGpu => "webgpu".to_string(),
                    GpuBackendKind::WebGl2 => "webgl2".to_string(),
                })
        }) {
            return Ok(kind);
        }

        Err(JsValue::from_str("GPU backend not initialized."))
    }

    #[wasm_bindgen]
    pub fn adapter_info() -> Result<JsValue, JsValue> {
        if let Some(info) = STATE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|state| state.presenter.adapter_info_js())
        }) {
            return Ok(info);
        }

        if let Some(info) = D3D9_STATE.with(|slot| {
            slot.borrow().as_ref().map(|state| {
                let obj = Object::new();
                if let Some(vendor) = &state.adapter_info.vendor {
                    let _ = Reflect::set(
                        &obj,
                        &JsValue::from_str("vendor"),
                        &JsValue::from_str(vendor),
                    );
                }
                if let Some(renderer) = &state.adapter_info.renderer {
                    let _ = Reflect::set(
                        &obj,
                        &JsValue::from_str("renderer"),
                        &JsValue::from_str(renderer),
                    );
                }
                if let Some(description) = &state.adapter_info.description {
                    let _ = Reflect::set(
                        &obj,
                        &JsValue::from_str("description"),
                        &JsValue::from_str(description),
                    );
                }
                obj.into()
            })
        }) {
            return Ok(info);
        }

        Err(JsValue::from_str("GPU backend not initialized."))
    }

    #[wasm_bindgen]
    pub fn capabilities() -> Result<JsValue, JsValue> {
        with_state(|state| {
            let output_css_width = state.output_width_override.unwrap_or(state.src_width);
            let output_css_height = state.output_height_override.unwrap_or(state.src_height);
            Ok(state.presenter.capabilities_js(
                state.src_width,
                state.src_height,
                output_css_width,
                output_css_height,
                state.device_pixel_ratio,
            ))
        })
    }

    #[wasm_bindgen]
    pub fn present_test_pattern() -> Result<(), JsValue> {
        with_state_mut(|state| {
            let (w, h) = state.presenter.src_size;
            let rgba = make_test_pattern(w, h)?;
            state.presenter.upload_rgba8_strided(&rgba, w * 4)?;
            state.presenter.present()
        })
    }

    #[wasm_bindgen]
    pub fn present_rgba8888(frame: &[u8], stride_bytes: u32) -> Result<(), JsValue> {
        with_state_mut(|state| {
            state.presenter.upload_rgba8_strided(frame, stride_bytes)?;
            state.presenter.present()
        })
    }

    /// Present an RGBA8888 frame and return whether it was actually presented.
    ///
    /// Returns `false` when the frame is intentionally dropped due to surface acquire failures
    /// (e.g. timeout, lost/outdated after reconfigure), matching docs/04.
    #[wasm_bindgen]
    pub fn present_rgba8888_with_result(frame: &[u8], stride_bytes: u32) -> Result<bool, JsValue> {
        with_state_mut(|state| {
            state.presenter.upload_rgba8_strided(frame, stride_bytes)?;
            state.presenter.present_with_result()
        })
    }

    /// Upload an RGBA8888 frame into the internal framebuffer texture without presenting it.
    ///
    /// This is primarily useful for tests/benchmarks that validate upload behavior independent
    /// of surface presentation.
    #[wasm_bindgen]
    pub fn upload_rgba8888(frame: &[u8], stride_bytes: u32) -> Result<(), JsValue> {
        with_state_mut(|state| state.presenter.upload_rgba8_strided(frame, stride_bytes))
    }

    #[wasm_bindgen]
    pub fn present_rgba8888_dirty_rects(
        frame: &[u8],
        stride_bytes: u32,
        rects: &[u32],
    ) -> Result<(), JsValue> {
        with_state_mut(|state| {
            state
                .presenter
                .upload_rgba8_strided_dirty_rects(frame, stride_bytes, rects)?;
            state.presenter.present()
        })
    }

    /// Dirty-rect present variant that returns whether it was actually presented.
    ///
    /// See `present_rgba8888_with_result` for drop semantics.
    #[wasm_bindgen]
    pub fn present_rgba8888_dirty_rects_with_result(
        frame: &[u8],
        stride_bytes: u32,
        rects: &[u32],
    ) -> Result<bool, JsValue> {
        with_state_mut(|state| {
            state
                .presenter
                .upload_rgba8_strided_dirty_rects(frame, stride_bytes, rects)?;
            state.presenter.present_with_result()
        })
    }

    /// Upload dirty rects from an RGBA8888 frame into the internal framebuffer texture without presenting.
    #[wasm_bindgen]
    pub fn upload_rgba8888_dirty_rects(
        frame: &[u8],
        stride_bytes: u32,
        rects: &[u32],
    ) -> Result<(), JsValue> {
        with_state_mut(|state| {
            state
                .presenter
                .upload_rgba8_strided_dirty_rects(frame, stride_bytes, rects)
        })
    }

    #[wasm_bindgen]
    pub async fn request_screenshot() -> Result<Uint8Array, JsValue> {
        let d3d9_state = D3D9_STATE.with(|slot| slot.borrow_mut().take());
        if let Some(d3d9_state) = d3d9_state {
            let result = if let Some(scanout_id) = d3d9_state.last_presented_scanout {
                d3d9_state
                    .executor
                    .read_presented_scanout_rgba8(scanout_id)
                    .await
                    .map_err(|err| JsValue::from_str(&err.to_string()))
                    .map(|opt| opt.map(|(_, _, bytes)| bytes).unwrap_or_default())
            } else {
                Ok(Vec::new())
            };

            D3D9_STATE.with(|slot| {
                *slot.borrow_mut() = Some(d3d9_state);
            });

            let bytes = result?;
            return Ok(Uint8Array::from(bytes.as_slice()));
        }

        let state = STATE
            .with(|slot| slot.borrow_mut().take())
            .ok_or_else(|| JsValue::from_str("GPU backend not initialized."))?;

        let result = state.presenter.screenshot().await;

        // Restore state regardless of whether screenshot succeeds.
        STATE.with(|slot| {
            *slot.borrow_mut() = Some(state);
        });

        let bytes = result?;
        Ok(Uint8Array::from(bytes.as_slice()))
    }

    /// Request a screenshot along with its dimensions.
    ///
    /// - When the D3D9 executor is initialized, this captures the last-presented scanout.
    /// - Otherwise, it captures the legacy RGBA presenter framebuffer.
    ///
    /// Returned object shape:
    /// `{ width: number, height: number, rgba8: ArrayBuffer, origin: "top-left" }`.
    #[wasm_bindgen]
    pub async fn request_screenshot_info() -> Result<JsValue, JsValue> {
        let d3d9_state = D3D9_STATE.with(|slot| slot.borrow_mut().take());
        if let Some(d3d9_state) = d3d9_state {
            let result = if let Some(scanout_id) = d3d9_state.last_presented_scanout {
                d3d9_state
                    .executor
                    .read_presented_scanout_rgba8(scanout_id)
                    .await
                    .map_err(|err| JsValue::from_str(&err.to_string()))
                    .map(|opt| opt.unwrap_or((0, 0, Vec::new())))
            } else {
                Ok((0, 0, Vec::new()))
            };

            D3D9_STATE.with(|slot| {
                *slot.borrow_mut() = Some(d3d9_state);
            });

            let (width, height, bytes) = result?;
            let rgba8 = Uint8Array::from(bytes.as_slice()).buffer();

            let out = Object::new();
            Reflect::set(
                &out,
                &JsValue::from_str("width"),
                &JsValue::from_f64(width as f64),
            )?;
            Reflect::set(
                &out,
                &JsValue::from_str("height"),
                &JsValue::from_f64(height as f64),
            )?;
            Reflect::set(&out, &JsValue::from_str("rgba8"), &rgba8)?;
            Reflect::set(
                &out,
                &JsValue::from_str("origin"),
                &JsValue::from_str("top-left"),
            )?;
            return Ok(out.into());
        }

        let state = STATE.with(|slot| slot.borrow_mut().take());
        let Some(state) = state else {
            return Err(JsValue::from_str("GPU backend not initialized."));
        };

        let (width, height) = state.presenter.src_size;
        let result = state.presenter.screenshot().await;

        // Restore state regardless of whether screenshot succeeds.
        STATE.with(|slot| {
            *slot.borrow_mut() = Some(state);
        });

        let bytes = result?;
        let rgba8 = Uint8Array::from(bytes.as_slice()).buffer();

        let out = Object::new();
        Reflect::set(
            &out,
            &JsValue::from_str("width"),
            &JsValue::from_f64(width as f64),
        )?;
        Reflect::set(
            &out,
            &JsValue::from_str("height"),
            &JsValue::from_f64(height as f64),
        )?;
        Reflect::set(&out, &JsValue::from_str("rgba8"), &rgba8)?;
        Reflect::set(
            &out,
            &JsValue::from_str("origin"),
            &JsValue::from_str("top-left"),
        )?;
        Ok(out.into())
    }

    #[wasm_bindgen]
    pub fn get_frame_timings() -> Result<JsValue, JsValue> {
        with_state(|state| match state.presenter.latest_timings() {
            Some(report) => Ok(timings_to_js(&report)),
            None => Ok(JsValue::NULL),
        })
    }

    #[wasm_bindgen]
    pub fn destroy_gpu() -> Result<(), JsValue> {
        STATE.with(|slot| {
            *slot.borrow_mut() = None;
        });
        D3D9_STATE.with(|slot| {
            *slot.borrow_mut() = None;
        });
        // Clear any queued diagnostics events so callers don't see stale errors after a reset.
        let _ = gpu_event_queue().drain();
        // `submit_aerogpu`/`submit_aerogpu_d3d9` are backed by a lightweight command processor
        // that caches resource descriptors, shared-surface mappings, and monotonic counters.
        //
        // Reset it alongside the GPU state so callers can safely reuse protocol handles after a
        // teardown/re-init cycle (e.g. GPU worker runtime restarts).
        PROCESSOR.with(|processor| {
            *processor.borrow_mut() = AeroGpuCommandProcessor::new();
        });
        Ok(())
    }
}

// Re-export wasm bindings so the crate's public surface is identical across
// `crate::` and `crate::wasm::` paths.
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
