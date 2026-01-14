// Note: This crate is built for both the single-threaded WASM variant (pinned stable toolchain)
// and the threaded/shared-memory variant (pinned nightly toolchain for `-Z build-std`). Keep the
// Rust code free of unstable language features so both builds remain viable.

use aero_gpu_vga::DisplayOutput;
use wasm_bindgen::prelude::*;

pub mod guest_cpu_bench;

#[cfg(any(target_arch = "wasm32", test))]
mod ehci_webusb_topology;
#[cfg(any(target_arch = "wasm32", test))]
mod webusb_ports;
#[cfg(target_arch = "wasm32")]
pub use webusb_ports::WEBUSB_ROOT_PORT;

#[cfg(any(target_arch = "wasm32", test))]
mod guest_phys;

#[cfg(any(target_arch = "wasm32", test))]
mod guest_memory_bus;

// Re-export Aero IPC SharedArrayBuffer ring helpers so the generated `aero-wasm`
// wasm-pack package exposes them to JS (both threaded + single builds).
#[cfg(target_arch = "wasm32")]
pub use aero_ipc::wasm::{SharedRingBuffer, open_ring_by_kind};

#[cfg(target_arch = "wasm32")]
mod guest_layout;

#[cfg(target_arch = "wasm32")]
mod runtime_alloc;

#[cfg(target_arch = "wasm32")]
mod webhid_parse;

#[cfg(target_arch = "wasm32")]
mod vm;

#[cfg(target_arch = "wasm32")]
pub use vm::WasmVm;

#[cfg(any(target_arch = "wasm32", test))]
mod jit_write_log;

#[cfg(any(target_arch = "wasm32", test))]
mod opfs_virtual_disk;

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
mod vm_snapshot_device_kind;

#[cfg(any(target_arch = "wasm32", test))]
mod vm_snapshot_payload_version;

#[cfg(any(target_arch = "wasm32", test))]
mod demo_renderer;

#[cfg(target_arch = "wasm32")]
mod webusb_uhci_passthrough_harness;

#[cfg(target_arch = "wasm32")]
pub use webusb_uhci_passthrough_harness::WebUsbUhciPassthroughHarness;

#[cfg(target_arch = "wasm32")]
mod webusb_ehci_passthrough_harness;
#[cfg(target_arch = "wasm32")]
pub use webusb_ehci_passthrough_harness::WebUsbEhciPassthroughHarness;
#[cfg(target_arch = "wasm32")]
mod uhci_runtime;
#[cfg(target_arch = "wasm32")]
pub use uhci_runtime::UhciRuntime;

#[cfg(target_arch = "wasm32")]
mod hda_controller_bridge;
#[cfg(target_arch = "wasm32")]
pub use hda_controller_bridge::HdaControllerBridge;

#[cfg(target_arch = "wasm32")]
mod worker_vm_snapshot;
#[cfg(target_arch = "wasm32")]
pub use worker_vm_snapshot::WorkerVmSnapshot;

#[cfg(target_arch = "wasm32")]
mod uhci_controller_bridge;

#[cfg(target_arch = "wasm32")]
pub use uhci_controller_bridge::UhciControllerBridge;

#[cfg(target_arch = "wasm32")]
mod usb_topology;

#[cfg(target_arch = "wasm32")]
mod ehci_controller_bridge;
#[cfg(target_arch = "wasm32")]
pub use ehci_controller_bridge::EhciControllerBridge;
#[cfg(target_arch = "wasm32")]
mod xhci_controller_bridge;
#[cfg(target_arch = "wasm32")]
pub use xhci_controller_bridge::{XHCI_STEP_FRAMES_MAX_FRAMES, XhciControllerBridge};

#[cfg(target_arch = "wasm32")]
mod e1000_bridge;
#[cfg(target_arch = "wasm32")]
pub use e1000_bridge::E1000Bridge;

#[cfg(target_arch = "wasm32")]
mod aerogpu_bridge;
#[cfg(target_arch = "wasm32")]
pub use aerogpu_bridge::AerogpuBridge;

#[cfg(target_arch = "wasm32")]
mod i8042_bridge;
#[cfg(target_arch = "wasm32")]
pub use i8042_bridge::I8042Bridge;

#[cfg(target_arch = "wasm32")]
mod pc_machine;
#[cfg(target_arch = "wasm32")]
pub use pc_machine::PcMachine;

#[cfg(target_arch = "wasm32")]
mod webusb_uhci_bridge;

#[cfg(target_arch = "wasm32")]
pub use webusb_uhci_bridge::WebUsbUhciBridge;

mod virtio_input_bridge;
#[cfg(target_arch = "wasm32")]
pub use virtio_input_bridge::VirtioInputPciDevice;
pub use virtio_input_bridge::VirtioInputPciDeviceCore;

#[cfg(target_arch = "wasm32")]
mod virtio_net_pci_bridge;
#[cfg(target_arch = "wasm32")]
pub use virtio_net_pci_bridge::VirtioNetPciBridge;

#[cfg(target_arch = "wasm32")]
mod virtio_snd_pci_bridge;
#[cfg(target_arch = "wasm32")]
pub use virtio_snd_pci_bridge::VirtioSndPciBridge;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::worklet_bridge::WorkletBridge;

#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
use aero_shared::shared_framebuffer::{
    SharedFramebuffer, SharedFramebufferLayout, SharedFramebufferWriter,
};

#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
use aero_shared::scanout_state::{
    SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_LEGACY_VBE_LFB,
    SCANOUT_SOURCE_WDDM, SCANOUT_STATE_BYTE_LEN, ScanoutState, ScanoutStateUpdate,
};

#[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
use aero_shared::cursor_state::{CURSOR_STATE_BYTE_LEN, CursorState};

#[cfg(target_arch = "wasm32")]
use aero_opfs::OpfsSyncFile;

#[cfg(target_arch = "wasm32")]
use aero_platform::audio::mic_bridge::MicBridge;

#[cfg(target_arch = "wasm32")]
use js_sys::{BigInt, Error, Object, Reflect, SharedArrayBuffer, Uint8Array};

#[cfg(target_arch = "wasm32")]
use aero_audio::hda::HdaController;

#[cfg(target_arch = "wasm32")]
use aero_audio::mem::MemoryAccess;

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
    hid::{GamepadReport, UsbHidConsumerControl, UsbHidGamepad, UsbHidKeyboard, UsbHidMouse},
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
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    {
        // Ensure the TLS dummy is not optimized away.
        TLS_DUMMY.with(|v| core::hint::black_box(*v));
    }
}

// Some browser-only APIs used by `aero-wasm` (notably OPFS sync access handles) are worker-only.
//
// Do not globally force wasm-bindgen tests to run in a browser worker. CI runs wasm-bindgen tests
// under Node (`wasm-pack test --node`), and configuring the entire crate as worker-only would skip
// all unit tests in that environment.
//
// Tests that require a worker should opt into worker mode in their specific test crate via:
// `wasm_bindgen_test_configure!(run_in_worker);`

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
// Browser storage capability probes
// -------------------------------------------------------------------------------------------------

/// Return cheap, synchronous feature probes for browser persistence backends.
///
/// The JS host can call this before attempting OPFS-backed disk/ISO attachment to surface clearer
/// diagnostics (e.g. "OPFS unavailable" vs "sync access handles require a worker").
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn storage_capabilities() -> JsValue {
    let obj = Object::new();

    let global = js_sys::global();
    let opfs_supported = aero_opfs::platform::storage::opfs::is_opfs_supported();
    let opfs_sync_supported = aero_opfs::platform::storage::opfs::opfs_sync_access_supported();
    let is_worker_scope = aero_opfs::platform::storage::opfs::is_worker_scope();
    let cross_origin_isolated = opfs_cross_origin_isolated().unwrap_or(false);
    let shared_array_buffer_supported =
        Reflect::has(&global, &JsValue::from_str("SharedArrayBuffer")).unwrap_or(false);
    let is_secure_context = Reflect::get(&global, &JsValue::from_str("isSecureContext"))
        .ok()
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("opfsSupported"),
        &JsValue::from_bool(opfs_supported),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("opfsSyncAccessSupported"),
        &JsValue::from_bool(opfs_sync_supported),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("isWorkerScope"),
        &JsValue::from_bool(is_worker_scope),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("crossOriginIsolated"),
        &JsValue::from_bool(cross_origin_isolated),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("sharedArrayBufferSupported"),
        &JsValue::from_bool(shared_array_buffer_supported),
    );
    let _ = Reflect::set(
        &obj,
        &JsValue::from_str("isSecureContext"),
        &JsValue::from_bool(is_secure_context),
    );

    obj.into()
}

// -------------------------------------------------------------------------------------------------
// Tier-1 JIT ABI constants (browser integration)
// -------------------------------------------------------------------------------------------------

/// Return the Tier-1 JIT ABI constants that JS glue code must agree on.
///
/// These values are derived from `aero_cpu_core::state` and define the in-memory
/// ABI between:
/// - the interpreter / JIT runtime (Rust, inside `aero-wasm`), and
/// - dynamically generated Tier-1 blocks (WASM) executed via `__aero_jit_call`,
///   plus any JS-side rollback/snapshot logic.
///
/// Keeping these values sourced from Rust avoids silently breaking browser
/// integration if `CpuState` layout changes.
#[wasm_bindgen]
pub fn jit_abi_constants() -> JsValue {
    #[cfg(target_arch = "wasm32")]
    {
        use aero_cpu_core::state::{
            CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF, CPU_STATE_ALIGN, CPU_STATE_SIZE, GPR_COUNT,
        };
        use aero_jit_x86::jit_ctx::{
            CODE_VERSION_TABLE_LEN_OFFSET, CODE_VERSION_TABLE_PTR_OFFSET, JitContext,
            TIER2_CTX_OFFSET, TIER2_CTX_SIZE, TRACE_EXIT_REASON_OFFSET,
        };
        use aero_jit_x86::{
            JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, PAGE_OFFSET_MASK, PAGE_SHIFT, PAGE_SIZE,
            TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
        };
        use js_sys::Uint32Array;

        let obj = Object::new();

        let set_u32 = |key: &str, value: u32| {
            Reflect::set(&obj, &JsValue::from_str(key), &JsValue::from(value))
                .expect("Reflect::set should succeed on a fresh object");
        };

        set_u32("cpu_state_size", CPU_STATE_SIZE as u32);
        set_u32("cpu_state_align", CPU_STATE_ALIGN as u32);
        set_u32("cpu_rip_off", CPU_RIP_OFF as u32);
        set_u32("cpu_rflags_off", CPU_RFLAGS_OFF as u32);
        set_u32("jit_ctx_ram_base_offset", JitContext::RAM_BASE_OFFSET);
        set_u32("jit_ctx_tlb_salt_offset", JitContext::TLB_SALT_OFFSET);
        set_u32("jit_ctx_tlb_offset", JitContext::TLB_OFFSET);
        set_u32("jit_ctx_header_bytes", JitContext::BYTE_SIZE as u32);
        set_u32("jit_ctx_total_bytes", JitContext::TOTAL_BYTE_SIZE as u32);
        set_u32("page_shift", PAGE_SHIFT);
        set_u32("page_size", PAGE_SIZE as u32);
        set_u32("page_offset_mask", PAGE_OFFSET_MASK as u32);
        set_u32("jit_tlb_entries", JIT_TLB_ENTRIES as u32);
        set_u32("jit_tlb_entry_bytes", JIT_TLB_ENTRY_SIZE);
        set_u32("jit_tlb_flag_read", TLB_FLAG_READ as u32);
        set_u32("jit_tlb_flag_write", TLB_FLAG_WRITE as u32);
        set_u32("jit_tlb_flag_exec", TLB_FLAG_EXEC as u32);
        set_u32("jit_tlb_flag_is_ram", TLB_FLAG_IS_RAM as u32);
        set_u32("tier2_ctx_offset", TIER2_CTX_OFFSET);
        set_u32("tier2_ctx_size", TIER2_CTX_SIZE);
        set_u32("trace_exit_reason_offset", TRACE_EXIT_REASON_OFFSET);
        set_u32(
            "code_version_table_ptr_offset",
            CODE_VERSION_TABLE_PTR_OFFSET,
        );
        set_u32(
            "code_version_table_len_offset",
            CODE_VERSION_TABLE_LEN_OFFSET,
        );
        let commit_flag_offset = TIER2_CTX_OFFSET + TIER2_CTX_SIZE;
        set_u32("commit_flag_offset", commit_flag_offset);
        set_u32("commit_flag_bytes", 4);

        let mut gpr_off_u32 = [0u32; GPR_COUNT];
        for (i, off) in CPU_GPR_OFF.iter().enumerate() {
            gpr_off_u32[i] = *off as u32;
        }
        let gpr_arr = Uint32Array::from(&gpr_off_u32[..]);
        Reflect::set(&obj, &JsValue::from_str("cpu_gpr_off"), &gpr_arr.into())
            .expect("Reflect::set(cpu_gpr_off) should succeed on a fresh object");

        obj.into()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        JsValue::NULL
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod jit_abi_constants_tests {
    use super::jit_abi_constants;
    use crate::tiered_vm::tiered_vm_jit_abi_layout;

    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    use js_sys::{Reflect, Uint32Array};

    use aero_cpu_core::state::{
        CPU_GPR_OFF, CPU_RFLAGS_OFF, CPU_RIP_OFF, CPU_STATE_ALIGN, CPU_STATE_SIZE, GPR_COUNT,
    };
    use aero_jit_x86::jit_ctx::{
        CODE_VERSION_TABLE_LEN_OFFSET, CODE_VERSION_TABLE_PTR_OFFSET, JitContext, TIER2_CTX_OFFSET,
        TIER2_CTX_SIZE, TRACE_EXIT_REASON_OFFSET,
    };
    use aero_jit_x86::{
        JIT_TLB_ENTRIES, JIT_TLB_ENTRY_SIZE, PAGE_OFFSET_MASK, PAGE_SHIFT, PAGE_SIZE,
        TLB_FLAG_EXEC, TLB_FLAG_IS_RAM, TLB_FLAG_READ, TLB_FLAG_WRITE,
    };

    fn read_u32(obj: &JsValue, key: &str) -> u32 {
        Reflect::get(obj, &JsValue::from_str(key))
            .unwrap_or_else(|_| panic!("missing jit_abi_constants key: {key}"))
            .as_f64()
            .unwrap_or_else(|| panic!("jit_abi_constants[{key}] must be a number"))
            .round() as u32
    }

    #[wasm_bindgen_test]
    fn exports_expected_jit_abi_constants() {
        let obj = jit_abi_constants();
        assert!(obj.is_object(), "jit_abi_constants must return an object");

        assert_eq!(read_u32(&obj, "cpu_state_size"), CPU_STATE_SIZE as u32);
        assert_eq!(read_u32(&obj, "cpu_state_align"), CPU_STATE_ALIGN as u32);
        assert_eq!(read_u32(&obj, "cpu_rip_off"), CPU_RIP_OFF as u32);
        assert_eq!(read_u32(&obj, "cpu_rflags_off"), CPU_RFLAGS_OFF as u32);

        assert_eq!(
            read_u32(&obj, "jit_ctx_ram_base_offset"),
            JitContext::RAM_BASE_OFFSET
        );
        assert_eq!(
            read_u32(&obj, "jit_ctx_tlb_salt_offset"),
            JitContext::TLB_SALT_OFFSET
        );
        assert_eq!(read_u32(&obj, "jit_ctx_tlb_offset"), JitContext::TLB_OFFSET);
        assert_eq!(
            read_u32(&obj, "jit_ctx_header_bytes"),
            JitContext::BYTE_SIZE as u32
        );
        assert_eq!(
            read_u32(&obj, "jit_ctx_total_bytes"),
            JitContext::TOTAL_BYTE_SIZE as u32
        );
        assert_eq!(read_u32(&obj, "page_shift"), PAGE_SHIFT);
        assert_eq!(read_u32(&obj, "page_size"), PAGE_SIZE as u32);
        assert_eq!(read_u32(&obj, "page_offset_mask"), PAGE_OFFSET_MASK as u32);
        assert_eq!(read_u32(&obj, "jit_tlb_entries"), JIT_TLB_ENTRIES as u32);
        assert_eq!(read_u32(&obj, "jit_tlb_entry_bytes"), JIT_TLB_ENTRY_SIZE);
        assert_eq!(read_u32(&obj, "jit_tlb_flag_read"), TLB_FLAG_READ as u32);
        assert_eq!(read_u32(&obj, "jit_tlb_flag_write"), TLB_FLAG_WRITE as u32);
        assert_eq!(read_u32(&obj, "jit_tlb_flag_exec"), TLB_FLAG_EXEC as u32);
        assert_eq!(
            read_u32(&obj, "jit_tlb_flag_is_ram"),
            TLB_FLAG_IS_RAM as u32
        );
        assert_eq!(read_u32(&obj, "tier2_ctx_offset"), TIER2_CTX_OFFSET);
        assert_eq!(read_u32(&obj, "tier2_ctx_size"), TIER2_CTX_SIZE);
        assert_eq!(
            read_u32(&obj, "trace_exit_reason_offset"),
            TRACE_EXIT_REASON_OFFSET
        );
        assert_eq!(
            read_u32(&obj, "code_version_table_ptr_offset"),
            CODE_VERSION_TABLE_PTR_OFFSET
        );
        assert_eq!(
            read_u32(&obj, "code_version_table_len_offset"),
            CODE_VERSION_TABLE_LEN_OFFSET
        );
        assert_eq!(
            read_u32(&obj, "commit_flag_offset"),
            TIER2_CTX_OFFSET + TIER2_CTX_SIZE
        );
        assert_eq!(read_u32(&obj, "commit_flag_bytes"), 4);

        // Self-consistency checks: ensure the exported values satisfy the same layout contract
        // that the JS host relies on.
        let cpu_state_size = read_u32(&obj, "cpu_state_size");
        let cpu_state_align = read_u32(&obj, "cpu_state_align");
        assert_eq!(
            cpu_state_size % cpu_state_align,
            0,
            "cpu_state_size must be a multiple of cpu_state_align"
        );

        let jit_ctx_header_bytes = read_u32(&obj, "jit_ctx_header_bytes");
        let jit_ctx_tlb_offset = read_u32(&obj, "jit_ctx_tlb_offset");
        assert_eq!(
            jit_ctx_tlb_offset, jit_ctx_header_bytes,
            "jit_ctx_tlb_offset must equal jit_ctx_header_bytes"
        );

        let jit_ctx_total_bytes = read_u32(&obj, "jit_ctx_total_bytes");
        let jit_tlb_entries = read_u32(&obj, "jit_tlb_entries");
        let jit_tlb_entry_bytes = read_u32(&obj, "jit_tlb_entry_bytes");
        let derived_total_u64 = u64::from(jit_ctx_header_bytes)
            + u64::from(jit_tlb_entries) * u64::from(jit_tlb_entry_bytes);
        assert!(
            derived_total_u64 <= u64::from(u32::MAX),
            "derived jit ctx total bytes must fit in u32"
        );
        let derived_total = derived_total_u64 as u32;
        assert_eq!(
            jit_ctx_total_bytes, derived_total,
            "jit_ctx_total_bytes must match header + entries*entry_bytes"
        );

        let tier2_ctx_offset = read_u32(&obj, "tier2_ctx_offset");
        assert_eq!(
            tier2_ctx_offset,
            cpu_state_size + jit_ctx_total_bytes,
            "tier2_ctx_offset must follow CpuState + JitContext"
        );

        assert_eq!(
            read_u32(&obj, "code_version_table_ptr_offset"),
            tier2_ctx_offset + 4,
            "code_version_table_ptr_offset must equal tier2_ctx_offset + 4"
        );
        assert_eq!(
            read_u32(&obj, "code_version_table_len_offset"),
            tier2_ctx_offset + 8,
            "code_version_table_len_offset must equal tier2_ctx_offset + 8"
        );

        let tier2_ctx_size = read_u32(&obj, "tier2_ctx_size");
        let commit_flag_offset = read_u32(&obj, "commit_flag_offset");
        assert_eq!(
            commit_flag_offset,
            tier2_ctx_offset + tier2_ctx_size,
            "commit_flag_offset must follow tier2 ctx"
        );
        assert_eq!(
            commit_flag_offset % 4,
            0,
            "commit_flag_offset must be 4-byte aligned"
        );

        let gpr =
            Reflect::get(&obj, &JsValue::from_str("cpu_gpr_off")).expect("cpu_gpr_off missing");
        let gpr = gpr
            .dyn_into::<Uint32Array>()
            .expect("cpu_gpr_off must be Uint32Array");
        assert_eq!(gpr.length() as usize, GPR_COUNT);
        for (i, expected) in CPU_GPR_OFF.iter().enumerate() {
            assert_eq!(
                gpr.get_index(i as u32),
                *expected as u32,
                "CPU_GPR_OFF[{i}] mismatch"
            );
        }

        // Additional layout invariants that the JS host relies on.
        assert!(
            cpu_state_align.is_power_of_two(),
            "cpu_state_align must be power-of-two"
        );
        assert_eq!(
            jit_ctx_header_bytes % 8,
            0,
            "jit_ctx_header_bytes must be 8-byte aligned"
        );
        assert!(
            jit_tlb_entries.is_power_of_two(),
            "jit_tlb_entries must be power-of-two"
        );
        assert_eq!(
            jit_tlb_entry_bytes % 8,
            0,
            "jit_tlb_entry_bytes must be 8-byte aligned"
        );
        assert_eq!(
            tier2_ctx_offset % 4,
            0,
            "tier2_ctx_offset must be 4-byte aligned"
        );
        assert_eq!(
            tier2_ctx_size % 4,
            0,
            "tier2_ctx_size must be 4-byte aligned"
        );
    }

    #[wasm_bindgen_test]
    fn tiered_vm_jit_abi_layout_matches_jit_abi_constants() {
        let obj = jit_abi_constants();
        let layout = tiered_vm_jit_abi_layout();

        assert_eq!(
            layout.jit_ctx_ptr_offset(),
            read_u32(&obj, "cpu_state_size"),
            "jit_ctx_ptr_offset should equal CpuState size"
        );
        assert_eq!(
            layout.jit_ctx_header_bytes(),
            read_u32(&obj, "jit_ctx_header_bytes")
        );
        assert_eq!(layout.jit_tlb_entries(), read_u32(&obj, "jit_tlb_entries"));
        assert_eq!(
            layout.jit_tlb_entry_bytes(),
            read_u32(&obj, "jit_tlb_entry_bytes")
        );
        assert_eq!(
            layout.tier2_ctx_offset(),
            read_u32(&obj, "tier2_ctx_offset")
        );
        assert_eq!(layout.tier2_ctx_bytes(), read_u32(&obj, "tier2_ctx_size"));
        assert_eq!(
            layout.trace_exit_reason_offset(),
            read_u32(&obj, "trace_exit_reason_offset")
        );
        assert_eq!(
            layout.code_version_table_ptr_offset(),
            read_u32(&obj, "code_version_table_ptr_offset")
        );
        assert_eq!(
            layout.code_version_table_len_offset(),
            read_u32(&obj, "code_version_table_len_offset")
        );
        assert_eq!(
            layout.commit_flag_offset(),
            read_u32(&obj, "commit_flag_offset")
        );
        assert_eq!(
            layout.commit_flag_offset(),
            layout.tier2_ctx_offset() + layout.tier2_ctx_bytes(),
            "commit_flag_offset should follow tier2 ctx"
        );
    }
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

    // Guest RAM lives in the 32-bit guest physical address space.
    //
    // Clamp it to:
    // - the wasm32 4GiB linear-memory limit (accounting for the runtime-reserved region), and
    // - the fixed PCI MMIO BAR window start, so PCI BARs never overlap guest RAM.
    let max_guest_pages_by_wasm = guest_layout::MAX_WASM32_PAGES.saturating_sub(base_pages);
    let max_guest_bytes_by_wasm = max_guest_pages_by_wasm * guest_layout::WASM_PAGE_BYTES;
    let max_guest_bytes = max_guest_bytes_by_wasm.min(guest_layout::GUEST_PCI_MMIO_BASE);

    // `desired_bytes` is u32 so it cannot represent 4GiB; align up safely in u64.
    let desired_bytes_aligned =
        guest_layout::align_up(desired_bytes as u64, guest_layout::WASM_PAGE_BYTES);
    let desired_bytes_clamped = desired_bytes_aligned.min(max_guest_bytes);
    let guest_pages = desired_bytes_clamped / guest_layout::WASM_PAGE_BYTES;
    let guest_size = guest_pages * guest_layout::WASM_PAGE_BYTES;

    GuestRamLayout {
        guest_base: guest_base as u32,
        guest_size: guest_size as u32,
        runtime_reserved: guest_base as u32,
    }
}

/// Validate that a `guest_base`/`guest_size` mapping provided by the JS host
/// matches the wasm-side shared guest RAM layout contract.
///
/// This is a defensive check used by wasm-bindgen "machine" constructors that
/// directly dereference guest physical memory via raw pointers into the module's
/// linear memory.
///
/// Without this validation, a mismatched coordinator/runtime can construct a VM
/// whose guest RAM overlaps the Rust runtime heap (UB) or extends past the end
/// of the imported `WebAssembly.Memory` (OOB traps/UB).
#[cfg(target_arch = "wasm32")]
pub(crate) fn validate_shared_guest_ram_layout(
    api: &str,
    guest_base: u32,
    guest_size: u32,
) -> Result<u64, JsValue> {
    let expected_base = guest_layout::align_up(
        guest_layout::RUNTIME_RESERVED_BYTES,
        guest_layout::WASM_PAGE_BYTES,
    );
    let guest_base_u64 = u64::from(guest_base);

    // Validate that guest RAM begins after the reserved runtime region and is
    // page-aligned (wasm linear memory is addressed in 64KiB pages).
    if guest_base_u64 < expected_base {
        return Err(js_error(format!(
            "{api}: invalid guest_base=0x{guest_base:x}. Guest RAM must begin at or above the reserved runtime region end (expected >= 0x{expected_base:x}).\n\
This usually means the JS coordinator's shared-guest-memory layout constants are out of sync with this WASM build.\n\
Fix: ensure `web/src/runtime/shared_layout.ts` matches `crates/aero-wasm/src/guest_layout.rs` and rebuild both together."
        )));
    }
    if guest_base_u64 % guest_layout::WASM_PAGE_BYTES != 0 {
        return Err(js_error(format!(
            "{api}: invalid guest_base=0x{guest_base:x}. guest_base must be 64KiB-aligned (wasm page size)."
        )));
    }

    // Resolve the effective guest size.
    //
    // Historical note: some wasm-bindgen APIs accept `guest_size == 0` as a
    // "use the remainder of linear memory" sentinel. The web coordinator
    // always passes an explicit `guest_size`, but keep supporting `0` for
    // tests/harnesses while still validating the resulting mapping.
    let mem_pages = core::arch::wasm32::memory_size(0) as u64;
    let mem_bytes = mem_pages.saturating_mul(guest_layout::WASM_PAGE_BYTES);
    let guest_size_u64 = if guest_size == 0 {
        mem_bytes.saturating_sub(guest_base_u64)
    } else {
        u64::from(guest_size)
    };

    // The shared guest RAM layout reserves the high PCI MMIO window; guest RAM
    // must not extend into that region.
    if guest_size_u64 > guest_layout::GUEST_PCI_MMIO_BASE {
        return Err(js_error(format!(
            "{api}: invalid guest_size=0x{guest_size_u64:x}. Guest RAM must be <= 0x{mmio_base:x} (GUEST_PCI_MMIO_BASE) so PCI MMIO BARs never overlap guest RAM.",
            mmio_base = guest_layout::GUEST_PCI_MMIO_BASE
        )));
    }

    let end = guest_base_u64
        .checked_add(guest_size_u64)
        .ok_or_else(|| js_error(format!("{api}: guest_base + guest_size overflow")))?;
    if end > mem_bytes {
        return Err(js_error(format!(
            "{api}: guest RAM mapping out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} end=0x{end:x} wasm_mem=0x{mem_bytes:x}.\n\
Fix: ensure the imported WebAssembly.Memory is created with byteLength >= guest_base + guest_size (and that the coordinator/WASM agree on the layout)."
        )));
    }

    Ok(guest_size_u64)
}

#[cfg(all(test, target_arch = "wasm32"))]
mod guest_ram_layout_tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn clamps_max_guest_ram_to_pci_mmio_base() {
        // The web runtime reserves the high 512MiB of the 32-bit guest physical address space for
        // PCI MMIO BARs, so guest RAM must never exceed `PCI_MMIO_BASE`.
        let layout = guest_ram_layout(u32::MAX);
        assert_eq!(
            layout.guest_size(),
            guest_layout::GUEST_PCI_MMIO_BASE as u32
        );
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod shared_guest_ram_layout_validation_tests {
    use super::validate_shared_guest_ram_layout;

    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn js_err_message(err: wasm_bindgen::JsValue) -> String {
        if let Some(e) = err.dyn_ref::<js_sys::Error>() {
            return e.message().into();
        }
        if let Some(s) = err.as_string() {
            return s;
        }
        "<non-string js error>".to_string()
    }

    fn ensure_reserved_pages() {
        let reserved_pages = (super::guest_layout::RUNTIME_RESERVED_BYTES
            / super::guest_layout::WASM_PAGE_BYTES) as usize;
        let cur = core::arch::wasm32::memory_size(0);
        if cur < reserved_pages {
            let delta = reserved_pages - cur;
            let prev = core::arch::wasm32::memory_grow(0, delta);
            assert_ne!(prev, usize::MAX, "memory.grow failed in test setup");
        }
    }

    #[wasm_bindgen_test]
    fn rejects_guest_base_below_runtime_reserved() {
        ensure_reserved_pages();
        let err = validate_shared_guest_ram_layout("test", 0x10000, 0x10000)
            .expect_err("expected validation error");
        let msg = js_err_message(err);
        assert!(
            msg.contains("invalid guest_base"),
            "unexpected message: {msg}"
        );
    }

    #[wasm_bindgen_test]
    fn rejects_unaligned_guest_base() {
        ensure_reserved_pages();
        let base = super::guest_layout::RUNTIME_RESERVED_BYTES as u32 + 1;
        let err = validate_shared_guest_ram_layout(
            "test",
            base,
            super::guest_layout::WASM_PAGE_BYTES as u32,
        )
        .expect_err("expected validation error");
        let msg = js_err_message(err);
        assert!(msg.contains("64KiB-aligned"), "unexpected message: {msg}");
    }

    #[wasm_bindgen_test]
    fn rejects_guest_size_overlapping_mmio_window() {
        ensure_reserved_pages();
        let base = super::guest_layout::RUNTIME_RESERVED_BYTES as u32;
        let too_big = (super::guest_layout::GUEST_PCI_MMIO_BASE + 0x1_0000) as u32;
        let err = validate_shared_guest_ram_layout("test", base, too_big)
            .expect_err("expected validation error");
        let msg = js_err_message(err);
        assert!(
            msg.contains("GUEST_PCI_MMIO_BASE"),
            "unexpected message: {msg}"
        );
    }

    #[wasm_bindgen_test]
    fn rejects_guest_ram_mapping_past_end_of_memory() {
        ensure_reserved_pages();
        let base = super::guest_layout::RUNTIME_RESERVED_BYTES as u32;
        let mem_pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = mem_pages.saturating_mul(super::guest_layout::WASM_PAGE_BYTES);
        let available = mem_bytes.saturating_sub(u64::from(base));
        // Choose a size that is in the valid MMIO-clamped range but extends past the current memory.
        let size = (available + 0x1_0000).min(super::guest_layout::GUEST_PCI_MMIO_BASE) as u32;
        let err = validate_shared_guest_ram_layout("test", base, size)
            .expect_err("expected validation error");
        let msg = js_err_message(err);
        assert!(msg.contains("out of bounds"), "unexpected message: {msg}");
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
fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
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
    // NOTE: Avoid serde_wasm_bindgen (and JSON stringify/parse roundtrips) here.
    //
    // Aero's threaded WASM runtime runs with a fixed-size imported memory (maximum == initial) so
    // `WebAssembly.Memory.grow()` is never invoked (to preserve a stable SharedArrayBuffer across
    // workers). Some serde_wasm_bindgen code paths can trigger `std::alloc::System` allocations
    // which attempt to grow memory and fail under this constraint.
    let collections = crate::webhid_parse::parse_webhid_collections(&collections_json)?;

    let bytes = synthesize_webhid_report_descriptor_bytes(&collections)
        .map_err(|err| js_error(format!("Failed to synthesize HID report descriptor: {err}")))?;

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
    {
        // Shared-memory (threaded wasm) builds: use atomic byte stores to avoid Rust data-race UB when
        // wasm linear memory is backed by a `SharedArrayBuffer`.
        #[cfg(feature = "wasm-threaded")]
        {
            use core::sync::atomic::{AtomicU8, Ordering};
            let dst: *mut AtomicU8 = core::ptr::with_exposed_provenance_mut(offset as usize);
            let bytes = value.to_le_bytes();
            for (i, byte) in bytes.into_iter().enumerate() {
                // Safety: the caller is responsible for providing a valid linear-memory offset;
                // `AtomicU8` has alignment 1.
                unsafe { (&*dst.add(i)).store(byte, Ordering::Relaxed) };
            }
        }

        #[cfg(not(feature = "wasm-threaded"))]
        unsafe {
            let dst: *mut u32 = core::ptr::with_exposed_provenance_mut(offset as usize);
            core::ptr::write_unaligned(dst, value);
        }
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
    {
        #[cfg(feature = "wasm-threaded")]
        {
            use core::sync::atomic::{AtomicU8, Ordering};
            let src: *const AtomicU8 = core::ptr::with_exposed_provenance(offset as usize);
            // Safety: the caller is responsible for providing a valid linear-memory offset;
            // `AtomicU8` has alignment 1.
            let bytes = unsafe {
                [
                    (&*src.add(0)).load(Ordering::Relaxed),
                    (&*src.add(1)).load(Ordering::Relaxed),
                    (&*src.add(2)).load(Ordering::Relaxed),
                    (&*src.add(3)).load(Ordering::Relaxed),
                ]
            };
            u32::from_le_bytes(bytes)
        }

        #[cfg(not(feature = "wasm-threaded"))]
        unsafe {
            let src: *const u32 = core::ptr::with_exposed_provenance(offset as usize);
            core::ptr::read_unaligned(src)
        }
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
            let dst_ptr = core::ptr::with_exposed_provenance_mut(dst_offset as usize);
            demo_renderer::render_rgba8888_raw(
                dst_ptr,
                slice_len,
                width,
                height,
                stride_bytes,
                now_ms,
            )
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
    consumer: UsbHidConsumerControl,
    mouse_buttons: u8,
}

#[cfg(target_arch = "wasm32")]
impl Default for UsbHidBridge {
    fn default() -> Self {
        Self::new()
    }
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

        let mut consumer = UsbHidConsumerControl::new();
        configure(&mut consumer);

        Self {
            keyboard,
            mouse,
            gamepad,
            consumer,
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

    /// Set mouse button state as a bitmask matching the low bits of DOM `MouseEvent.buttons`:
    /// - bit0 (`0x01`): left
    /// - bit1 (`0x02`): right
    /// - bit2 (`0x04`): middle
    /// - bit3 (`0x08`): back / side
    /// - bit4 (`0x10`): forward / extra
    pub fn mouse_buttons(&mut self, buttons: u8) {
        let next = buttons & 0x1f;
        let prev = self.mouse_buttons;
        let delta = prev ^ next;

        for bit in [0x01, 0x02, 0x04, 0x08, 0x10] {
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

    /// Inject a horizontal mouse wheel movement (positive = wheel right / AC Pan).
    pub fn mouse_hwheel(&mut self, delta: i32) {
        self.mouse.hwheel(delta);
    }

    /// Inject vertical + horizontal wheel movement in a single report frame.
    ///
    /// This matches how physical devices may report diagonal scrolling.
    pub fn mouse_wheel2(&mut self, wheel: i32, hwheel: i32) {
        self.mouse.wheel2(wheel, hwheel);
    }

    /// Inject an 8-byte USB HID gamepad report (packed into two 32-bit words).
    ///
    /// The packed format matches `web/src/input/gamepad.ts`:
    /// - `packed_lo`: bytes 0..3 (little-endian)
    /// - `packed_hi`: bytes 4..7 (little-endian)
    ///
    /// The canonical gamepad report layout is defined by `aero_usb::hid::GamepadReport`
    /// (`crates/aero-usb/src/hid/gamepad.rs`) and kept in sync with TypeScript via
    /// `docs/fixtures/hid_gamepad_report_vectors.json`.
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

    /// Inject a single HID Consumer Control usage event (Usage Page 0x0C).
    pub fn consumer_event(&mut self, usage: u16, pressed: bool) {
        self.consumer.consumer_event(usage, pressed);
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
    /// In report protocol this is 5 bytes: buttons, dx, dy, wheel, hwheel (AC Pan).
    pub fn drain_next_mouse_report(&mut self) -> JsValue {
        match self.mouse.handle_in_transfer(0x81, 5) {
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

    /// Drain the next consumer control report (or return `null` if none).
    ///
    /// The report format is 2 bytes: little-endian 16-bit Consumer usage ID (0 = none pressed).
    pub fn drain_next_consumer_report(&mut self) -> JsValue {
        match self.consumer.handle_in_transfer(0x81, 2) {
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

    /// Drain the next pending guest -> host feature report read request (`GET_REPORT`, Feature).
    ///
    /// Returns `null` when no request is pending.
    pub fn drain_next_feature_report_request(&mut self) -> JsValue {
        let Some(req) = self.device.pop_feature_report_request() else {
            return JsValue::NULL;
        };

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("requestId"),
            &JsValue::from_f64(f64::from(req.request_id)),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportId"),
            &JsValue::from_f64(f64::from(req.report_id)),
        );
        obj.into()
    }

    /// Complete a previously-drained feature report read request.
    ///
    /// `data` should contain the report payload bytes without a report-id prefix; the USB HID
    /// device model will insert the report-id prefix when needed.
    pub fn complete_feature_report_request(
        &mut self,
        request_id: u32,
        report_id: u32,
        data: &[u8],
    ) -> Result<bool, JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        Ok(self
            .device
            .complete_feature_report_request(request_id, report_id, data))
    }

    /// Fail a previously-drained feature report read request.
    pub fn fail_feature_report_request(
        &mut self,
        request_id: u32,
        report_id: u32,
        error: Option<String>,
    ) -> Result<bool, JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        // `UsbHidPassthroughHandle` no longer carries an error string for feature report failures,
        // but keep this JS-callable API stable.
        drop(error);
        Ok(self
            .device
            .fail_feature_report_request(request_id, report_id))
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
        let collections = crate::webhid_parse::parse_webhid_collections(&collections)?;

        let report_descriptor = webhid::synthesize_report_descriptor(&collections)
            .map_err(|e| js_error(format!("failed to synthesize HID report descriptor: {e}")))?;

        let (interface_subclass, interface_protocol) = webhid::infer_boot_interface(&collections)
            .map(|(subclass, protocol)| (Some(subclass), Some(protocol)))
            .unwrap_or((None, None));
        let max_output_on_wire = webhid::max_output_report_bytes_on_wire(&collections);
        let has_interrupt_out = max_output_on_wire > 0 && max_output_on_wire <= 64;

        let device = UsbHidPassthroughHandle::new(
            vendor_id,
            product_id,
            manufacturer.unwrap_or_else(|| "WebHID".to_string()),
            product.unwrap_or_else(|| "WebHID HID Device".to_string()),
            serial,
            report_descriptor,
            has_interrupt_out,
            None,
            interface_subclass,
            interface_protocol,
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

    /// Drain the next pending guest -> host feature report read request (`GET_REPORT`, Feature).
    ///
    /// Returns `null` when no request is pending.
    pub fn drain_next_feature_report_request(&mut self) -> JsValue {
        let Some(req) = self.device.pop_feature_report_request() else {
            return JsValue::NULL;
        };

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("requestId"),
            &JsValue::from_f64(f64::from(req.request_id)),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("reportId"),
            &JsValue::from_f64(f64::from(req.report_id)),
        );
        obj.into()
    }

    /// Complete a previously-drained feature report read request.
    ///
    /// `data` should contain the report payload bytes without a report-id prefix; the USB HID
    /// device model will insert the report-id prefix when needed.
    pub fn complete_feature_report_request(
        &mut self,
        request_id: u32,
        report_id: u32,
        data: &[u8],
    ) -> Result<bool, JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        Ok(self
            .device
            .complete_feature_report_request(request_id, report_id, data))
    }

    /// Fail a previously-drained feature report read request.
    pub fn fail_feature_report_request(
        &mut self,
        request_id: u32,
        report_id: u32,
        error: Option<String>,
    ) -> Result<bool, JsValue> {
        let report_id = u8::try_from(report_id)
            .map_err(|_| js_error("reportId is out of range (expected 0..=255)"))?;
        // `UsbHidPassthroughHandle` no longer carries an error string for feature report failures,
        // but keep this JS-callable API stable.
        drop(error);
        Ok(self
            .device
            .fail_feature_report_request(request_id, report_id))
    }

    /// Whether the guest has configured the USB device (SET_CONFIGURATION != 0).
    pub fn configured(&self) -> bool {
        self.device.configured()
    }
}

#[cfg(target_arch = "wasm32")]
impl WebHidPassthroughBridge {
    #[doc(hidden)]
    pub fn as_usb_device(&self) -> UsbHidPassthroughHandle {
        self.device.clone()
    }
}

#[cfg(target_arch = "wasm32")]
impl UsbHidPassthroughBridge {
    #[doc(hidden)]
    pub fn as_usb_device(&self) -> UsbHidPassthroughHandle {
        self.device.clone()
    }
}
#[cfg(all(test, target_arch = "wasm32"))]
mod webhid_passthrough_bridge_tests {
    use super::*;

    use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn make_minimal_item(usage_page: u32, usage: u32) -> webhid::HidReportItem {
        webhid::HidReportItem {
            usage_page,
            usages: vec![usage],
            usage_minimum: 0,
            usage_maximum: 0,
            report_size: 8,
            report_count: 1,
            unit_exponent: 0,
            unit: 0,
            logical_minimum: 0,
            logical_maximum: 255,
            physical_minimum: 0,
            physical_maximum: 0,
            strings: vec![],
            string_minimum: 0,
            string_maximum: 0,
            designators: vec![],
            designator_minimum: 0,
            designator_maximum: 0,
            is_absolute: true,
            is_array: false,
            is_buffered_bytes: false,
            is_constant: false,
            is_linear: true,
            is_range: false,
            is_relative: false,
            is_volatile: false,
            has_null: false,
            has_preferred_state: true,
            is_wrapped: false,
        }
    }

    fn keyboard_collections() -> Vec<webhid::HidCollectionInfo> {
        vec![webhid::HidCollectionInfo {
            usage_page: 0x01, // Generic Desktop
            usage: 0x06,      // Keyboard
            collection_type: webhid::HidCollectionType::Application,
            children: vec![],
            input_reports: vec![webhid::HidReportInfo {
                report_id: 0,
                items: vec![make_minimal_item(0x07, 0x04)], // Keyboard/Keypad: 'a'
            }],
            output_reports: vec![],
            feature_reports: vec![],
        }]
    }

    fn mouse_collections() -> Vec<webhid::HidCollectionInfo> {
        vec![webhid::HidCollectionInfo {
            usage_page: 0x01, // Generic Desktop
            usage: 0x02,      // Mouse
            collection_type: webhid::HidCollectionType::Application,
            children: vec![],
            input_reports: vec![webhid::HidReportInfo {
                report_id: 0,
                items: vec![make_minimal_item(0x09, 0x01)], // Button: Button 1
            }],
            output_reports: vec![],
            feature_reports: vec![],
        }]
    }

    fn large_output_collections() -> Vec<webhid::HidCollectionInfo> {
        let mut item = make_minimal_item(0x01, 0x00); // Generic Desktop / Undefined
        item.report_count = 65;
        vec![webhid::HidCollectionInfo {
            usage_page: 0x01, // Generic Desktop
            usage: 0x00,      // Undefined
            collection_type: webhid::HidCollectionType::Application,
            children: vec![],
            input_reports: vec![webhid::HidReportInfo {
                report_id: 0,
                items: vec![make_minimal_item(0x01, 0x00)],
            }],
            output_reports: vec![webhid::HidReportInfo {
                report_id: 0,
                items: vec![item],
            }],
            feature_reports: vec![],
        }]
    }

    fn parse_interface_descriptor_fields(bytes: &[u8]) -> Option<(u8, u8)> {
        const INTERFACE_DESC_OFFSET: usize = 9;
        if bytes.len() < INTERFACE_DESC_OFFSET + 9 {
            return None;
        }
        // Config descriptor is always followed immediately by a single interface descriptor.
        if bytes[INTERFACE_DESC_OFFSET] != 0x09 || bytes[INTERFACE_DESC_OFFSET + 1] != 0x04 {
            return None;
        }
        let subclass = bytes[INTERFACE_DESC_OFFSET + 6];
        let protocol = bytes[INTERFACE_DESC_OFFSET + 7];
        Some((subclass, protocol))
    }

    fn parse_num_endpoints(bytes: &[u8]) -> Option<u8> {
        const INTERFACE_DESC_OFFSET: usize = 9;
        if bytes.len() < INTERFACE_DESC_OFFSET + 9 {
            return None;
        }
        if bytes[INTERFACE_DESC_OFFSET] != 0x09 || bytes[INTERFACE_DESC_OFFSET + 1] != 0x04 {
            return None;
        }
        Some(bytes[INTERFACE_DESC_OFFSET + 4])
    }

    #[wasm_bindgen_test]
    fn webhid_passthrough_bridge_infers_boot_keyboard_interface_descriptor() {
        let collections_json =
            serde_wasm_bindgen::to_value(&keyboard_collections()).expect("collections to JsValue");
        let bridge = WebHidPassthroughBridge::new(
            0x1234,
            0x5678,
            Some("WebHID".to_string()),
            Some("Test Keyboard".to_string()),
            None,
            collections_json,
        )
        .expect("WebHidPassthroughBridge::new ok");

        let mut dev = bridge.as_usb_device();
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: 0x06,
                w_value: 0x0200,
                w_index: 0,
                w_length: 256,
            },
            None,
        );
        let ControlResponse::Data(bytes) = resp else {
            panic!("expected config descriptor bytes, got {resp:?}");
        };

        assert_eq!(
            parse_interface_descriptor_fields(&bytes),
            Some((0x01, 0x01))
        );
    }

    #[wasm_bindgen_test]
    fn webhid_passthrough_bridge_infers_boot_mouse_interface_descriptor() {
        let collections_json =
            serde_wasm_bindgen::to_value(&mouse_collections()).expect("collections to JsValue");
        let bridge = WebHidPassthroughBridge::new(
            0x1234,
            0x5678,
            Some("WebHID".to_string()),
            Some("Test Mouse".to_string()),
            None,
            collections_json,
        )
        .expect("WebHidPassthroughBridge::new ok");

        let mut dev = bridge.as_usb_device();
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: 0x06,
                w_value: 0x0200,
                w_index: 0,
                w_length: 256,
            },
            None,
        );
        let ControlResponse::Data(bytes) = resp else {
            panic!("expected config descriptor bytes, got {resp:?}");
        };

        assert_eq!(
            parse_interface_descriptor_fields(&bytes),
            Some((0x01, 0x02))
        );
    }

    #[wasm_bindgen_test]
    fn webhid_passthrough_bridge_omits_interrupt_out_for_large_output_reports() {
        let collections_json = serde_wasm_bindgen::to_value(&large_output_collections())
            .expect("collections to JsValue");
        let bridge = WebHidPassthroughBridge::new(
            0x1234,
            0x5678,
            Some("WebHID".to_string()),
            Some("Large Output Device".to_string()),
            None,
            collections_json,
        )
        .expect("WebHidPassthroughBridge::new ok");

        let mut dev = bridge.as_usb_device();
        let resp = dev.handle_control_request(
            SetupPacket {
                bm_request_type: 0x80,
                b_request: 0x06,
                w_value: 0x0200,
                w_index: 0,
                w_length: 256,
            },
            None,
        );
        let ControlResponse::Data(bytes) = resp else {
            panic!("expected config descriptor bytes, got {resp:?}");
        };

        assert_eq!(parse_num_endpoints(&bytes), Some(1));
        assert!(
            !bytes.windows(3).any(|w| w == [0x07, 0x05, 0x01]),
            "expected config descriptor to omit interrupt OUT endpoint 0x01: {bytes:02x?}"
        );
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
    let mut bridge = MicBridge::from_shared_buffer(sab)?;
    // The mic ring buffer is written continuously by the AudioWorklet. If the consumer attaches
    // after the producer has already started, discard any buffered samples so callers observe low
    // latency rather than replaying stale audio.
    bridge.discard_buffered_samples();
    bridge.reset_dropped_samples_baseline();
    Ok(bridge)
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
impl Default for UsbPassthroughBridge {
    fn default() -> Self {
        Self::new()
    }
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
        let actions = self
            .inner
            .drain_actions_limit(crate::webusb_ports::MAX_WEBUSB_HOST_ACTIONS_PER_DRAIN);
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
        self.device
            .drain_actions_limit(crate::webusb_ports::MAX_WEBUSB_HOST_ACTIONS_PER_DRAIN)
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
impl Default for UsbPassthroughDemo {
    fn default() -> Self {
        Self::new()
    }
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

#[cfg(target_arch = "wasm32")]
impl Default for SineTone {
    fn default() -> Self {
        Self::new()
    }
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
        if frames == 0 || !sample_rate.is_finite() || sample_rate <= 0.0 {
            return 0;
        }

        let channel_count = bridge.channel_count();
        if channel_count == 0 {
            return 0;
        }

        let capacity = bridge.capacity_frames();
        let level = bridge.buffer_level_frames();
        let free_frames = capacity.saturating_sub(level);
        if free_frames == 0 {
            return 0;
        }

        // Clamp per-call work: this is a JS-callable demo API and callers may be untrusted. Avoid
        // allocating a multi-gigabyte scratch buffer if a hostile caller passes `frames=u32::MAX`.
        let frames = frames.min(free_frames);

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
        // Defensive clamp: this is a JS-callable API and callers may be untrusted.
        let dst_sample_rate_hz = dst_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
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
        let dst_sample_rate_hz = dst_sample_rate_hz.min(aero_audio::MAX_HOST_SAMPLE_RATE_HZ);
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

        let capacity = bridge.capacity_frames();
        let level = bridge.buffer_level_frames();
        let mut free_frames = capacity.saturating_sub(level);
        if free_frames == 0 {
            return Ok(0);
        }
        // Bound per-call work/allocations. This API is JS-callable; avoid decoding/resampling
        // multi-second buffers in one go if a caller passes a huge ring capacity or `frames` value.
        free_frames = free_frames.min(self.dst_sample_rate_hz);
        if free_frames == 0 {
            return Ok(0);
        }

        // Only decode as many source frames as needed to fill the available ring space. Dropping
        // excess producer input avoids unbounded buffering if the AudioWorklet consumer stalls.
        let required_src = self.resampler.required_source_frames(free_frames as usize);
        let queued_src = self.resampler.queued_source_frames();
        let need_src = required_src.saturating_sub(queued_src);
        if need_src > 0 && !pcm_bytes.is_empty() {
            let bytes_per_frame = fmt.bytes_per_frame();
            let max_bytes = need_src.saturating_mul(bytes_per_frame);
            let take = pcm_bytes.len().min(max_bytes);
            decode_pcm_to_stereo_f32_into(&pcm_bytes[..take], fmt, &mut self.decode_scratch);
            if !self.decode_scratch.is_empty() {
                self.resampler.push_source_frames(&self.decode_scratch);
            }
        }

        self.resampler
            .produce_interleaved_stereo_into(free_frames as usize, &mut self.resample_out_scratch);
        Ok(bridge.write_f32_interleaved(&self.resample_out_scratch))
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
struct HdaGuestMemory {
    /// Byte offset inside the wasm linear memory where guest physical address 0 begins.
    ///
    /// In the real worker runtime this is the `guest_base` from the `guest_ram_layout` contract.
    /// In the `HdaPlaybackDemo` harness we point this at a private heap allocation.
    guest_base: u32,
    /// Guest RAM size in bytes.
    guest_size: u64,
}

#[cfg(target_arch = "wasm32")]
impl MemoryAccess for HdaGuestMemory {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        let mut paddr = addr;
        let mut off = 0usize;

        while off < buf.len() {
            let remaining = buf.len() - off;
            let chunk =
                crate::guest_phys::translate_guest_paddr_chunk(self.guest_size, paddr, remaining);
            let chunk_len = match chunk {
                crate::guest_phys::GuestRamChunk::Ram { ram_offset, len } => {
                    let Some(linear) = (self.guest_base as u64)
                        .checked_add(ram_offset)
                        .and_then(|v| u32::try_from(v).ok())
                    else {
                        buf[off..].fill(0);
                        return;
                    };

                    // Shared-memory (threaded wasm) builds: use atomic byte loads to avoid Rust
                    // data-race UB when the guest RAM lives in a shared `WebAssembly.Memory`.
                    #[cfg(feature = "wasm-threaded")]
                    {
                        use core::sync::atomic::{AtomicU8, Ordering};
                        let src: *const AtomicU8 =
                            core::ptr::with_exposed_provenance(linear as usize);
                        for (i, slot) in buf[off..off + len].iter_mut().enumerate() {
                            // Safety: `translate_guest_paddr_chunk` bounds-checks against the
                            // configured guest RAM size and `AtomicU8` has alignment 1.
                            *slot = unsafe { (&*src.add(i)).load(Ordering::Relaxed) };
                        }
                    }

                    // Non-threaded wasm builds: linear memory is not shared across threads, so memcpy
                    // is fine.
                    #[cfg(not(feature = "wasm-threaded"))]
                    unsafe {
                        // Safety: `translate_guest_paddr_chunk` bounds-checks against the
                        // configured guest RAM size and `linear` is a wasm32-compatible linear
                        // address.
                        let src: *const u8 = core::ptr::with_exposed_provenance(linear as usize);
                        core::ptr::copy_nonoverlapping(src, buf[off..].as_mut_ptr(), len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    buf[off..off + len].fill(0xFF);
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    buf[off..off + len].fill(0);
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }
            off += chunk_len;
            paddr = match paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => {
                    buf[off..].fill(0);
                    return;
                }
            };
        }
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let mut paddr = addr;
        let mut off = 0usize;

        while off < buf.len() {
            let remaining = buf.len() - off;
            let chunk =
                crate::guest_phys::translate_guest_paddr_chunk(self.guest_size, paddr, remaining);
            let chunk_len = match chunk {
                crate::guest_phys::GuestRamChunk::Ram { ram_offset, len } => {
                    let Some(linear) = (self.guest_base as u64)
                        .checked_add(ram_offset)
                        .and_then(|v| u32::try_from(v).ok())
                    else {
                        return;
                    };

                    // Shared-memory (threaded wasm) builds: use atomic byte stores to avoid Rust
                    // data-race UB when the guest RAM lives in a shared `WebAssembly.Memory`.
                    #[cfg(feature = "wasm-threaded")]
                    {
                        use core::sync::atomic::{AtomicU8, Ordering};
                        let dst: *const AtomicU8 =
                            core::ptr::with_exposed_provenance(linear as usize);
                        for (i, byte) in buf[off..off + len].iter().copied().enumerate() {
                            // Safety: `translate_guest_paddr_chunk` bounds-checks against the
                            // configured guest RAM size and `AtomicU8` has alignment 1.
                            unsafe { (&*dst.add(i)).store(byte, Ordering::Relaxed) };
                        }
                    }

                    // Non-threaded wasm builds: linear memory is not shared across threads, so memcpy
                    // is fine.
                    #[cfg(not(feature = "wasm-threaded"))]
                    unsafe {
                        // Safety: `translate_guest_paddr_chunk` bounds-checks against the
                        // configured guest RAM size and `linear` is a wasm32-compatible linear
                        // address.
                        let dst: *mut u8 = core::ptr::with_exposed_provenance_mut(linear as usize);
                        core::ptr::copy_nonoverlapping(buf[off..].as_ptr(), dst, len);
                    }
                    len
                }
                crate::guest_phys::GuestRamChunk::Hole { len } => {
                    // Open bus: writes are ignored.
                    len
                }
                crate::guest_phys::GuestRamChunk::OutOfBounds { len } => {
                    // Preserve existing semantics: ignore out-of-range writes.
                    len
                }
            };

            if chunk_len == 0 {
                break;
            }
            off += chunk_len;
            paddr = match paddr.checked_add(chunk_len as u64) {
                Some(v) => v,
                None => return,
            };
        }
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
    // Keep the backing allocation alive so `mem`'s raw `guest_base` pointer remains valid.
    #[allow(dead_code)]
    guest: Vec<u8>,
    mem: HdaGuestMemory,
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

        // Defensive clamp: this is a JS-callable demo API, so avoid allocating multi-gigabyte
        // buffers if a caller passes an absurd sample rate.
        let host_sample_rate = host_sample_rate.clamp(1, aero_audio::MAX_HOST_SAMPLE_RATE_HZ);

        let bridge = WorkletBridge::from_shared_buffer(ring_sab, capacity_frames, channel_count)?;

        let hda = HdaController::new_with_output_rate(host_sample_rate);

        // Allocate a small guest-physical memory backing store. The demo programs
        // a short BDL + PCM buffer and loops it forever.
        let mut guest = vec![0u8; 0x20_000];
        let mem = HdaGuestMemory {
            guest_base: guest.as_mut_ptr() as u32,
            guest_size: guest.len() as u64,
        };

        // Default to a 440Hz tone so the demo works even if callers don't invoke
        // `init_sine_dma()` explicitly.
        let mut demo = Self {
            hda,
            guest,
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
        self.mem.write_u64(bdl_base, pcm_base);
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
#[cfg(all(test, target_arch = "wasm32"))]
mod hda_dma_oob_tests {
    use super::*;

    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn hda_process_completes_on_oob_dma_pointer() {
        // Small synthetic guest RAM region for the DMA engine.
        let mut guest = vec![0u8; 0x4000];
        let guest_size = guest.len() as u64;
        let mut mem = HdaGuestMemory {
            guest_base: guest.as_mut_ptr() as u32,
            guest_size,
        };

        let mut hda = HdaController::new();

        // Bring controller out of reset.
        hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

        // Configure the codec converter to listen on stream 1, channel 0.
        // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
        let set_stream_ch = (0x706u32 << 8) | 0x10;
        hda.codec_mut().execute_verb(2, set_stream_ch);

        // Stream format: 48kHz, 16-bit, 2ch.
        let fmt_raw: u16 = (1 << 4) | 0x1;
        // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
        let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
        hda.codec_mut().execute_verb(2, set_fmt);

        // Guest buffer layout: BDL is in-bounds, but it points at an out-of-bounds PCM address.
        let bdl_base = 0x1000u64;
        let pcm_len_bytes = 512u32; // 128 frames @ 16-bit stereo
        let oob_pcm_base = guest_size + 0x1000;

        // One BDL entry pointing at an invalid buffer address.
        mem.write_u64(bdl_base, oob_pcm_base);
        mem.write_u32(bdl_base + 8, pcm_len_bytes);
        mem.write_u32(bdl_base + 12, 1); // IOC=1

        // Configure stream descriptor 0.
        {
            let sd = hda.stream_mut(0);
            sd.bdpl = bdl_base as u32;
            sd.bdpu = 0;
            sd.cbl = pcm_len_bytes;
            sd.lvi = 0;
            sd.fmt = fmt_raw;
            // SRST | RUN | IOCE | stream number 1.
            sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
        }

        // The call should complete without panicking even though the DMA address is invalid.
        hda.process(&mut mem, 128);
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

        pub fn debugcon_output(&mut self) -> Vec<u8> {
            self.inner.debugcon_output_bytes()
        }

        pub fn debugcon_output_len(&mut self) -> u32 {
            self.inner.debugcon_output_len().min(u64::from(u32::MAX)) as u32
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
            let mut file = OpfsSyncFile::create(&path).await.map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.snapshot_full_to_opfs", &path, e)
            })?;

            self.inner.save_snapshot_full_to(&mut file).map_err(|e| {
                crate::opfs_snapshot_error_to_js("DemoVm.snapshot_full_to_opfs", &path, e)
            })?;

            file.close().map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.snapshot_full_to_opfs", &path, e)
            })?;
            Ok(())
        }

        #[cfg(target_arch = "wasm32")]
        pub async fn snapshot_dirty_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
            let mut file = OpfsSyncFile::create(&path).await.map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.snapshot_dirty_to_opfs", &path, e)
            })?;

            self.inner.save_snapshot_dirty_to(&mut file).map_err(|e| {
                crate::opfs_snapshot_error_to_js("DemoVm.snapshot_dirty_to_opfs", &path, e)
            })?;

            file.close().map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.snapshot_dirty_to_opfs", &path, e)
            })?;
            Ok(())
        }

        #[cfg(target_arch = "wasm32")]
        pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
            let mut file = OpfsSyncFile::open(&path, false).await.map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.restore_snapshot_from_opfs", &path, e)
            })?;

            self.inner
                .restore_snapshot_from_checked(&mut file)
                .map_err(|e| {
                    crate::opfs_snapshot_error_to_js("DemoVm.restore_snapshot_from_opfs", &path, e)
                })?;

            file.close().map_err(|e| {
                crate::opfs_io_error_to_js("DemoVm.restore_snapshot_from_opfs", &path, e)
            })?;
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
    framebuffer: SharedFramebuffer,
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
            if guest_counter_offset_bytes == 0 || !guest_counter_offset_bytes.is_multiple_of(4) {
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

            if framebuffer_offset_bytes == 0 || !framebuffer_offset_bytes.is_multiple_of(4) {
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
                let framebuffer_ptr: *mut u8 =
                    core::ptr::with_exposed_provenance_mut(framebuffer_offset_bytes as usize);
                SharedFramebuffer::from_raw_parts(framebuffer_ptr, layout)
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
                let counter_ptr: *mut AtomicU32 =
                    core::ptr::with_exposed_provenance_mut(guest_counter_offset_bytes as usize);
                (*counter_ptr).store(0, Ordering::SeqCst);
            }

            Ok(Self {
                guest_counter_offset_bytes,
                framebuffer: shared,
            })
        }

        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            let _ = ram_size_bytes;
            Err(JsValue::from_str(
                "CpuWorkerDemo requires the threaded WASM build (enable the `wasm-threaded` feature; requires shared memory + atomics).",
            ))
        }
    }

    /// Increment a shared guest-memory counter (Atomics-visible from JS).
    ///
    /// Returns the incremented value.
    pub fn tick(&self, _now_ms: f64) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        unsafe {
            let counter_ptr: *const AtomicU32 =
                core::ptr::with_exposed_provenance(self.guest_counter_offset_bytes as usize);
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
            let header = self.framebuffer.header();
            // `frame_dirty` is a producer->consumer "new frame available" flag. The JS side clears
            // it after presentation; treat it as an acknowledgement that the last published frame
            // is no longer in use.
            //
            // This throttles the demo producer to at most one outstanding frame, avoiding
            // overwriting a buffer that the JS presenter might still be reading.
            if header.frame_dirty.load(Ordering::SeqCst) != 0 {
                return header.frame_seq.load(Ordering::SeqCst);
            }

            let writer = SharedFramebufferWriter::new(self.framebuffer);
            writer.write_frame(|buf, dirty, layout| {
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

/// DOM-style mouse button IDs (mirrors `MouseEvent.button`).
///
/// This enum exists for JS ergonomics; the canonical machine wrapper also provides explicit helpers
/// (`Machine::inject_mouse_left/right/middle`) and accepts raw numeric values.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    Back = 3,
    Forward = 4,
}

/// Mouse buttons bit values matching `MouseEvent.buttons`.
///
/// This is a "bitflag-like" enum: values can be OR'd together to build a mask.
///
/// - bit0 (`0x01`): left
/// - bit1 (`0x02`): right
/// - bit2 (`0x04`): middle
/// - bit3 (`0x08`): back
/// - bit4 (`0x10`): forward
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButtons {
    Left = 0x01,
    Right = 0x02,
    Middle = 0x04,
    Back = 0x08,
    Forward = 0x10,
}

#[cfg(any(target_arch = "wasm32", test))]
const AEROSPARSE_HEADER_SIZE_BYTES: usize = 64;

#[cfg(any(target_arch = "wasm32", test))]
fn open_or_create_cow_disk<Base, OverlayBackend>(
    base: Base,
    mut overlay_backend: OverlayBackend,
    overlay_block_size_bytes: u32,
) -> aero_storage::Result<aero_storage::AeroCowDisk<Base, OverlayBackend>>
where
    Base: aero_storage::VirtualDisk,
    OverlayBackend: aero_storage::StorageBackend + aero_storage::VirtualDiskSend,
{
    let base_size = base.capacity_bytes();
    if base_size == 0 {
        return Err(aero_storage::DiskError::Io(
            "base disk capacity is 0 bytes".to_string(),
        ));
    }
    if !base_size.is_multiple_of(aero_storage::SECTOR_SIZE as u64) {
        return Err(aero_storage::DiskError::Io(format!(
            "base disk size {base_size} is not a multiple of {} bytes",
            aero_storage::SECTOR_SIZE
        )));
    }

    let overlay_len = overlay_backend.len()?;
    if overlay_len == 0 {
        if overlay_block_size_bytes == 0 {
            return Err(aero_storage::DiskError::Io(
                "overlay_block_size_bytes must be non-zero when creating a new overlay".to_string(),
            ));
        }
        return aero_storage::AeroCowDisk::create(base, overlay_backend, overlay_block_size_bytes);
    }

    if overlay_len < AEROSPARSE_HEADER_SIZE_BYTES as u64 {
        return Err(aero_storage::DiskError::Io(format!(
            "overlay image is too small ({overlay_len} bytes) to contain an aerosparse header ({AEROSPARSE_HEADER_SIZE_BYTES} bytes)"
        )));
    }

    let mut header_bytes = [0u8; AEROSPARSE_HEADER_SIZE_BYTES];
    overlay_backend.read_at(0, &mut header_bytes)?;
    let header = aero_storage::AeroSparseHeader::decode(&header_bytes)?;

    if header.disk_size_bytes != base_size {
        return Err(aero_storage::DiskError::Io(format!(
            "overlay disk_size_bytes ({}) does not match base disk size ({base_size})",
            header.disk_size_bytes
        )));
    }
    if overlay_block_size_bytes != 0 && header.block_size_bytes != overlay_block_size_bytes {
        return Err(aero_storage::DiskError::Io(format!(
            "overlay block_size_bytes ({}) does not match expected block size ({overlay_block_size_bytes})",
            header.block_size_bytes
        )));
    }

    aero_storage::AeroCowDisk::open(base, overlay_backend)
}

#[cfg(test)]
mod cow_base_format_tests {
    use super::open_or_create_cow_disk;

    use aero_storage::StorageBackend;
    use aero_storage::VirtualDisk;
    use aero_storage::{AeroSparseConfig, AeroSparseDisk, DiskFormat, DiskImage, MemBackend};

    fn sample_pattern(len: usize) -> Vec<u8> {
        // Deterministic non-trivial data so a failure to consult the base disk is obvious.
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(7))
            .collect()
    }

    #[test]
    fn cow_reads_from_raw_base_when_overlay_blocks_unallocated() {
        const BASE_SIZE: u64 = 8192;
        const OVERLAY_BLOCK_SIZE: u32 = 4096;
        let pattern = sample_pattern(6000);

        // Base: raw bytes.
        let mut base_backend = MemBackend::with_len(BASE_SIZE).expect("MemBackend::with_len");
        base_backend
            .write_at(0, &pattern)
            .expect("write pattern to raw base backend");

        let base = DiskImage::open_auto(base_backend).expect("DiskImage::open_auto(raw)");
        assert_eq!(base.format(), DiskFormat::Raw);

        // Overlay: empty -> created by `open_or_create_cow_disk`.
        let overlay_backend = MemBackend::new();
        let mut cow =
            open_or_create_cow_disk(base, overlay_backend, OVERLAY_BLOCK_SIZE).expect("open COW");

        // Sanity: overlay should start with no allocated data blocks.
        assert!(!cow.overlay().is_block_allocated(0));
        assert!(!cow.overlay().is_block_allocated(1));

        let mut buf = vec![0u8; pattern.len()];
        cow.read_at(0, &mut buf)
            .expect("read from COW falls back to base");
        assert_eq!(buf, pattern);
    }

    #[test]
    fn cow_reads_from_aerosparse_base_when_overlay_blocks_unallocated() {
        const BASE_SIZE: u64 = 8192;
        const BASE_BLOCK_SIZE: u32 = 4096;
        const OVERLAY_BLOCK_SIZE: u32 = 4096;
        let pattern = sample_pattern(6000);

        // Base: aerosparse image stored in a MemBackend.
        let base_backend = MemBackend::new();
        let mut base_sparse = AeroSparseDisk::create(
            base_backend,
            AeroSparseConfig {
                disk_size_bytes: BASE_SIZE,
                block_size_bytes: BASE_BLOCK_SIZE,
            },
        )
        .expect("create aerosparse base");

        base_sparse
            .write_at(0, &pattern)
            .expect("write pattern to aerosparse base");
        base_sparse.flush().expect("flush aerosparse base");

        // Re-open via format detection to match the wasm OPFS code path.
        let base_backend = base_sparse.into_backend();
        let base = DiskImage::open_auto(base_backend).expect("DiskImage::open_auto(aerosparse)");
        assert_eq!(base.format(), DiskFormat::AeroSparse);

        let overlay_backend = MemBackend::new();
        let mut cow =
            open_or_create_cow_disk(base, overlay_backend, OVERLAY_BLOCK_SIZE).expect("open COW");

        assert!(!cow.overlay().is_block_allocated(0));
        assert!(!cow.overlay().is_block_allocated(1));

        let mut buf = vec![0u8; pattern.len()];
        cow.read_at(0, &mut buf)
            .expect("read from COW falls back to base");
        assert_eq!(buf, pattern);
    }
}

/// Canonical machine BIOS boot device selection.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineBootDevice {
    /// Boot from the primary HDD (AHCI port 0 / BIOS disk).
    Hdd = 0,
    /// Boot from the install media CD-ROM (IDE secondary master ATAPI).
    Cdrom = 1,
}

/// Canonical full-system Aero VM exported to JS via wasm-bindgen.
///
/// This wrapper is backed by `aero_machine::Machine` and is the intended target for new browser
/// integration work (device wiring, networking via `NET_TX`/`NET_RX` rings, snapshots, …).
///
/// `new Machine(ramSize)` constructs a "full PC" configuration suitable for booting/installing
/// Windows 7 in the browser:
///
/// - Canonical PC platform topology (PIC/APIC/PIT/RTC/PCI/ACPI/HPET)
/// - Canonical Win7 storage topology (ICH9 AHCI + PIIX3 IDE) as defined in
///   `docs/05-storage-topology-win7.md`
/// - E1000 NIC + UHCI (USB 1.1) + AeroGPU (default; VGA disabled; legacy VGA ranges are aliased
///   through VRAM)
///
/// To opt into the legacy VGA/VBE device model instead of AeroGPU, use
/// [`Machine::new_with_config`] with `enable_aerogpu=false` (VGA defaults to enabled in that case).
///
/// To request more than one vCPU (SMP), use [`Machine::new_with_cpu_count`] or pass `cpu_count` to
/// [`Machine::new_with_config`].
///
/// Storage attachment points are identified by stable `disk_id` values exposed via
/// [`Machine::disk_id_primary_hdd`], [`Machine::disk_id_install_media`], and
/// [`Machine::disk_id_ide_primary_master`].
///
/// The worker CPU runtime currently uses the legacy `WasmVm` / `WasmTieredVm` exports instead: they
/// execute only the CPU core in WASM and forward port I/O / MMIO back to JS via shims.
#[wasm_bindgen]
pub struct Machine {
    inner: aero_machine::Machine,
    // Tracks the last injected mouse button state (low 5 bits = DOM `MouseEvent.buttons`).
    //
    // This exists solely to support the ergonomic JS-side `inject_mouse_buttons_mask` API without
    // emitting redundant button transition packets.
    mouse_buttons: u8,
    mouse_buttons_known: bool,

    /// Shared scanout descriptor used by the browser presentation pipeline to select the active
    /// scanout source (legacy VGA text, legacy VBE LFB, or WDDM/AeroGPU).
    ///
    /// In the threaded WASM build this lives inside the shared wasm linear memory so both:
    /// - WASM device models (this VM), and
    /// - JS workers (GPU presenter / frame scheduler)
    /// can read/write it using atomics without additional SharedArrayBuffer allocations.
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    scanout_state: &'static ScanoutState,

    /// Last legacy scanout state this VM published (used to avoid bumping generation every slice).
    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    last_published_scanout: Option<ScanoutStateUpdate>,
}

#[cfg(target_arch = "wasm32")]
fn opfs_cross_origin_isolated() -> Option<bool> {
    Reflect::get(&js_sys::global(), &JsValue::from_str("crossOriginIsolated"))
        .ok()
        .and_then(|v| v.as_bool())
}

#[cfg(target_arch = "wasm32")]
fn opfs_is_secure_context() -> Option<bool> {
    Reflect::get(&js_sys::global(), &JsValue::from_str("isSecureContext"))
        .ok()
        .and_then(|v| v.as_bool())
}

#[cfg(target_arch = "wasm32")]
fn opfs_worker_hint() -> String {
    let mut hint =
        "Hint: Run the wasm module in a DedicatedWorker (not the main thread).".to_string();
    // Many Aero browser flows require the threaded WASM build, which in turn requires COOP/COEP
    // (cross-origin isolation) to unlock `SharedArrayBuffer`. Mention this only when we can
    // observe that the current context is *not* cross-origin isolated.
    if matches!(opfs_cross_origin_isolated(), Some(false)) {
        hint.push_str(" If you are using the threaded build, also ensure the page is cross-origin isolated (COOP/COEP) so SharedArrayBuffer is available.");
    }
    if matches!(opfs_is_secure_context(), Some(false)) {
        hint.push_str(" OPFS requires a secure context (HTTPS or localhost).");
    }
    hint
}

#[cfg(target_arch = "wasm32")]
fn opfs_disk_error_to_js(operation: &str, path: &str, err: aero_opfs::DiskError) -> JsValue {
    use aero_opfs::DiskError;

    let err_str = err.to_string();

    match err {
        DiskError::InUse => {
            let extra = "The OPFS file is already in use (another context may have an open sync access handle). Close other tabs/workers or ensure the previous handle is closed before retrying.";
            Error::new(&format!(
                "{operation} failed for OPFS path \"{path}\": {err_str}\n{extra}"
            ))
            .into()
        }
        DiskError::QuotaExceeded => {
            let extra = "Storage quota exceeded. Free up space or adjust browser/site storage permissions, then retry.";
            Error::new(&format!(
                "{operation} failed for OPFS path \"{path}\": {err_str}\n{extra}"
            ))
            .into()
        }
        DiskError::NotSupported(msg) => {
            let extra = if msg.contains("sync access handle") || msg.contains("sync access handles")
            {
                "OPFS sync access handles (FileSystemSyncAccessHandle) are worker-only and are typically only available in Chromium-based browsers."
            } else if msg.contains("navigator.storage.getDirectory") {
                "OPFS is unavailable in this environment (navigator.storage.getDirectory missing)."
            } else {
                "OPFS backend is not supported in this environment."
            };

            let worker_hint = opfs_worker_hint();
            let probe_tip = "Tip: Call storage_capabilities() to probe { opfsSupported, opfsSyncAccessSupported, isWorkerScope, crossOriginIsolated, sharedArrayBufferSupported, isSecureContext }.";
            Error::new(&format!(
                "{operation} failed for OPFS path \"{path}\": {err_str}\n{extra}\n{worker_hint}\n{probe_tip}"
            ))
            .into()
        }
        DiskError::BackendUnavailable => {
            let extra =
                "Storage APIs may be blocked by browser/privacy settings or iframe restrictions.";
            let worker_hint = opfs_worker_hint();
            let probe_tip = "Tip: Call storage_capabilities() to probe { opfsSupported, opfsSyncAccessSupported, isWorkerScope, crossOriginIsolated, sharedArrayBufferSupported, isSecureContext }.";
            Error::new(&format!(
                "{operation} failed for OPFS path \"{path}\": {err_str}\n{extra}\n{worker_hint}\n{probe_tip}"
            ))
            .into()
        }
        _ => Error::new(&format!(
            "{operation} failed for OPFS path \"{path}\": {err_str}"
        ))
        .into(),
    }
}

#[cfg(target_arch = "wasm32")]
fn opfs_io_error_to_js(operation: &str, path: &str, err: std::io::Error) -> JsValue {
    let err_str = err.to_string();
    let kind = err.kind();
    let mut msg = format!("{operation} failed for OPFS path \"{path}\": {err_str}");

    match kind {
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::NotConnected => {
            let worker_hint = opfs_worker_hint();
            let probe_tip = "Tip: Call storage_capabilities() to probe { opfsSupported, opfsSyncAccessSupported, isWorkerScope, crossOriginIsolated, sharedArrayBufferSupported, isSecureContext }.";
            msg.push('\n');
            msg.push_str(&worker_hint);
            msg.push('\n');
            msg.push_str(probe_tip);
        }
        std::io::ErrorKind::ResourceBusy => {
            msg.push_str("\nThe OPFS file is already in use (another context may have an open sync access handle). Close it and retry.");
        }
        std::io::ErrorKind::StorageFull => {
            msg.push_str(
                "\nStorage quota exceeded. Free up space or adjust browser/site storage permissions, then retry.",
            );
        }
        _ => {}
    }

    Error::new(&msg).into()
}
// Native-only helpers for integration tests.
//
// These are intentionally *not* part of the wasm-bindgen export surface.
#[cfg(not(target_arch = "wasm32"))]
impl Machine {
    pub fn debug_inner(&self) -> &aero_machine::Machine {
        &self.inner
    }

    pub fn debug_inner_mut(&mut self) -> &mut aero_machine::Machine {
        &mut self.inner
    }
}

#[cfg(target_arch = "wasm32")]
fn opfs_context_error_to_js(operation: &str, path: &str, err: impl core::fmt::Display) -> JsValue {
    Error::new(&format!(
        "{operation} failed for OPFS path \"{path}\": {err}"
    ))
    .into()
}

#[cfg(target_arch = "wasm32")]
fn opfs_snapshot_error_to_js(
    operation: &str,
    path: &str,
    err: aero_snapshot::SnapshotError,
) -> JsValue {
    match err {
        aero_snapshot::SnapshotError::Io(e) => opfs_io_error_to_js(operation, path, e),
        other => Error::new(&format!(
            "{operation} failed for OPFS path \"{path}\": {other}"
        ))
        .into(),
    }
}

#[wasm_bindgen]
impl Machine {
    fn new_with_native_config(cfg: aero_machine::MachineConfig) -> Result<Self, JsValue> {
        #[allow(unused_mut)]
        let mut inner =
            aero_machine::Machine::new(cfg).map_err(|e| JsValue::from_str(&e.to_string()))?;

        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        let scanout_state = {
            let scanout_state = Self::scanout_state_ref();
            let cursor_state = Self::cursor_state_ref();
            // Allow the underlying `aero-machine` integration layer (BIOS INT10, AeroGPU scanout0,
            // etc.) to publish scanout transitions into the same shared descriptor that JS workers
            // read.
            inner.set_scanout_state_static(Some(scanout_state));
            inner.set_cursor_state_static(Some(cursor_state));
            scanout_state
        };
        Ok(Self {
            inner,
            mouse_buttons: 0,
            mouse_buttons_known: true,

            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            scanout_state,
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            last_published_scanout: None,
        })
    }

    fn validate_cpu_count(cpu_count: u32) -> Result<u8, JsValue> {
        if !(1..=u32::from(u8::MAX)).contains(&cpu_count) {
            let msg = format!(
                "invalid cpu_count {cpu_count} (must be between 1 and {})",
                u8::MAX
            );
            #[cfg(target_arch = "wasm32")]
            {
                return Err(JsValue::from_str(&msg));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // `wasm-bindgen` does not implement string constructors for `JsValue` on non-wasm
                // targets (they panic). Host-side unit tests still exercise error paths, so return
                // a sentinel `JsValue` instead.
                let _ = msg;
                return Err(JsValue::NULL);
            }
        }
        Ok(cpu_count as u8)
    }

    /// Create a new canonical full-system Aero machine.
    ///
    /// # vCPU count / SMP
    /// This JS constructor uses the canonical browser defaults, which currently configure
    /// `cpu_count=1`.
    ///
    /// To request a different CPU count, use [`Machine::new_with_cpu_count`] (or
    /// [`Machine::new_with_config`]).
    ///
    /// Note: SMP is still **bring-up only** (not a robust multi-vCPU environment yet). For real
    /// guest boots, prefer `cpu_count=1`.
    ///
    /// See `docs/21-smp.md#status-today` and `docs/09-bios-firmware.md#smp-boot-bsp--aps`.
    #[wasm_bindgen(constructor)]
    pub fn new(ram_size_bytes: u32) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_synthetic_usb_hid = true;
        Self::new_with_native_config(cfg)
    }

    /// Create a new canonical full-system machine with a custom SMBIOS System UUID seed.
    ///
    /// This is a convenience for runtimes that need a stable per-VM identity (notably Windows
    /// guests). The seed is forwarded to firmware and used to derive the SMBIOS Type 1 UUID.
    #[wasm_bindgen(js_name = newWithSmbiosUuidSeed)]
    pub fn new_with_smbios_uuid_seed(
        ram_size_bytes: u32,
        smbios_uuid_seed: u64,
    ) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_synthetic_usb_hid = true;
        cfg.smbios_uuid_seed = smbios_uuid_seed;
        Self::new_with_native_config(cfg)
    }

    /// Construct a canonical machine with an explicit vCPU count.
    ///
    /// This is a constructor-like alternative to `new(ram_size_bytes)` that lets JS opt into SMP
    /// by configuring `cpu_count` for firmware topology publication (SMBIOS + ACPI).
    ///
    /// Note: `aero_machine::Machine` supports basic SMP bring-up (AP wait-for-SIPI + INIT/SIPI via
    /// LAPIC ICR + bounded cooperative AP execution), but SMP is still **bring-up only** (not a
    /// robust multi-vCPU environment yet). For real guest boots, prefer `cpu_count=1`.
    ///
    /// See `docs/21-smp.md#status-today` and `docs/09-bios-firmware.md#smp-boot-bsp--aps`.
    #[wasm_bindgen]
    pub fn new_with_cpu_count(ram_size_bytes: u32, cpu_count: u32) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_synthetic_usb_hid = true;
        cfg.cpu_count = Self::validate_cpu_count(cpu_count)?;
        Self::new_with_native_config(cfg)
    }

    /// Construct a machine with explicit graphics configuration.
    ///
    /// `enable_aerogpu` is forwarded to [`aero_machine::MachineConfig::enable_aerogpu`].
    ///
    /// When `enable_aerogpu` is `true`, VGA is disabled by default.
    ///
    /// Note: `enable_aerogpu` and `enable_vga` are mutually exclusive in the native machine
    /// configuration; passing `enable_aerogpu=true` and `enable_vga=true` will fail construction.
    #[wasm_bindgen]
    pub fn new_with_config(
        ram_size_bytes: u32,
        enable_aerogpu: bool,
        enable_vga: Option<bool>,
        cpu_count: Option<u32>,
    ) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_synthetic_usb_hid = true;
        if let Some(cpu_count) = cpu_count {
            cfg.cpu_count = Self::validate_cpu_count(cpu_count)?;
        }
        cfg.enable_aerogpu = enable_aerogpu;
        cfg.enable_vga = enable_vga.unwrap_or(!enable_aerogpu);
        Self::new_with_native_config(cfg)
    }

    /// Set the SMBIOS System UUID seed used by firmware.
    ///
    /// This only takes effect after the next [`Machine::reset`].
    pub fn set_smbios_uuid_seed(&mut self, seed: u64) {
        self.inner.set_smbios_uuid_seed(seed);
    }

    /// Number of vCPUs configured for this machine.
    pub fn cpu_count(&self) -> u32 {
        self.inner.config().cpu_count as u32
    }

    /// Construct a machine with an options object that can override the default device set.
    ///
    /// This preserves the existing `new(ram_size_bytes)` behavior when `options` is omitted.
    ///
    /// Example:
    ///
    /// ```ts
    /// const machine = api.Machine.new_with_options(64 * 1024 * 1024, {
    ///   enable_virtio_input: true,
    ///   enable_i8042: false,
    /// });
    /// ```
    #[cfg(target_arch = "wasm32")]
    pub fn new_with_options(
        ram_size_bytes: u32,
        options: Option<JsValue>,
    ) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_synthetic_usb_hid = true;

        if let Some(options) = options
            && !options.is_null()
            && !options.is_undefined()
        {
            if !options.is_object() {
                return Err(JsValue::from_str("Machine options must be an object"));
            }

            let get_bool = |key: &str| -> Result<Option<bool>, JsValue> {
                let v = Reflect::get(&options, &JsValue::from_str(key))?;
                if v.is_undefined() || v.is_null() {
                    return Ok(None);
                }
                v.as_bool()
                    .ok_or_else(|| {
                        JsValue::from_str(&format!("Machine option `{key}` must be a boolean"))
                    })
                    .map(Some)
            };

            if let Some(v) = get_bool("enable_pc_platform")? {
                cfg.enable_pc_platform = v;
            }
            if let Some(v) = get_bool("enable_acpi")? {
                cfg.enable_acpi = v;
            }
            if let Some(v) = get_bool("enable_e1000")? {
                cfg.enable_e1000 = v;
            }
            if let Some(v) = get_bool("enable_virtio_net")? {
                cfg.enable_virtio_net = v;
            }
            if let Some(v) = get_bool("enable_virtio_blk")? {
                cfg.enable_virtio_blk = v;
            }
            if let Some(v) = get_bool("enable_virtio_input")? {
                cfg.enable_virtio_input = v;
            }
            if let Some(v) = get_bool("enable_ahci")? {
                cfg.enable_ahci = v;
            }
            if let Some(v) = get_bool("enable_nvme")? {
                cfg.enable_nvme = v;
            }
            if let Some(v) = get_bool("enable_ide")? {
                cfg.enable_ide = v;
            }
            if let Some(v) = get_bool("enable_uhci")? {
                cfg.enable_uhci = v;
            }
            if let Some(v) = get_bool("enable_ehci")? {
                cfg.enable_ehci = v;
            }
            if let Some(v) = get_bool("enable_xhci")? {
                cfg.enable_xhci = v;
            }
            let mut enable_vga_set = false;
            let mut enable_aerogpu_set = false;
            if let Some(v) = get_bool("enable_synthetic_usb_hid")? {
                cfg.enable_synthetic_usb_hid = v;
            }
            if let Some(v) = get_bool("enable_vga")? {
                cfg.enable_vga = v;
                enable_vga_set = true;
            }
            if let Some(v) = get_bool("enable_aerogpu")? {
                cfg.enable_aerogpu = v;
                enable_aerogpu_set = true;
            }
            // Mirror `new_with_config` defaults for the mutually-exclusive VGA/AeroGPU device
            // selection when callers only specify one side:
            // - If callers explicitly set `enable_aerogpu` without specifying VGA, VGA defaults
            //   to `!enable_aerogpu`.
            // - If callers enable VGA without explicitly specifying `enable_aerogpu`, disable
            //   AeroGPU to avoid a configuration error.
            if enable_aerogpu_set && !enable_vga_set {
                cfg.enable_vga = !cfg.enable_aerogpu;
            } else if enable_vga_set && cfg.enable_vga && !enable_aerogpu_set {
                cfg.enable_aerogpu = false;
            }
            if let Some(v) = get_bool("enable_serial")? {
                cfg.enable_serial = v;
            }
            if let Some(v) = get_bool("enable_i8042")? {
                cfg.enable_i8042 = v;
            }
            if let Some(v) = get_bool("enable_a20_gate")? {
                cfg.enable_a20_gate = v;
            }
            if let Some(v) = get_bool("enable_reset_ctrl")? {
                cfg.enable_reset_ctrl = v;
            }
        }

        // Synthetic HID devices are always attached behind UHCI.
        if cfg.enable_synthetic_usb_hid {
            cfg.enable_uhci = true;
        }

        Self::new_with_native_config(cfg)
    }

    /// Construct a canonical machine and optionally enable additional input backends.
    ///
    /// - PS/2 (i8042) remains enabled so early boot always works.
    /// - virtio-input is a paravirtualized fast path (requires a guest driver).
    /// - synthetic USB HID devices are attached behind UHCI via an external hub.
    pub fn new_with_input_backends(
        ram_size_bytes: u32,
        enable_virtio_input: bool,
        enable_synthetic_usb_hid: bool,
    ) -> Result<Self, JsValue> {
        let mut cfg = aero_machine::MachineConfig::browser_defaults(ram_size_bytes as u64);
        cfg.enable_virtio_input = enable_virtio_input;
        cfg.enable_synthetic_usb_hid = enable_synthetic_usb_hid;
        if enable_synthetic_usb_hid {
            cfg.enable_uhci = true;
        }
        Self::new_with_native_config(cfg)
    }

    /// Construct a machine whose guest RAM is backed by the wasm linear memory.
    ///
    /// This is intended for the threaded/shared-memory wasm build, where the Rust heap is capped
    /// to the runtime-reserved region (`runtime_alloc.rs`). Supplying a guest RAM region in linear
    /// memory avoids heap-allocating a `Vec<u8>`/`DenseMemory` of `guest_size` bytes.
    ///
    /// The backing storage is the linear-memory byte range `[guest_base, guest_base + guest_size)`.
    #[cfg(target_arch = "wasm32")]
    pub fn new_shared(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let guest_size_u64 =
            crate::validate_shared_guest_ram_layout("Machine.new_shared", guest_base, guest_size)?;
        if guest_size_u64 == 0 {
            return Err(js_error("Machine.new_shared: guest_size must be non-zero"));
        }
        let mut cfg = aero_machine::MachineConfig::browser_defaults(guest_size_u64);
        cfg.enable_synthetic_usb_hid = true;

        let mem = memory::WasmSharedGuestMemory::new(guest_base, guest_size_u64).map_err(|e| {
            js_error(format!(
                "Machine.new_shared: failed to init shared guest RAM backend: {e}"
            ))
        })?;
        #[allow(unused_mut)]
        let mut inner = aero_machine::Machine::new_with_guest_memory(cfg, Box::new(mem))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        let scanout_state = {
            let scanout_state = Self::scanout_state_ref();
            let cursor_state = Self::cursor_state_ref();
            inner.set_scanout_state_static(Some(scanout_state));
            inner.set_cursor_state_static(Some(cursor_state));
            scanout_state
        };
        Ok(Self {
            inner,
            mouse_buttons: 0,
            mouse_buttons_known: true,

            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            scanout_state,
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            last_published_scanout: None,
        })
    }

    /// Construct a shared-guest-memory machine with explicit graphics configuration.
    ///
    /// This is the shared-memory equivalent of [`Machine::new_with_config`]. It is intended for
    /// browser runtimes that need to back guest RAM with the wasm linear memory (`new_shared`) but
    /// still want to force-enable VGA or force-disable AeroGPU for debugging/compatibility.
    ///
    /// `enable_aerogpu` is forwarded to [`aero_machine::MachineConfig::enable_aerogpu`]. When
    /// `enable_aerogpu` is `true`, VGA is disabled by default (`enable_vga` defaults to
    /// `!enable_aerogpu`).
    ///
    /// Note: `enable_aerogpu` and `enable_vga` are mutually exclusive in the native machine
    /// configuration; passing `enable_aerogpu=true` and `enable_vga=true` will fail construction.
    #[cfg(target_arch = "wasm32")]
    pub fn new_shared_with_config(
        guest_base: u32,
        guest_size: u32,
        enable_aerogpu: bool,
        enable_vga: Option<bool>,
        cpu_count: Option<u32>,
    ) -> Result<Self, JsValue> {
        let guest_size_u64 = crate::validate_shared_guest_ram_layout(
            "Machine.new_shared_with_config",
            guest_base,
            guest_size,
        )?;
        if guest_size_u64 == 0 {
            return Err(js_error(
                "Machine.new_shared_with_config: guest_size must be non-zero",
            ));
        }

        let mut cfg = aero_machine::MachineConfig::browser_defaults(guest_size_u64);
        cfg.enable_synthetic_usb_hid = true;
        if let Some(cpu_count) = cpu_count {
            cfg.cpu_count = Self::validate_cpu_count(cpu_count)?;
        }
        cfg.enable_aerogpu = enable_aerogpu;
        cfg.enable_vga = enable_vga.unwrap_or(!enable_aerogpu);

        let mem = memory::WasmSharedGuestMemory::new(guest_base, guest_size_u64).map_err(|e| {
            js_error(format!(
                "Machine.new_shared_with_config: failed to init shared guest RAM backend: {e}"
            ))
        })?;
        #[allow(unused_mut)]
        let mut inner = aero_machine::Machine::new_with_guest_memory(cfg, Box::new(mem))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        let scanout_state = {
            let scanout_state = Self::scanout_state_ref();
            let cursor_state = Self::cursor_state_ref();
            inner.set_scanout_state_static(Some(scanout_state));
            inner.set_cursor_state_static(Some(cursor_state));
            scanout_state
        };
        Ok(Self {
            inner,
            mouse_buttons: 0,
            mouse_buttons_known: true,

            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            scanout_state,
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            last_published_scanout: None,
        })
    }

    /// Construct a canonical Win7 storage topology machine backed by shared guest RAM.
    ///
    /// This is the shared-memory equivalent of [`Machine::new_win7_storage`].
    ///
    /// The backing storage is the linear-memory byte range `[guest_base, guest_base + guest_size)`.
    #[cfg(target_arch = "wasm32")]
    pub fn new_win7_storage_shared(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_size == 0 {
            return Err(js_error(
                "Machine.new_win7_storage_shared: guest_size must be non-zero",
            ));
        }

        let guest_size_u64 = crate::validate_shared_guest_ram_layout(
            "Machine.new_win7_storage_shared",
            guest_base,
            guest_size,
        )?;

        let cfg = aero_machine::MachineConfig::win7_storage_defaults(guest_size_u64);

        let mem = memory::WasmSharedGuestMemory::new(guest_base, guest_size_u64).map_err(|e| {
            js_error(format!(
                "Machine.new_win7_storage_shared: failed to init shared guest RAM backend: {e}"
            ))
        })?;
        #[allow(unused_mut)]
        let mut inner = aero_machine::Machine::new_with_guest_memory(cfg, Box::new(mem))
            .map_err(|e| JsValue::from_str(&e.to_string()))?;

        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        let scanout_state = {
            let scanout_state = Self::scanout_state_ref();
            let cursor_state = Self::cursor_state_ref();
            inner.set_scanout_state_static(Some(scanout_state));
            inner.set_cursor_state_static(Some(cursor_state));
            scanout_state
        };
        Ok(Self {
            inner,
            mouse_buttons: 0,
            mouse_buttons_known: true,

            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            scanout_state,
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
            last_published_scanout: None,
        })
    }

    /// Construct a canonical Windows 7 storage topology machine (AHCI + IDE at the normative BDFs).
    ///
    /// This uses [`aero_machine::MachineConfig::win7_storage_defaults`], which matches
    /// `docs/05-storage-topology-win7.md` and keeps non-storage devices conservative by default:
    ///
    /// - AHCI (ICH9) enabled at `00:02.0`
    /// - IDE (PIIX3) enabled at `00:01.1` (with the multi-function ISA bridge at `00:01.0`)
    /// - **Networking is disabled** in this preset (E1000/virtio-net off)
    /// - USB is disabled (UHCI off)
    pub fn new_win7_storage(ram_size_bytes: u32) -> Result<Self, JsValue> {
        let cfg = aero_machine::MachineConfig::win7_storage_defaults(ram_size_bytes as u64);
        Self::new_with_native_config(cfg)
    }

    pub fn reset(&mut self) {
        self.inner.reset();
        self.mouse_buttons = 0;
        self.mouse_buttons_known = true;

        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            // Reset returns the canonical VM to legacy VGA text mode. Publish this so the browser
            // presenter can switch scanout sources without heuristics.
            self.last_published_scanout = None;
            self.publish_legacy_text_scanout();
        }
    }

    /// Set the preferred BIOS boot device for the next reset.
    pub fn set_boot_device(&mut self, device: MachineBootDevice) {
        let native = match device {
            MachineBootDevice::Hdd => aero_machine::BootDevice::Hdd,
            MachineBootDevice::Cdrom => aero_machine::BootDevice::Cdrom,
        };
        self.inner.set_boot_device(native);
    }

    /// Returns the configured boot device preference.
    pub fn boot_device(&self) -> MachineBootDevice {
        match self.inner.boot_device() {
            aero_machine::BootDevice::Hdd => MachineBootDevice::Hdd,
            aero_machine::BootDevice::Cdrom => MachineBootDevice::Cdrom,
        }
    }

    /// Returns the effective boot device used for the current boot session.
    ///
    /// When the firmware "CD-first when present" policy is enabled, this reflects what firmware
    /// actually booted from (CD vs HDD), rather than just the configured preference.
    pub fn active_boot_device(&self) -> MachineBootDevice {
        match self.inner.active_boot_device() {
            aero_machine::BootDevice::Hdd => MachineBootDevice::Hdd,
            aero_machine::BootDevice::Cdrom => MachineBootDevice::Cdrom,
        }
    }

    /// Returns the configured BIOS boot drive number (`DL`) used for firmware POST/boot.
    ///
    /// Recommended values:
    /// - `0x80`: primary HDD (normal boot)
    /// - `0xE0`: ATAPI CD-ROM (El Torito install media)
    pub fn boot_drive(&self) -> u32 {
        u32::from(self.inner.boot_drive())
    }

    /// Returns whether the firmware "CD-first when present" boot policy is enabled.
    pub fn boot_from_cd_if_present(&self) -> bool {
        self.inner.boot_from_cd_if_present()
    }

    /// Returns the BIOS drive number used for CD-ROM boot when the "CD-first when present" policy
    /// is enabled.
    pub fn cd_boot_drive(&self) -> u32 {
        u32::from(self.inner.cd_boot_drive())
    }

    /// Enable/disable the firmware "CD-first when present" boot policy.
    ///
    /// When enabled and install media is attached, BIOS POST attempts to boot from CD-ROM first
    /// and falls back to the configured `boot_drive` on failure.
    ///
    /// Call [`Machine::reset`] to apply the new policy to the next boot.
    pub fn set_boot_from_cd_if_present(&mut self, enabled: bool) {
        self.inner.set_boot_from_cd_if_present(enabled);
    }

    /// Set the BIOS CD-ROM drive number used when booting under the "CD-first when present" policy.
    ///
    /// Valid El Torito CD-ROM drive numbers are `0xE0..=0xEF`.
    ///
    /// Call [`Machine::reset`] to apply the new value to the next boot.
    pub fn set_cd_boot_drive(&mut self, drive: u32) -> Result<(), JsValue> {
        if !(0xE0..=0xEF).contains(&drive) {
            return Err(JsValue::from_str(
                "cd boot drive must be in 0xE0..=0xEF (recommended 0xE0 for first CD-ROM)",
            ));
        }
        self.inner.set_cd_boot_drive(drive as u8);
        Ok(())
    }

    pub fn set_disk_image(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.inner
            .set_disk_image(bytes.to_vec())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Set the BIOS boot drive number (`DL`) used when transferring control to the boot sector.
    ///
    /// Recommended values:
    /// - `0x80`: primary HDD (normal boot)
    /// - `0xE0`: ATAPI CD-ROM (El Torito install media)
    ///
    /// Note: this selection is consumed during BIOS POST/boot. Call [`Machine::reset`] after
    /// changing it to re-run POST with the new `DL` value.
    pub fn set_boot_drive(&mut self, drive: u32) -> Result<(), JsValue> {
        if drive == 0 {
            return Err(JsValue::from_str(
                "boot drive must be non-zero (recommended 0x80 for HDD or 0xE0 for CD-ROM)",
            ));
        }
        // Clamp to the BIOS/INT13 drive number range.
        let drive = drive.min(u32::from(u8::MAX)) as u8;
        self.inner.set_boot_drive(drive);
        Ok(())
    }

    /// Attach an ISO image (raw bytes) as the machine's canonical install media / ATAPI CD-ROM
    /// (`disk_id=1`).
    ///
    /// This API only attaches the host-side backing store. Snapshot overlay refs for `disk_id=1`
    /// are configured separately via `set_ide_secondary_master_atapi_overlay_ref`.
    pub fn attach_install_media_iso_bytes(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.is_empty() {
            return Err(JsValue::from_str("ISO image must be non-empty"));
        }
        self.inner
            .attach_install_media_iso_bytes(bytes.to_vec())
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Open an existing OPFS-backed ISO image (using the file's current size) and attach it as the
    /// machine's canonical install media / ATAPI CD-ROM (`disk_id=1`).
    ///
    /// This is the preferred way to attach large ISOs without copying them into WASM memory.
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs_existing(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let disk = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.attach_install_media_iso_opfs_existing", &path, e)
            })?;
        self.inner
            .attach_ide_secondary_master_iso(Box::new(disk))
            .map_err(|e| {
                opfs_context_error_to_js("Machine.attach_install_media_iso_opfs_existing", &path, e)
            })
    }

    /// Open an existing OPFS-backed ISO image (using the file's current size), attach it as the
    /// machine's canonical install media / ATAPI CD-ROM (`disk_id=1`), and set the snapshot overlay
    /// reference (`DISKS` entry).
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::attach_install_media_iso_opfs_existing`]
    /// so callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs_existing_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_install_media_iso_opfs_existing(path).await?;
        self.set_ide_secondary_master_atapi_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Back-compat alias: attach an in-memory ISO as the canonical install media CD-ROM.
    ///
    /// Prefer [`Machine::attach_install_media_iso_bytes`], which more clearly describes the
    /// canonical machine's install-media slot (`disk_id=1`).
    pub fn set_cd_image(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.attach_install_media_iso_bytes(bytes)
    }

    /// Open (or create) an OPFS-backed disk image and attach it as the machine's canonical disk.
    ///
    /// This enables large disks without fully loading the image into RAM.
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        base_format: Option<String>,
    ) -> Result<(), JsValue> {
        let format = base_format.as_deref().unwrap_or("raw");

        // Aerosparse images have an on-disk header + allocation table, so they must be opened via
        // `aero_storage::AeroSparseDisk`. Treating the file as a raw sector stream would expose
        // the sparse header bytes to the guest.
        if format.eq_ignore_ascii_case("aerospar") || format.eq_ignore_ascii_case("aerosparse") {
            let backend = aero_opfs::OpfsByteStorage::open(&path, create)
                .await
                .map_err(|e| opfs_disk_error_to_js("Machine.set_disk_opfs", &path, e))?;

            let disk = if create {
                // Use a conservative default allocation unit size (1 MiB) suitable for general use
                // and aligned to sector boundaries.
                let cfg = aero_storage::AeroSparseConfig {
                    disk_size_bytes: size_bytes,
                    block_size_bytes: 1024 * 1024,
                };
                aero_storage::AeroSparseDisk::create(backend, cfg)
                    .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs", &path, e))?
            } else {
                let disk = aero_storage::AeroSparseDisk::open(backend)
                    .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs", &path, e))?;
                if size_bytes != 0 && disk.header().disk_size_bytes != size_bytes {
                    return Err(js_error(format!(
                        "Machine.set_disk_opfs failed for OPFS path \"{path}\": aerosparse disk size mismatch (header={} expected={size_bytes})",
                        disk.header().disk_size_bytes
                    )));
                }
                disk
            };

            return self
                .inner
                .set_disk_backend(Box::new(disk))
                .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs", &path, e));
        }

        // Default: raw disk image (flat sector file).
        let backend = aero_opfs::OpfsBackend::open(&path, create, size_bytes)
            .await
            .map_err(|e| opfs_disk_error_to_js("Machine.set_disk_opfs", &path, e))?;
        self.inner
            .set_disk_backend(Box::new(backend))
            .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs", &path, e))
    }

    /// Open (or create) an OPFS-backed disk image, attach it as the machine's canonical disk, and
    /// set the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// When using OPFS-based disks, recording the overlay ref is important so that snapshot/restore
    /// flows can re-open and re-attach the disk based solely on the snapshot contents.
    ///
    /// This helper sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// (i.e. a single-file raw/aerospar image; no separate overlay file).
    ///
    /// This method is intentionally separate from [`Machine::set_disk_opfs`] so callers do not
    /// silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs_and_set_overlay_ref(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        base_format: Option<String>,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.set_disk_opfs(path, create, size_bytes, base_format)
            .await?;
        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open (or create) an OPFS-backed disk image and attach it as the machine's canonical disk,
    /// reporting create/resize progress via a JS callback.
    ///
    /// The callback is invoked with a numeric progress value in `[0.0, 1.0]`.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs_with_progress(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        progress: js_sys::Function,
    ) -> Result<(), JsValue> {
        let backend =
            aero_opfs::OpfsBackend::open_with_progress(&path, create, size_bytes, Some(&progress))
                .await
                .map_err(|e| {
                    opfs_disk_error_to_js("Machine.set_disk_opfs_with_progress", &path, e)
                })?;
        self.inner
            .set_disk_backend(Box::new(backend))
            .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs_with_progress", &path, e))
    }

    /// Open (or create) an OPFS-backed disk image, attach it as the machine's canonical disk,
    /// report create/resize progress via a JS callback, and set the snapshot overlay reference
    /// (`DISKS` entry) for `disk_id=0`.
    ///
    /// This is equivalent to calling:
    /// - [`Machine::set_disk_opfs_with_progress`], then
    /// - [`Machine::set_ahci_port0_disk_overlay_ref`] with `overlay_image=""`.
    ///
    /// This method is intentionally separate from [`Machine::set_disk_opfs_with_progress`] so
    /// callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs_with_progress_and_set_overlay_ref(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        progress: js_sys::Function,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.set_disk_opfs_with_progress(path, create, size_bytes, progress)
            .await?;
        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open an existing OPFS-backed disk image (using the file's current size) and attach it as the
    /// machine's canonical disk.
    ///
    /// This avoids requiring the caller to know the disk size ahead of time.
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs_existing(
        &mut self,
        path: String,
        base_format: Option<String>,
        expected_size_bytes: Option<u64>,
    ) -> Result<(), JsValue> {
        let format = base_format.as_deref().unwrap_or("raw");
        let disk = crate::opfs_virtual_disk::open_opfs_virtual_disk(
            "Machine.set_disk_opfs_existing",
            &path,
            format,
        )
        .await?;

        if let Some(expected) = expected_size_bytes {
            let actual = disk.capacity_bytes();
            if expected != 0 && actual != expected {
                return Err(js_error(format!(
                    "Machine.set_disk_opfs_existing failed for OPFS path \"{path}\": disk size mismatch (opened={actual} expected={expected})"
                )));
            }
        }

        self.inner
            .set_disk_backend(disk)
            .map_err(|e| opfs_context_error_to_js("Machine.set_disk_opfs_existing", &path, e))
    }

    /// Open an existing OPFS-backed ISO image and attach it as the IDE secondary channel master
    /// ATAPI CD-ROM (install media, `disk_id=1`).
    ///
    /// Notes:
    /// - ISO images must have a capacity that is a multiple of 2048 bytes.
    /// - OPFS sync access handles are worker-only; this requires running in a Dedicated Worker.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_secondary_master_iso_opfs_existing(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let disk = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js(
                    "Machine.attach_ide_secondary_master_iso_opfs_existing",
                    &path,
                    e,
                )
            })?;
        self.inner
            .attach_ide_secondary_master_iso(Box::new(disk))
            .map_err(|e| {
                opfs_context_error_to_js(
                    "Machine.attach_ide_secondary_master_iso_opfs_existing",
                    &path,
                    e,
                )
            })
    }

    /// Open an existing OPFS-backed ISO image, attach it as the IDE secondary channel master ATAPI
    /// CD-ROM (install media), and set the snapshot overlay reference (`DISKS` entry) for
    /// `disk_id=1`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from
    /// [`Machine::attach_ide_secondary_master_iso_opfs_existing`] so callers do not silently
    /// overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_secondary_master_iso_opfs_existing_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_ide_secondary_master_iso_opfs_existing(path)
            .await?;
        self.set_ide_secondary_master_atapi_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open (or create) an OPFS-backed disk image and attach it as the IDE primary channel master
    /// ATA disk.
    ///
    /// This corresponds to the canonical Windows 7 storage topology slot:
    /// Intel PIIX3 IDE primary channel, master drive (`disk_id=2`).
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
    ) -> Result<(), JsValue> {
        let backend = aero_opfs::OpfsBackend::open(&path, create, size_bytes)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.attach_ide_primary_master_disk_opfs", &path, e)
            })?;
        self.inner
            .attach_ide_primary_master_disk(Box::new(backend))
            .map_err(|e| {
                opfs_context_error_to_js("Machine.attach_ide_primary_master_disk_opfs", &path, e)
            })
    }

    /// Open (or create) an OPFS-backed disk image and attach it as the IDE primary channel master
    /// ATA disk, reporting create/resize progress via a JS callback.
    ///
    /// The callback is invoked with a numeric progress value in `[0.0, 1.0]`.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs_with_progress(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        progress: js_sys::Function,
    ) -> Result<(), JsValue> {
        let backend =
            aero_opfs::OpfsBackend::open_with_progress(&path, create, size_bytes, Some(&progress))
                .await
                .map_err(|e| {
                    opfs_disk_error_to_js(
                        "Machine.attach_ide_primary_master_disk_opfs_with_progress",
                        &path,
                        e,
                    )
                })?;
        self.inner
            .attach_ide_primary_master_disk(Box::new(backend))
            .map_err(|e| {
                opfs_context_error_to_js(
                    "Machine.attach_ide_primary_master_disk_opfs_with_progress",
                    &path,
                    e,
                )
            })
    }

    /// Open (or create) an OPFS-backed disk image, attach it as the IDE primary channel master ATA
    /// disk, and set the snapshot overlay reference (`DISKS` entry) for `disk_id=2`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::attach_ide_primary_master_disk_opfs`]
    /// so callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs_and_set_overlay_ref(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_ide_primary_master_disk_opfs(path, create, size_bytes)
            .await?;
        self.set_ide_primary_master_ata_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open (or create) an OPFS-backed disk image, attach it as the IDE primary channel master ATA
    /// disk, report create/resize progress via a JS callback, and set the snapshot overlay
    /// reference (`DISKS` entry) for `disk_id=2`.
    ///
    /// This method is intentionally separate from
    /// [`Machine::attach_ide_primary_master_disk_opfs_with_progress`] so callers do not silently
    /// overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs_with_progress_and_set_overlay_ref(
        &mut self,
        path: String,
        create: bool,
        size_bytes: u64,
        progress: js_sys::Function,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_ide_primary_master_disk_opfs_with_progress(path, create, size_bytes, progress)
            .await?;
        self.set_ide_primary_master_ata_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open an existing OPFS-backed disk image (using the file's current size) and attach it as
    /// the IDE primary channel master ATA disk.
    ///
    /// This corresponds to the canonical Windows 7 storage topology slot:
    /// Intel PIIX3 IDE primary channel, master drive (`disk_id=2`).
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs_existing(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let backend = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js(
                    "Machine.attach_ide_primary_master_disk_opfs_existing",
                    &path,
                    e,
                )
            })?;
        self.inner
            .attach_ide_primary_master_disk(Box::new(backend))
            .map_err(|e| {
                opfs_context_error_to_js(
                    "Machine.attach_ide_primary_master_disk_opfs_existing",
                    &path,
                    e,
                )
            })
    }

    /// Open an existing OPFS-backed disk image, attach it as the IDE primary channel master ATA
    /// disk, and set the snapshot overlay reference (`DISKS` entry) for `disk_id=2`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from
    /// [`Machine::attach_ide_primary_master_disk_opfs_existing`] so callers do not silently
    /// overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_ide_primary_master_disk_opfs_existing_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_ide_primary_master_disk_opfs_existing(path)
            .await?;
        self.set_ide_primary_master_ata_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open an existing OPFS-backed disk image, attach it as the machine's canonical disk, and set
    /// the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::set_disk_opfs_existing`] so callers do
    /// not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_opfs_existing_and_set_overlay_ref(
        &mut self,
        path: String,
        base_format: Option<String>,
        expected_size_bytes: Option<u64>,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.set_disk_opfs_existing(path, base_format, expected_size_bytes)
            .await?;
        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Create a new OPFS-backed Aero sparse disk (`.aerospar`) and attach it as the machine's
    /// canonical disk.
    ///
    /// This is the preferred persistence format for large VM disks in the browser: it stores only
    /// allocated blocks in OPFS while presenting a fixed-capacity virtual disk to the guest.
    ///
    /// Notes:
    /// - OPFS sync access handles are worker-only; this requires running in a Dedicated Worker.
    /// - `block_size_bytes` must be a power-of-two multiple of 512.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_aerospar_opfs_create(
        &mut self,
        path: String,
        disk_size_bytes: u64,
        block_size_bytes: u32,
    ) -> Result<(), JsValue> {
        let storage = aero_opfs::OpfsByteStorage::open(&path, true)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.set_disk_aerospar_opfs_create", &path, e)
            })?;
        let disk = aero_storage::AeroSparseDisk::create(
            storage,
            aero_storage::AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes,
            },
        )
        .map_err(|e| {
            js_error(format!(
                "Machine.set_disk_aerospar_opfs_create failed for OPFS path \"{path}\": {e}"
            ))
        })?;
        self.inner.set_disk_backend(Box::new(disk)).map_err(|e| {
            opfs_context_error_to_js("Machine.set_disk_aerospar_opfs_create", &path, e)
        })
    }

    /// Create a new OPFS-backed Aero sparse disk (`.aerospar`), attach it as the machine's
    /// canonical disk, and set the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::set_disk_aerospar_opfs_create`] so
    /// callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_aerospar_opfs_create_and_set_overlay_ref(
        &mut self,
        path: String,
        disk_size_bytes: u64,
        block_size_bytes: u32,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.set_disk_aerospar_opfs_create(path, disk_size_bytes, block_size_bytes)
            .await?;
        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Open an existing OPFS-backed Aero sparse disk (`.aerospar`) and attach it as the machine's
    /// canonical disk.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_aerospar_opfs_open(&mut self, path: String) -> Result<(), JsValue> {
        let storage = aero_opfs::OpfsByteStorage::open(&path, false)
            .await
            .map_err(|e| opfs_disk_error_to_js("Machine.set_disk_aerospar_opfs_open", &path, e))?;
        let disk = aero_storage::AeroSparseDisk::open(storage).map_err(|e| {
            js_error(format!(
                "Machine.set_disk_aerospar_opfs_open failed for OPFS path \"{path}\": {e}"
            ))
        })?;
        self.inner
            .set_disk_backend(Box::new(disk))
            .map_err(|e| opfs_context_error_to_js("Machine.set_disk_aerospar_opfs_open", &path, e))
    }

    /// Open an existing OPFS-backed Aero sparse disk (`.aerospar`), attach it as the machine's
    /// canonical disk, and set the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::set_disk_aerospar_opfs_open`] so
    /// callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_aerospar_opfs_open_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.set_disk_aerospar_opfs_open(path).await?;
        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Create an OPFS-backed copy-on-write disk: `base_path` (read-only, any supported format) +
    /// `overlay_path` (writable Aero sparse disk).
    ///
    /// The overlay is created with the same capacity as the base disk and attached as the
    /// machine's canonical disk.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_cow_opfs_create(
        &mut self,
        base_path: String,
        overlay_path: String,
        overlay_block_size_bytes: u32,
    ) -> Result<(), JsValue> {
        let base_storage = aero_opfs::OpfsByteStorage::open(&base_path, false)
            .await
            .map_err(|e| {
                let paths = format!("base={base_path}, overlay={overlay_path}");
                opfs_disk_error_to_js("Machine.set_disk_cow_opfs_create(base)", &paths, e)
            })?;
        let base_disk = aero_storage::DiskImage::open_auto(base_storage).map_err(|e| {
            js_error(format!(
                "Machine.set_disk_cow_opfs_create failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
            ))
        })?;

        let overlay_backend = aero_opfs::OpfsByteStorage::open(&overlay_path, true)
            .await
            .map_err(|e| {
                let paths = format!("base={base_path}, overlay={overlay_path}");
                opfs_disk_error_to_js("Machine.set_disk_cow_opfs_create(overlay)", &paths, e)
            })?;

        let disk = aero_storage::AeroCowDisk::create(
            base_disk,
            overlay_backend,
            overlay_block_size_bytes,
        )
        .map_err(|e| {
            js_error(format!(
                "Machine.set_disk_cow_opfs_create failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
            ))
        })?;

        self.inner
            .set_disk_backend(Box::new(disk))
            .map_err(|e| {
                js_error(format!(
                    "Machine.set_disk_cow_opfs_create failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
                ))
            })
    }

    /// Create an OPFS-backed copy-on-write disk, attach it as the machine's canonical disk, and
    /// set the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// This sets:
    /// - `base_image = base_path`
    /// - `overlay_image = overlay_path`
    ///
    /// This method is intentionally separate from [`Machine::set_disk_cow_opfs_create`] so callers
    /// do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_cow_opfs_create_and_set_overlay_ref(
        &mut self,
        base_path: String,
        overlay_path: String,
        overlay_block_size_bytes: u32,
    ) -> Result<(), JsValue> {
        let base_ref = base_path.clone();
        let overlay_ref = overlay_path.clone();
        self.set_disk_cow_opfs_create(base_path, overlay_path, overlay_block_size_bytes)
            .await?;
        self.set_ahci_port0_disk_overlay_ref(&base_ref, &overlay_ref);
        Ok(())
    }

    /// Open an existing OPFS-backed copy-on-write disk: `base_path` (read-only) + `overlay_path`
    /// (existing writable Aero sparse disk overlay).
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_cow_opfs_open(
        &mut self,
        base_path: String,
        overlay_path: String,
    ) -> Result<(), JsValue> {
        let base_storage = aero_opfs::OpfsByteStorage::open(&base_path, false)
            .await
            .map_err(|e| {
                let paths = format!("base={base_path}, overlay={overlay_path}");
                opfs_disk_error_to_js("Machine.set_disk_cow_opfs_open(base)", &paths, e)
            })?;
        let base_disk = aero_storage::DiskImage::open_auto(base_storage).map_err(|e| {
            js_error(format!(
                "Machine.set_disk_cow_opfs_open failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
            ))
        })?;

        let overlay_backend = aero_opfs::OpfsByteStorage::open(&overlay_path, false)
            .await
            .map_err(|e| {
                let paths = format!("base={base_path}, overlay={overlay_path}");
                opfs_disk_error_to_js("Machine.set_disk_cow_opfs_open(overlay)", &paths, e)
            })?;

        let disk = aero_storage::AeroCowDisk::open(base_disk, overlay_backend).map_err(|e| {
            js_error(format!(
                "Machine.set_disk_cow_opfs_open failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
            ))
        })?;

        self.inner
            .set_disk_backend(Box::new(disk))
            .map_err(|e| {
                js_error(format!(
                    "Machine.set_disk_cow_opfs_open failed for base=\"{base_path}\" overlay=\"{overlay_path}\": {e}"
                ))
            })
    }

    /// Open an OPFS-backed copy-on-write disk, attach it as the machine's canonical disk, and set
    /// the snapshot overlay reference (`DISKS` entry) for `disk_id=0`.
    ///
    /// This sets:
    /// - `base_image = base_path`
    /// - `overlay_image = overlay_path`
    ///
    /// This method is intentionally separate from [`Machine::set_disk_cow_opfs_open`] so callers do
    /// not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_disk_cow_opfs_open_and_set_overlay_ref(
        &mut self,
        base_path: String,
        overlay_path: String,
    ) -> Result<(), JsValue> {
        let base_ref = base_path.clone();
        let overlay_ref = overlay_path.clone();
        self.set_disk_cow_opfs_open(base_path, overlay_path).await?;
        self.set_ahci_port0_disk_overlay_ref(&base_ref, &overlay_ref);
        Ok(())
    }

    /// Attach an existing OPFS-backed ISO image as the canonical install media CD-ROM.
    ///
    /// This attaches the image to the canonical Windows 7 attachment point:
    /// IDE PIIX3 secondary channel master ATAPI (`disk_id=1`).
    ///
    /// Note: this requires OPFS `FileSystemSyncAccessHandle` support (worker-only).
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let disk = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.attach_install_media_iso_opfs", &path, e)
            })?;

        self.inner
            .attach_ide_secondary_master_iso(Box::new(disk))
            .map_err(|e| {
                opfs_context_error_to_js("Machine.attach_install_media_iso_opfs", &path, e)
            })
    }

    /// Attach an existing OPFS-backed ISO image as the canonical install media CD-ROM and set the
    /// snapshot overlay reference (`DISKS` entry) for `disk_id=1`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from [`Machine::attach_install_media_iso_opfs`] so
    /// callers do not silently overwrite previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_install_media_iso_opfs(path).await?;
        self.set_ide_secondary_master_atapi_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Attach an existing OPFS-backed ISO image as the canonical install media CD-ROM, preserving
    /// guest-visible ATAPI media state.
    ///
    /// This is intended for snapshot restore flows where the ATAPI device state is restored from a
    /// snapshot but the host-side backend must be re-attached before resuming execution.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs_for_restore(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let disk = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js(
                    "Machine.attach_install_media_iso_opfs_for_restore",
                    &path,
                    e,
                )
            })?;

        self.inner
            .attach_ide_secondary_master_iso_for_restore(Box::new(disk))
            .map_err(|e| {
                opfs_context_error_to_js(
                    "Machine.attach_install_media_iso_opfs_for_restore",
                    &path,
                    e,
                )
            })
    }

    /// Attach an existing OPFS-backed ISO image as the canonical install media CD-ROM, preserving
    /// guest-visible ATAPI media state, and set the snapshot overlay reference (`DISKS` entry) for
    /// `disk_id=1`.
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// This method is intentionally separate from
    /// [`Machine::attach_install_media_iso_opfs_for_restore`] so callers do not silently overwrite
    /// previously configured overlay refs unless they opt in.
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_iso_opfs_for_restore_and_set_overlay_ref(
        &mut self,
        path: String,
    ) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        self.attach_install_media_iso_opfs_for_restore(path).await?;
        self.set_ide_secondary_master_atapi_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Attach the canonical primary HDD (`disk_id=0`, AHCI port 0) as a copy-on-write disk built
    /// from:
    /// - a base image (OPFS; raw or AeroSparse), plus
    /// - an `aerosparse` overlay (OPFS) that stores all guest writes.
    ///
    /// This allows the VM to boot from a persistent base disk image without mutating it, while
    /// still supporting writable disks and snapshot/restore flows.
    ///
    /// The overlay file is created if it does not exist. If it already exists, its aerosparse
    /// header must match the base disk size. If `overlay_block_size_bytes` is non-zero, it must
    /// also match the header's block size. Pass `0` to infer the block size from an existing
    /// overlay header (useful for snapshot restore flows where the host does not persist the block
    /// size separately).
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn set_primary_hdd_opfs_cow(
        &mut self,
        base_path: String,
        overlay_path: String,
        overlay_block_size_bytes: u32,
    ) -> Result<(), JsValue> {
        let paths = format!("base={base_path}, overlay={overlay_path}");
        let base_storage = aero_opfs::OpfsByteStorage::open(&base_path, false)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.set_primary_hdd_opfs_cow(base)", &paths, e)
            })?;

        let base_disk = aero_storage::DiskImage::open_auto(base_storage).map_err(|e| {
            opfs_context_error_to_js("Machine.set_primary_hdd_opfs_cow(base)", &paths, e)
        })?;

        let overlay_storage = aero_opfs::OpfsByteStorage::open(&overlay_path, true)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.set_primary_hdd_opfs_cow(overlay)", &paths, e)
            })?;

        let cow_disk =
            open_or_create_cow_disk(base_disk, overlay_storage, overlay_block_size_bytes).map_err(
                |e| opfs_context_error_to_js("Machine.set_primary_hdd_opfs_cow", &paths, e),
            )?;

        self.inner
            .set_disk_backend(Box::new(cow_disk))
            .map_err(|e| opfs_context_error_to_js("Machine.set_primary_hdd_opfs_cow", &paths, e))?;

        // Record stable `{base_image, overlay_image}` strings into the DISKS snapshot overlay refs
        // so the JS coordinator can reopen these images after snapshot restore.
        self.inner
            .set_ahci_port0_disk_overlay_ref(&base_path, &overlay_path);
        Ok(())
    }

    /// Attach the canonical primary HDD (`disk_id=0`, AHCI port 0) from an existing OPFS-backed
    /// raw disk image *without* a copy-on-write overlay, and set the snapshot overlay reference
    /// (`DISKS` entry).
    ///
    /// This is a minimal disk attach API intended for machine-mode bring-up and debugging (e.g.
    /// when using a pre-writable disk image).
    ///
    /// This sets:
    /// - `base_image = path`
    /// - `overlay_image = ""`
    ///
    /// so that snapshot restore flows can reattach the same primary HDD based solely on snapshot
    /// contents.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_primary_hdd_opfs_existing(&mut self, path: String) -> Result<(), JsValue> {
        let overlay_path = path.clone();
        let backend = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.set_primary_hdd_opfs_existing", &path, e)
            })?;
        self.inner
            .set_disk_backend(Box::new(backend))
            .map_err(|e| {
                opfs_context_error_to_js("Machine.set_primary_hdd_opfs_existing", &path, e)
            })?;

        self.set_ahci_port0_disk_overlay_ref(&overlay_path, "");
        Ok(())
    }

    /// Attach an OPFS-backed ISO image as the canonical install media (`disk_id=1`).
    ///
    /// This attaches the ISO to the canonical Windows 7 storage topology slot:
    /// IDE secondary channel, master device (ATAPI CD-ROM).
    ///
    /// ## Snapshot overlay ref policy
    ///
    /// Install media is treated as read-only, so the machine records:
    /// - `base_image = path`
    /// - `overlay_image = ""` (empty string indicates "no writable overlay")
    #[cfg(target_arch = "wasm32")]
    pub async fn attach_install_media_opfs_iso(&mut self, path: String) -> Result<(), JsValue> {
        let path_for_err = path.clone();
        let backend = aero_opfs::OpfsBackend::open_existing(&path)
            .await
            .map_err(|e| {
                opfs_disk_error_to_js("Machine.attach_install_media_opfs_iso", &path, e)
            })?;
        self.inner
            .attach_install_media_iso_and_set_overlay_ref(Box::new(backend), path)
            .map_err(|e| {
                opfs_context_error_to_js("Machine.attach_install_media_opfs_iso", &path_for_err, e)
            })
    }

    /// Eject the canonical install media (IDE secondary master ATAPI) and clear its snapshot
    /// overlay ref (`disk_id=1`).
    pub fn eject_install_media(&mut self) {
        self.inner.eject_install_media();
    }

    /// Back-compat alias: attach an existing OPFS-backed ISO as the canonical install media CD-ROM.
    ///
    /// Prefer [`Machine::attach_install_media_opfs_iso`], which also records the snapshot overlay
    /// reference for `disk_id=1`.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_cd_opfs_existing(&mut self, path: String) -> Result<(), JsValue> {
        self.attach_install_media_opfs_iso(path).await
    }

    pub fn run_slice(&mut self, max_insts: u32) -> RunExit {
        let exit = self.inner.run_slice(max_insts as u64);

        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            // Poll for legacy VGA/VBE mode transitions and publish scanout state updates.
            // (WDDM scanout updates are published by the AeroGPU path.)
            self.maybe_publish_legacy_scanout_from_vga();
        }

        RunExit::from_native(exit)
    }

    // -------------------------------------------------------------------------
    // Scanout state (threaded WASM only)
    // -------------------------------------------------------------------------

    /// Pointer (into wasm linear memory) to the shared [`ScanoutState`] header.
    ///
    /// Returns 0 when the build does not support a shared scanout state (e.g. non-threaded WASM
    /// variant or non-wasm host builds).
    pub fn scanout_state_ptr(&self) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            Self::scanout_state_offset_bytes()
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            0
        }
    }

    /// Length in bytes of the shared scanout state header.
    pub fn scanout_state_len_bytes(&self) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            SCANOUT_STATE_BYTE_LEN as u32
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            0
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn scanout_state_offset_bytes() -> u32 {
        // Keep this in sync with:
        // - `crates/aero-wasm/src/runtime_alloc.rs` (`HEAP_TAIL_GUARD_BYTES`)
        // - `web/src/runtime/shared_layout.ts` (embedded scanoutState/cursorState offsets)
        const WASM_MEMORY_PROBE_WINDOW_BYTES: u32 = 64;
        let tail_guard = WASM_MEMORY_PROBE_WINDOW_BYTES
            + SCANOUT_STATE_BYTE_LEN as u32
            + CURSOR_STATE_BYTE_LEN as u32;
        (crate::guest_layout::RUNTIME_RESERVED_BYTES as u32).saturating_sub(tail_guard)
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn ensure_runtime_reserved_floor_for_scanout_state() {
        // The browser runtime always instantiates this module with a `WebAssembly.Memory` that is
        // at least `RUNTIME_RESERVED_BYTES` large.
        //
        // wasm-bindgen tests (and other non-worker contexts) may start with a much smaller default
        // linear memory; grow it to the required floor so `scanout_state_offset_bytes()` is
        // in-bounds before we create a reference into linear memory.
        let page_bytes = crate::guest_layout::WASM_PAGE_BYTES as usize;
        let reserved_bytes = crate::guest_layout::RUNTIME_RESERVED_BYTES as usize;
        let cur_pages = core::arch::wasm32::memory_size(0);
        let cur_bytes = cur_pages.saturating_mul(page_bytes);
        if cur_bytes >= reserved_bytes {
            return;
        }

        let desired_pages = reserved_bytes.div_ceil(page_bytes);
        let delta_pages = desired_pages.saturating_sub(cur_pages);
        if delta_pages == 0 {
            return;
        }

        // `memory_grow` returns the previous size, or `usize::MAX` on failure.
        let prev = core::arch::wasm32::memory_grow(0, delta_pages);
        if prev == usize::MAX {
            // Re-check and abort if we still cannot satisfy the runtime layout contract.
            let pages = core::arch::wasm32::memory_size(0);
            let bytes = pages.saturating_mul(page_bytes);
            if bytes < reserved_bytes {
                panic!(
                    "WASM linear memory too small for scanout state: have {bytes} bytes, need at least {reserved_bytes} bytes (runtime reserved). Ensure the module is instantiated with the worker guest memory."
                );
            }
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn scanout_state_ref() -> &'static ScanoutState {
        Self::ensure_runtime_reserved_floor_for_scanout_state();
        let offset = Self::scanout_state_offset_bytes();
        debug_assert_eq!(
            offset % (core::mem::align_of::<ScanoutState>() as u32),
            0,
            "scanout state offset must be aligned to ScanoutState"
        );
        // Safety:
        // - The web runtime allocates at least `RUNTIME_RESERVED_BYTES` of linear memory.
        // - The wasm-side runtime allocator leaves a tail guard so this region does not overlap heap allocations.
        // - The region is treated as a plain `u32` array (Atomics-compatible) by both Rust and JS.
        let ptr: *const ScanoutState = core::ptr::with_exposed_provenance(offset as usize);
        unsafe { &*ptr }
    }

    // -------------------------------------------------------------------------
    // Cursor state (threaded WASM only)
    // -------------------------------------------------------------------------

    /// Pointer (into wasm linear memory) to the shared [`CursorState`] header.
    ///
    /// Returns 0 when the build does not support a shared cursor state (e.g. non-threaded WASM
    /// variant or non-wasm host builds).
    pub fn cursor_state_ptr(&self) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            Self::cursor_state_offset_bytes()
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            0
        }
    }

    /// Length in bytes of the shared cursor state header.
    pub fn cursor_state_len_bytes(&self) -> u32 {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
        {
            CURSOR_STATE_BYTE_LEN as u32
        }
        #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threaded")))]
        {
            0
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn cursor_state_offset_bytes() -> u32 {
        Self::scanout_state_offset_bytes().saturating_add(SCANOUT_STATE_BYTE_LEN as u32)
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn cursor_state_ref() -> &'static CursorState {
        Self::ensure_runtime_reserved_floor_for_scanout_state();
        let offset = Self::cursor_state_offset_bytes();
        // Safety: same argument as `scanout_state_ref()`; cursor state is placed in the same
        // allocator-excluded tail guard region inside wasm linear memory.
        let ptr: *const CursorState = core::ptr::with_exposed_provenance(offset as usize);
        unsafe { &*ptr }
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn publish_scanout(&mut self, update: ScanoutStateUpdate) {
        if self.last_published_scanout == Some(update) {
            return;
        }

        // Avoid bumping generation if the underlying machine/device model already published the
        // same update (e.g. BIOS INT10 mode set updates).
        if let Some(snap) = self.scanout_state.try_snapshot()
            && snap.source == update.source
            && snap.base_paddr_lo == update.base_paddr_lo
            && snap.base_paddr_hi == update.base_paddr_hi
            && snap.width == update.width
            && snap.height == update.height
            && snap.pitch_bytes == update.pitch_bytes
            && snap.format == update.format
        {
            self.last_published_scanout = Some(update);
            return;
        }

        if self.scanout_state.try_publish(update).is_some() {
            self.last_published_scanout = Some(update);
        }
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn publish_legacy_text_scanout(&mut self) {
        self.publish_scanout(ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: SCANOUT_FORMAT_B8G8R8X8,
        });
    }

    #[cfg(all(target_arch = "wasm32", feature = "wasm-threaded"))]
    fn maybe_publish_legacy_scanout_from_vga(&mut self) {
        // Do not override WDDM ownership (see `docs/16-aerogpu-vga-vesa-compat.md`).
        match self.scanout_state.try_snapshot() {
            Some(snap) if snap.source == SCANOUT_SOURCE_WDDM => return,
            None => return,
            _ => {}
        }

        let Some(vga) = self.inner.vga() else {
            return;
        };

        // Prefer the canonical helper that accounts for BIOS VBE scanline overrides (INT 10h
        // AX=4F06) in addition to the Bochs VBE register file.
        let update = vga.borrow().active_scanout_update();
        if update.source == SCANOUT_SOURCE_LEGACY_VBE_LFB
            && update.width != 0
            && update.height != 0
            && update.pitch_bytes != 0
        {
            self.publish_scanout(update);
        } else {
            self.publish_legacy_text_scanout();
        }
    }

    /// Returns and clears any accumulated BIOS "TTY output".
    ///
    /// This is a best-effort early-boot debug log recorded by the HLE BIOS, currently capturing:
    /// - INT 10h teletype output (AH=0Eh)
    /// - BIOS boot panic strings (e.g. missing/invalid boot sector)
    pub fn bios_tty_output(&mut self) -> Vec<u8> {
        let out = self.inner.bios_tty_output_bytes();
        self.inner.clear_bios_tty_output();
        out
    }

    /// Return the current BIOS TTY output length without copying the bytes into JS.
    pub fn bios_tty_output_len(&self) -> u32 {
        self.inner.bios_tty_output().len().min(u32::MAX as usize) as u32
    }

    /// Clear the BIOS TTY output buffer without reading it.
    pub fn clear_bios_tty_output(&mut self) {
        self.inner.clear_bios_tty_output();
    }

    /// Read a 32-bit aligned DWORD from PCI configuration space using config mechanism #1.
    ///
    /// This is primarily intended for tests and host-side inspection. The canonical browser runtime
    /// can also use it to verify that expected PCI devices are present.
    ///
    /// Returns `0xFFFF_FFFF` when the machine does not have PCI config ports (PC platform disabled)
    /// or when parameters are out of range.
    pub fn pci_config_read_u32(
        &mut self,
        bus: u32,
        device: u32,
        function: u32,
        offset: u32,
    ) -> u32 {
        if self.inner.pci_config_ports().is_none() {
            return u32::MAX;
        }

        let Ok(bus) = u8::try_from(bus) else {
            return u32::MAX;
        };
        let Ok(device) = u8::try_from(device) else {
            return u32::MAX;
        };
        let Ok(function) = u8::try_from(function) else {
            return u32::MAX;
        };
        if device >= 32 || function >= 8 {
            return u32::MAX;
        }
        if offset >= 256 {
            return u32::MAX;
        }

        // PCI configuration mechanism #1 address format:
        // - bit31: enable
        // - bits23..16: bus
        // - bits15..11: device
        // - bits10..8: function
        // - bits7..2: register number (DWORD aligned)
        // - bits1..0: must be 0
        let aligned = offset & !0x3;
        let addr = 0x8000_0000u32
            | (u32::from(bus) << 16)
            | (u32::from(device) << 11)
            | (u32::from(function) << 8)
            | aligned;

        self.inner.io_write(0xCF8, 4, addr);
        self.inner.io_read(0xCFC, 4)
    }

    /// Returns and clears any accumulated serial output.
    pub fn serial_output(&mut self) -> Vec<u8> {
        self.inner.take_serial_output()
    }

    /// Return the current serial output length without copying the bytes into JS.
    pub fn serial_output_len(&mut self) -> u32 {
        self.inner.serial_output_len().min(u64::from(u32::MAX)) as u32
    }

    /// Returns and clears any accumulated DebugCon output (I/O port `0xE9`).
    ///
    /// Many emulators (Bochs/QEMU) expose a "debug console" sink at port `0xE9`, allowing guests to
    /// print a byte with `out 0xE9, al`.
    pub fn debugcon_output(&mut self) -> Vec<u8> {
        self.inner.take_debugcon_output()
    }

    /// Return the current DebugCon output length without copying the bytes into JS.
    pub fn debugcon_output_len(&mut self) -> u32 {
        self.inner.debugcon_output_len().min(u64::from(u32::MAX)) as u32
    }

    // -------------------------------------------------------------------------
    // Unified display scanout (legacy VGA/VBE today; AeroGPU/WDDM later)
    // -------------------------------------------------------------------------

    /// Re-render the emulated display into the machine's host-visible framebuffer cache.
    ///
    /// Call this before reading the framebuffer via `display_framebuffer_*` or before sampling
    /// `display_width()`/`display_height()` after the guest changes video modes.
    pub fn display_present(&mut self) {
        self.inner.display_present();
    }

    /// Current display output width in pixels (0 if no display scanout is available).
    ///
    /// This is the width of the last framebuffer produced by [`Machine::display_present`].
    pub fn display_width(&self) -> u32 {
        let (w, _) = self.inner.display_resolution();
        w
    }

    /// Current display output height in pixels (0 if no display scanout is available).
    ///
    /// This is the height of the last framebuffer produced by [`Machine::display_present`].
    pub fn display_height(&self) -> u32 {
        let (_, h) = self.inner.display_resolution();
        h
    }

    /// Current display output stride in bytes (RGBA8888, tightly packed).
    pub fn display_stride_bytes(&self) -> u32 {
        self.display_width().saturating_mul(4)
    }

    /// Pointer (into wasm linear memory) to the display RGBA8888 framebuffer.
    ///
    /// Returns `0` if no display scanout is available.
    ///
    /// # Safety contract (JS/host)
    /// The returned pointer is a view into the `aero_machine::Machine`'s internal framebuffer cache
    /// (a `Vec<u32>`).
    ///
    /// The caller must re-query both the pointer and length after each [`Machine::display_present`]
    /// call because the underlying `Vec` can be resized/reallocated, invalidating any previously
    /// returned pointer.
    ///
    /// Note: [`Machine::display_framebuffer_copy_rgba8888`] calls [`Machine::display_present`]
    /// internally, so calling it also invalidates any previous pointer.
    #[cfg(target_arch = "wasm32")]
    pub fn display_framebuffer_ptr(&self) -> u32 {
        let fb = self.inner.display_framebuffer();
        if fb.is_empty() {
            return 0;
        }
        fb.as_ptr() as u32
    }

    /// See [`Machine::display_framebuffer_ptr`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn display_framebuffer_ptr(&self) -> u32 {
        0
    }

    /// Length in bytes of the display framebuffer (RGBA8888).
    ///
    /// Returns `0` if no display scanout is available.
    ///
    /// The caller must re-query this length after each [`Machine::display_present`] call because it
    /// may change (e.g. due to resize/reallocation).
    pub fn display_framebuffer_len_bytes(&self) -> u32 {
        let fb = self.inner.display_framebuffer();
        let bytes = (fb.len() as u64).saturating_mul(4);
        bytes.min(u64::from(u32::MAX)) as u32
    }

    /// Copy the current display buffer into a byte vector (RGBA8888).
    ///
    /// This is significantly slower than using `display_framebuffer_ptr()`/`display_framebuffer_len_bytes()`,
    /// but it is convenient for tests and for simple JS callers.
    ///
    /// Note: this calls [`Machine::display_present`] internally, so calling it invalidates any
    /// previously returned framebuffer pointer.
    pub fn display_framebuffer_copy_rgba8888(&mut self) -> Vec<u8> {
        self.inner.display_present();
        let fb: &[u32] = self.inner.display_framebuffer();
        if fb.is_empty() {
            return Vec::new();
        }

        // The canonical display scanout stores pixels as `u32::from_le_bytes([r,g,b,a])`.
        //
        // On little-endian targets (including wasm32), the in-memory representation of `u32`
        // matches RGBA byte order, so we can copy the framebuffer as a raw byte slice.
        //
        // On big-endian targets, we must convert each pixel to little-endian bytes explicitly.
        #[cfg(target_endian = "little")]
        {
            // Safety: `fb` is a valid slice of `u32` pixels (RGBA8888). Reinterpreting it as bytes
            // preserves RGBA byte order on little-endian architectures.
            let bytes =
                unsafe { core::slice::from_raw_parts(fb.as_ptr() as *const u8, fb.len() * 4) };
            bytes.to_vec()
        }

        #[cfg(not(target_endian = "little"))]
        {
            let mut out = Vec::with_capacity(fb.len().saturating_mul(4));
            for &px in fb {
                out.extend_from_slice(&px.to_le_bytes());
            }
            out
        }
    }

    // -------------------------------------------------------------------------
    // AeroGPU + VBE discovery helpers (tests / JS glue)
    // -------------------------------------------------------------------------

    /// Return the BIOS-reported VBE linear framebuffer (LFB) base address.
    ///
    /// This is the value reported via `INT 10h AX=4F01h` (`VBE ModeInfoBlock.PhysBasePtr`).
    pub fn vbe_lfb_base(&self) -> u32 {
        self.inner.vbe_lfb_base_u32()
    }

    /// Returns whether the canonical AeroGPU PCI function (`00:07.0`, `A3A0:0001`) is present.
    pub fn aerogpu_present(&self) -> bool {
        self.inner.aerogpu_bdf().is_some()
    }

    /// Return the base address assigned to an AeroGPU PCI BAR.
    ///
    /// Returns `0` when AeroGPU is not present or when the BAR is missing/unassigned.
    pub fn aerogpu_bar_base(&self, bar: u8) -> u32 {
        let Some(bdf) = self.inner.aerogpu_bdf() else {
            return 0;
        };
        let Some(base) = self.inner.pci_bar_base(bdf, bar) else {
            return 0;
        };
        u32::try_from(base).unwrap_or(0)
    }

    // -------------------------------------------------------------------------
    // Legacy VGA/SVGA scanout (BIOS text mode + VBE graphics)
    // -------------------------------------------------------------------------

    /// Re-render the VGA/SVGA device into its front buffer if necessary.
    ///
    /// Call this before reading the framebuffer via `vga_framebuffer_*` or before sampling
    /// `vga_width()`/`vga_height()` after the guest changes video modes.
    ///
    /// This is a transitional API for the legacy VGA/VBE scanout path. Prefer the unified
    /// `display_*` methods instead.
    pub fn vga_present(&mut self) {
        let Some(vga) = self.inner.vga() else {
            return;
        };
        vga.borrow_mut().present();
    }

    /// Current VGA output width in pixels (0 if the machine does not have a VGA device).
    ///
    /// Legacy/transitional: prefer [`Machine::display_width`].
    pub fn vga_width(&self) -> u32 {
        let Some(vga) = self.inner.vga() else {
            return 0;
        };
        let (w, _) = vga.borrow().get_resolution();
        w
    }

    /// Current VGA output height in pixels (0 if the machine does not have a VGA device).
    ///
    /// Legacy/transitional: prefer [`Machine::display_height`].
    pub fn vga_height(&self) -> u32 {
        let Some(vga) = self.inner.vga() else {
            return 0;
        };
        let (_, h) = vga.borrow().get_resolution();
        h
    }

    /// Current VGA output stride in bytes (RGBA8888, tightly packed).
    ///
    /// Legacy/transitional: prefer [`Machine::display_stride_bytes`].
    pub fn vga_stride_bytes(&self) -> u32 {
        self.vga_width().saturating_mul(4)
    }

    /// Pointer (into wasm linear memory) to the VGA RGBA8888 framebuffer.
    ///
    /// Returns `0` if VGA is absent.
    ///
    /// Legacy/transitional: prefer [`Machine::display_framebuffer_ptr`].
    ///
    /// # Safety contract (JS/host)
    /// The caller must re-query this pointer after each [`Machine::vga_present`] call because the
    /// front buffer pointer may change (front/back swap or resize).
    ///
    /// Note: [`Machine::vga_framebuffer_copy_rgba8888`] and [`Machine::vga_framebuffer_rgba8888_copy`]
    /// call [`Machine::vga_present`] internally, so calling them also invalidates any previously
    /// returned pointer.
    #[cfg(target_arch = "wasm32")]
    pub fn vga_framebuffer_ptr(&self) -> u32 {
        let Some(vga) = self.inner.vga() else {
            return 0;
        };
        vga.borrow().get_framebuffer().as_ptr() as u32
    }

    /// See [`Machine::vga_framebuffer_ptr`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn vga_framebuffer_ptr(&self) -> u32 {
        0
    }

    /// Length in bytes of the VGA framebuffer.
    ///
    /// Returns `0` if VGA is absent.
    ///
    /// Legacy/transitional: prefer [`Machine::display_framebuffer_len_bytes`].
    ///
    /// The caller must re-query this length after each [`Machine::vga_present`] call because it
    /// may change (front/back swap or resize).
    pub fn vga_framebuffer_len_bytes(&self) -> u32 {
        let Some(vga) = self.inner.vga() else {
            return 0;
        };
        let bytes = (vga.borrow().get_framebuffer().len() as u64).saturating_mul(4);
        bytes.min(u64::from(u32::MAX)) as u32
    }

    /// Copy the current VGA front buffer into a byte vector (RGBA8888).
    ///
    /// This is significantly slower than using `vga_framebuffer_ptr()`/`vga_framebuffer_len_bytes()`,
    /// but it is convenient for tests and for simple JS callers.
    ///
    /// Legacy/transitional: prefer [`Machine::display_framebuffer_copy_rgba8888`].
    pub fn vga_framebuffer_copy_rgba8888(&mut self) -> Vec<u8> {
        let Some(vga) = self.inner.vga() else {
            return Vec::new();
        };

        let mut vga = vga.borrow_mut();
        vga.present();
        let fb: &[u32] = vga.get_framebuffer();
        // The VGA device stores pixels as `u32::from_le_bytes([r,g,b,a])`.
        //
        // On little-endian targets (including wasm32), the in-memory representation of `u32`
        // matches RGBA byte order, so we can copy the framebuffer as a raw byte slice.
        //
        // On big-endian targets, we must convert each pixel to little-endian bytes explicitly.
        #[cfg(target_endian = "little")]
        {
            // Safety: `fb` is a valid slice of `u32` pixels (RGBA8888). Reinterpreting it as bytes
            // preserves RGBA byte order on little-endian architectures.
            let bytes =
                unsafe { core::slice::from_raw_parts(fb.as_ptr() as *const u8, fb.len() * 4) };
            bytes.to_vec()
        }

        #[cfg(not(target_endian = "little"))]
        {
            let mut out = Vec::with_capacity(fb.len().saturating_mul(4));
            for &px in fb {
                out.extend_from_slice(&px.to_le_bytes());
            }
            out
        }
    }

    /// Legacy helper: copy the current VGA front buffer into a JS `Uint8Array` (RGBA8888).
    ///
    /// Prefer [`Machine::vga_framebuffer_copy_rgba8888`] (returns a `Vec<u8>` which wasm-bindgen
    /// already maps to `Uint8Array`) or the raw pointer/len view for zero-copy scanout.
    ///
    /// Returns `null` if VGA is absent.
    #[cfg(target_arch = "wasm32")]
    pub fn vga_framebuffer_rgba8888_copy(&mut self) -> JsValue {
        let Some(vga) = self.inner.vga() else {
            return JsValue::NULL;
        };
        let mut vga = vga.borrow_mut();
        vga.present();
        let fb: &[u32] = vga.get_framebuffer();
        // Safety: `fb` is a valid slice of `u32` pixels; reinterpret as raw bytes.
        let bytes = unsafe { core::slice::from_raw_parts(fb.as_ptr() as *const u8, fb.len() * 4) };
        Uint8Array::from(bytes).into()
    }

    // -------------------------------------------------------------------------
    // AeroGPU submission bridge (WASM external GPU worker integration)
    // -------------------------------------------------------------------------
    //
    // This is an integration hook for browser builds where the guest-visible AeroGPU PCI device
    // model runs in-process (inside `aero-wasm`), but command execution/present happens externally
    // (e.g. a dedicated GPU worker running `aero-gpu-wasm`).
    //
    // Calling protocol (high level):
    // 1. The guest rings the AeroGPU doorbell (MMIO), causing the device model to decode new
    //    submissions and mark their fences as in-flight.
    // 2. JS calls `aerogpu_drain_submissions()` to retrieve newly-decoded submissions.
    // 3. JS executes each submission out-of-process and, once complete, calls
    //    `aerogpu_complete_fence(signalFence)` for each signaled fence.
    // 4. The device model updates the fence page and IRQ state in response to completions.
    //
    // NOTE: This hook is opt-in: calling `aerogpu_drain_submissions()` enables the submission bridge
    // inside the in-process device model so subsequent submissions no longer complete fences
    // automatically. Callers must then invoke `aerogpu_complete_fence()` for forward progress.

    /// Drain newly-decoded AeroGPU submissions.
    ///
    /// Returns an array of objects:
    /// `{ cmdStream: Uint8Array, signalFence: BigInt, contextId: number, engineId: number, flags: number, allocTable: Uint8Array | null }`.
    #[cfg(target_arch = "wasm32")]
    pub fn aerogpu_drain_submissions(&mut self) -> JsValue {
        // Enable external-executor fence completion semantics for the browser runtime. This keeps
        // native `aero_machine::Machine` tests free to drain submissions for inspection while
        // retaining legacy "no-op backend" forward progress.
        self.inner.aerogpu_enable_submission_bridge();
        let subs = self.inner.aerogpu_drain_submissions();
        let out = js_sys::Array::new();

        for sub in subs {
            let obj = Object::new();

            let cmd_stream = Uint8Array::from(sub.cmd_stream.as_slice());
            let _ = Reflect::set(&obj, &"cmdStream".into(), &cmd_stream.into());
            let _ = Reflect::set(
                &obj,
                &"signalFence".into(),
                &BigInt::from(sub.signal_fence).into(),
            );
            let _ = Reflect::set(&obj, &"contextId".into(), &JsValue::from(sub.context_id));
            let _ = Reflect::set(&obj, &"engineId".into(), &JsValue::from(sub.engine_id));
            let _ = Reflect::set(&obj, &"flags".into(), &JsValue::from(sub.flags));

            let alloc_table: JsValue = match sub.alloc_table {
                Some(bytes) => Uint8Array::from(bytes.as_slice()).into(),
                None => JsValue::NULL,
            };
            let _ = Reflect::set(&obj, &"allocTable".into(), &alloc_table);

            out.push(&obj);
        }

        out.into()
    }

    /// Mark a previously-drained submission's fence as complete.
    ///
    /// The underlying AeroGPU device model is responsible for raising IRQ status bits and updating
    /// the guest fence page so the Windows driver observes forward progress.
    #[cfg(target_arch = "wasm32")]
    pub fn aerogpu_complete_fence(&mut self, fence: BigInt) {
        // `js_sys::BigInt` does not expose a lossless `to_u64`, so round-trip through a decimal
        // string.
        let Ok(s) = fence.to_string(10) else {
            return;
        };
        let Some(s) = s.as_string() else {
            return;
        };
        let Ok(value) = s.parse::<u64>() else {
            return;
        };
        // Ensure the in-process device model is in external-executor mode before delivering the
        // completion so the fence is not treated as a legacy no-op completion.
        self.inner.aerogpu_enable_submission_bridge();
        self.inner.aerogpu_complete_fence(value);
    }

    /// Inject a batch of input events encoded in the `InputEventQueue` wire format
    /// (`web/src/input/event_queue.ts`).
    ///
    /// This is a high-throughput entry point intended to reduce JS parsing overhead by letting Rust
    /// decode and dispatch events in one call.
    pub fn inject_input_batch(&mut self, words: &[u32]) {
        self.inner.inject_input_batch(words);
        // The batch may contain mouse button state events; conservatively treat our JS-facing cache
        // as unknown so follow-up per-button injections don't assume it is synchronized.
        self.mouse_buttons_known = false;
    }

    /// Inject a browser-style keyboard event into the guest.
    ///
    /// `code` must be a DOM `KeyboardEvent.code` string (e.g. `"KeyA"`, `"Enter"`, `"ArrowUp"`).
    /// Unknown codes are ignored.
    ///
    /// Routing:
    /// - If `code` has a PS/2 Set-2 scancode mapping and the i8042 controller is present, it is
    ///   injected via PS/2.
    /// - Otherwise, if `code` maps to a HID Consumer Control usage (media keys / browser navigation),
    ///   it is forwarded to the Consumer Control backend:
    ///   - virtio-input (when the guest driver is active and the usage is supported), otherwise
    ///   - the synthetic USB HID consumer-control device (when enabled).
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        self.inner.inject_browser_key(code, pressed);
    }

    /// Inject up to 4 raw PS/2 Set-2 scancode bytes into the guest i8042 keyboard device.
    ///
    /// This matches the format used by `web/src/input/event_queue.ts` (`InputEventType.KeyScancode`):
    /// - `packed`: little-endian packed bytes (b0 in bits 0..7)
    /// - `len`: number of valid bytes in `packed` (1..=4)
    ///
    /// Bytes are treated as Set-2 scancode bytes (including `0xE0`/`0xF0` prefixes).
    pub fn inject_key_scancode_bytes(&mut self, packed: u32, len: u8) {
        let len = len.min(4) as usize;
        if len == 0 {
            return;
        }

        let bytes = packed.to_le_bytes();
        self.inner.inject_key_scancode_bytes(&bytes[..len]);
    }

    /// Inject an arbitrary-length raw PS/2 Set-2 scancode byte sequence into the guest i8042 keyboard device.
    pub fn inject_keyboard_bytes(&mut self, bytes: &[u8]) {
        self.inner.inject_key_scancode_bytes(bytes);
    }

    /// Inject a relative PS/2 mouse movement event (plus optional wheel delta).
    ///
    /// Coordinate conventions:
    /// - `dx`: positive is right.
    /// - `dy`: positive is up (PS/2 convention).
    /// - `wheel`: positive is wheel up.
    pub fn inject_ps2_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        // The canonical machine mouse injection API uses browser-style coordinates (+Y is down).
        // Convert PS/2 convention (+Y is up) into that API.
        // Host input values are untrusted; avoid overflow when negating `i32::MIN`.
        self.inject_mouse_motion(dx, 0i32.saturating_sub(dy), wheel);
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
    /// - `3`: back
    /// - `4`: forward
    ///
    /// Other values are ignored.
    pub fn inject_mouse_button(&mut self, button: u8, pressed: bool) {
        match button {
            0 => self.inject_mouse_left(pressed),
            1 => self.inject_mouse_middle(pressed),
            2 => self.inject_mouse_right(pressed),
            3 => self.inject_mouse_back(pressed),
            4 => self.inject_mouse_forward(pressed),
            _ => {}
        }
    }

    /// Set PS/2 mouse button state as a bitmask matching DOM `MouseEvent.buttons` (low 5 bits).
    ///
    /// This mirrors the PS/2 packet button bits (and also DOM `MouseEvent.buttons`).
    pub fn inject_ps2_mouse_buttons(&mut self, buttons: u8) {
        self.inject_mouse_buttons_mask(buttons);
    }

    /// Set all mouse buttons at once using a bitmask matching DOM `MouseEvent.buttons`:
    /// - bit0 (`0x01`): left
    /// - bit1 (`0x02`): right
    /// - bit2 (`0x04`): middle
    /// - bit3 (`0x08`): back/side (only emitted in PS/2 packets if the guest enabled device ID 0x04)
    /// - bit4 (`0x10`): forward/extra (same note as bit3)
    ///
    /// Bits above `0x1f` are ignored.
    pub fn inject_mouse_buttons_mask(&mut self, mask: u8) {
        let next = mask & 0x1f;
        // Delegate to the canonical machine helper so we compute deltas using authoritative guest
        // device state. The guest can reset the PS/2 mouse independently, making a purely
        // host-side "previous buttons" cache stale (and potentially turning this into a no-op even
        // when the guest button image was cleared).
        self.inner.inject_ps2_mouse_buttons(next);

        self.mouse_buttons = next;
        self.mouse_buttons_known = true;
    }

    /// Convenience wrapper: set the left mouse button state.
    pub fn inject_mouse_left(&mut self, pressed: bool) {
        // Only `inject_mouse_buttons_mask` can fully synchronize all 5 buttons at once. If our
        // cached button state is currently "unknown" (e.g. after snapshot restore), individual
        // transitions shouldn't flip that flag back to "known".
        let known = self.mouse_buttons_known;
        self.inner.inject_mouse_left(pressed);
        if pressed {
            self.mouse_buttons |= 0x01;
        } else {
            self.mouse_buttons &= !0x01;
        }
        self.mouse_buttons_known = known;
    }

    /// Convenience wrapper: set the right mouse button state.
    pub fn inject_mouse_right(&mut self, pressed: bool) {
        let known = self.mouse_buttons_known;
        self.inner.inject_mouse_right(pressed);
        if pressed {
            self.mouse_buttons |= 0x02;
        } else {
            self.mouse_buttons &= !0x02;
        }
        self.mouse_buttons_known = known;
    }

    /// Convenience wrapper: set the middle mouse button state.
    pub fn inject_mouse_middle(&mut self, pressed: bool) {
        let known = self.mouse_buttons_known;
        self.inner.inject_mouse_middle(pressed);
        if pressed {
            self.mouse_buttons |= 0x04;
        } else {
            self.mouse_buttons &= !0x04;
        }
        self.mouse_buttons_known = known;
    }

    /// Convenience wrapper: set the back/side mouse button state.
    pub fn inject_mouse_back(&mut self, pressed: bool) {
        let known = self.mouse_buttons_known;
        self.inner
            .inject_mouse_button(aero_machine::Ps2MouseButton::Side, pressed);
        if pressed {
            self.mouse_buttons |= 0x08;
        } else {
            self.mouse_buttons &= !0x08;
        }
        self.mouse_buttons_known = known;
    }

    /// Convenience wrapper: set the forward/extra mouse button state.
    pub fn inject_mouse_forward(&mut self, pressed: bool) {
        let known = self.mouse_buttons_known;
        self.inner
            .inject_mouse_button(aero_machine::Ps2MouseButton::Extra, pressed);
        if pressed {
            self.mouse_buttons |= 0x10;
        } else {
            self.mouse_buttons &= !0x10;
        }
        self.mouse_buttons_known = known;
    }

    // -------------------------------------------------------------------------
    // virtio-input (paravirtualized keyboard/mouse)
    // -------------------------------------------------------------------------

    /// Inject a Linux input `KEY_*` code into the virtio-input keyboard device (if present).
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_key(&mut self, linux_key: u32, pressed: bool) {
        let Ok(code) = u16::try_from(linux_key) else {
            return;
        };
        self.inner.inject_virtio_key(code, pressed);
    }

    /// Inject relative motion (`REL_X`/`REL_Y`) into the virtio-input mouse device (if present).
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_rel(&mut self, dx: i32, dy: i32) {
        self.inner.inject_virtio_rel(dx, dy);
    }

    /// Inject a Linux input `BTN_*` code into the virtio-input mouse device (if present).
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_button(&mut self, btn: u32, pressed: bool) {
        let Ok(code) = u16::try_from(btn) else {
            return;
        };
        self.inner.inject_virtio_button(code, pressed);
    }

    /// Inject a mouse wheel delta into the virtio-input mouse device (if present).
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_wheel(&mut self, delta: i32) {
        self.inner.inject_virtio_wheel(delta);
    }

    /// Inject a mouse horizontal wheel delta into the virtio-input mouse device (if present).
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_hwheel(&mut self, delta: i32) {
        self.inner.inject_virtio_hwheel(delta);
    }

    /// Inject vertical + horizontal wheel deltas into the virtio-input mouse device (if present),
    /// using a single `SYN_REPORT`.
    ///
    /// This is safe to call even when virtio-input is disabled; it will no-op.
    pub fn inject_virtio_wheel2(&mut self, wheel: i32, hwheel: i32) {
        self.inner.inject_virtio_wheel2(wheel, hwheel);
    }

    // Newer, more explicit aliases (preferred for new code).
    pub fn inject_virtio_mouse_rel(&mut self, dx: i32, dy: i32) {
        self.inject_virtio_rel(dx, dy);
    }

    pub fn inject_virtio_mouse_button(&mut self, btn: u32, pressed: bool) {
        self.inject_virtio_button(btn, pressed);
    }

    pub fn inject_virtio_mouse_wheel(&mut self, delta: i32) {
        self.inject_virtio_wheel(delta);
    }

    /// Whether the guest virtio-input keyboard driver has reached `DRIVER_OK`.
    pub fn virtio_input_keyboard_driver_ok(&self) -> bool {
        self.inner.virtio_input_keyboard_driver_ok()
    }

    /// Returns the virtio-input keyboard LED bitmask (NumLock/CapsLock/ScrollLock/Compose/Kana)
    /// as last set by the guest OS, or 0 if virtio-input is disabled/unavailable.
    pub fn virtio_input_keyboard_leds(&self) -> u32 {
        u32::from(self.inner.virtio_input_keyboard_leds())
    }

    /// Whether the guest virtio-input mouse driver has reached `DRIVER_OK`.
    pub fn virtio_input_mouse_driver_ok(&self) -> bool {
        self.inner.virtio_input_mouse_driver_ok()
    }

    /// Returns the PS/2 (i8042) keyboard LED bitmask as last set by the guest OS, or 0 if the
    /// i8042 controller is disabled/unavailable.
    ///
    /// The bit layout matches [`Machine::usb_hid_keyboard_leds`].
    pub fn ps2_keyboard_leds(&self) -> u32 {
        u32::from(self.inner.ps2_keyboard_leds())
    }

    // -------------------------------------------------------------------------
    // Synthetic USB HID (UHCI external hub)
    // -------------------------------------------------------------------------

    /// Whether the guest has configured the synthetic USB HID keyboard (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_keyboard_configured(&self) -> bool {
        self.inner.usb_hid_keyboard_configured()
    }

    /// Returns the current HID boot keyboard LED bitmask (NumLock/CapsLock/ScrollLock/Compose/Kana)
    /// as last set by the guest OS, or 0 if the synthetic USB HID keyboard is not present.
    pub fn usb_hid_keyboard_leds(&self) -> u32 {
        u32::from(self.inner.usb_hid_keyboard_leds())
    }

    /// Whether the guest has configured the synthetic USB HID mouse (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_mouse_configured(&self) -> bool {
        self.inner.usb_hid_mouse_configured()
    }

    /// Whether the guest has configured the synthetic USB HID gamepad (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_gamepad_configured(&self) -> bool {
        self.inner.usb_hid_gamepad_configured()
    }

    /// Whether the guest has configured the synthetic USB HID consumer-control device
    /// (`SET_CONFIGURATION != 0`).
    pub fn usb_hid_consumer_control_configured(&self) -> bool {
        self.inner.usb_hid_consumer_control_configured()
    }

    /// Inject a USB HID keyboard usage into the synthetic USB HID keyboard device (if enabled).
    pub fn inject_usb_hid_keyboard_usage(&mut self, usage: u32, pressed: bool) {
        let Ok(usage) = u8::try_from(usage) else {
            return;
        };
        self.inner.inject_usb_hid_keyboard_usage(usage, pressed);
    }

    /// Inject a relative mouse movement event into the synthetic USB HID mouse device (if
    /// enabled).
    pub fn inject_usb_hid_mouse_move(&mut self, dx: i32, dy: i32) {
        self.inner.inject_usb_hid_mouse_move(dx, dy);
    }

    /// Set the synthetic USB HID mouse button state (bitmask matching `MouseEvent.buttons`, low 5
    /// bits).
    pub fn inject_usb_hid_mouse_buttons(&mut self, mask: u32) {
        self.inner.inject_usb_hid_mouse_buttons(mask as u8);
    }

    /// Inject a mouse wheel delta into the synthetic USB HID mouse device (if enabled).
    pub fn inject_usb_hid_mouse_wheel(&mut self, delta: i32) {
        self.inner.inject_usb_hid_mouse_wheel(delta);
    }

    /// Inject a horizontal mouse wheel delta into the synthetic USB HID mouse device (if enabled).
    pub fn inject_usb_hid_mouse_hwheel(&mut self, delta: i32) {
        self.inner.inject_usb_hid_mouse_hwheel(delta);
    }

    /// Inject both vertical and horizontal mouse wheel deltas into the synthetic USB HID mouse
    /// device (if enabled).
    pub fn inject_usb_hid_mouse_wheel2(&mut self, wheel: i32, hwheel: i32) {
        self.inner.inject_usb_hid_mouse_wheel2(wheel, hwheel);
    }

    /// Inject an entire 8-byte gamepad report into the synthetic USB HID gamepad device (if
    /// enabled).
    ///
    /// This is packed as two little-endian u32 words (`a` = bytes 0..3, `b` = bytes 4..7) to match
    /// JS/WASM call overhead constraints.
    pub fn inject_usb_hid_gamepad_report(&mut self, a: u32, b: u32) {
        self.inner.inject_usb_hid_gamepad_report(a, b);
    }

    /// Inject a USB HID Consumer Control usage event into the synthetic consumer-control device (if
    /// enabled).
    pub fn inject_usb_hid_consumer_usage(&mut self, usage: u32, pressed: bool) {
        self.inner.inject_usb_hid_consumer_usage(usage, pressed);
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
        self.inner.attach_l2_tunnel_rings(tx, rx);
        Ok(())
    }

    /// Legacy/compatibility alias for [`Machine::attach_l2_tunnel_rings`].
    ///
    /// Some older JS runtimes refer to these rings as "NET rings" rather than "L2 tunnel rings".
    /// Prefer [`Machine::attach_l2_tunnel_rings`] for new code.
    #[cfg(target_arch = "wasm32")]
    pub fn attach_net_rings(
        &mut self,
        net_tx: SharedRingBuffer,
        net_rx: SharedRingBuffer,
    ) -> Result<(), JsValue> {
        self.attach_l2_tunnel_rings(net_tx, net_rx)
    }

    /// Convenience: open `NET_TX`/`NET_RX` rings from an `ioIpcSab` and attach them as an L2 tunnel.
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_from_io_ipc_sab(
        &mut self,
        io_ipc: SharedArrayBuffer,
    ) -> Result<(), JsValue> {
        let tx = open_ring_by_kind(
            io_ipc.clone(),
            aero_ipc::layout::io_ipc_queue_kind::NET_TX,
            0,
        )?;
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

    /// Legacy/compatibility alias for [`Machine::detach_network`].
    ///
    /// Prefer [`Machine::detach_network`] for new code.
    pub fn detach_net_rings(&mut self) {
        self.detach_network();
    }

    /// Poll network devices (e.g. the PCI E1000) and bridge frames via any attached network backend.
    pub fn poll_network(&mut self) {
        self.inner.poll_network();
    }

    /// Return best-effort stats for the attached `NET_TX`/`NET_RX` ring backend (or `null`).
    ///
    /// Values are exposed as JS `BigInt` so callers do not lose precision for long-running VMs.
    #[cfg(target_arch = "wasm32")]
    pub fn net_stats(&self) -> JsValue {
        let Some(stats) = self.inner.network_backend_l2_ring_stats() else {
            return JsValue::NULL;
        };
        // Destructure to keep this JS surface in lockstep with `L2TunnelRingBackendStats`.
        //
        // If the stats struct gains new fields, this becomes a compile error, forcing us to update
        // the exported JS object.
        let aero_net_backend::L2TunnelRingBackendStats {
            tx_pushed_frames,
            tx_pushed_bytes,
            tx_dropped_oversize,
            tx_dropped_oversize_bytes,
            tx_dropped_full,
            tx_dropped_full_bytes,
            rx_popped_frames,
            rx_popped_bytes,
            rx_dropped_oversize,
            rx_dropped_oversize_bytes,
            rx_corrupt,
            rx_broken,
        } = stats;

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_pushed_frames"),
            &BigInt::from(tx_pushed_frames).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_pushed_bytes"),
            &BigInt::from(tx_pushed_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_oversize"),
            &BigInt::from(tx_dropped_oversize).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_oversize_bytes"),
            &BigInt::from(tx_dropped_oversize_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_full"),
            &BigInt::from(tx_dropped_full).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_full_bytes"),
            &BigInt::from(tx_dropped_full_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_popped_frames"),
            &BigInt::from(rx_popped_frames).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_popped_bytes"),
            &BigInt::from(rx_popped_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_dropped_oversize"),
            &BigInt::from(rx_dropped_oversize).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_dropped_oversize_bytes"),
            &BigInt::from(rx_dropped_oversize_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_corrupt"),
            &BigInt::from(rx_corrupt).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_broken"),
            &JsValue::from_bool(rx_broken),
        );

        obj.into()
    }

    // -------------------------------------------------------------------------
    // Snapshot disk overlay refs (DISKS section)
    // -------------------------------------------------------------------------

    /// Stable `disk_id=0`: primary HDD (AHCI `SATA_AHCI_ICH9` port 0).
    pub fn disk_id_primary_hdd() -> u32 {
        aero_machine::Machine::DISK_ID_PRIMARY_HDD
    }

    /// Stable `disk_id=1`: install media / CD-ROM (IDE `IDE_PIIX3` secondary master ATAPI).
    pub fn disk_id_install_media() -> u32 {
        aero_machine::Machine::DISK_ID_INSTALL_MEDIA
    }

    /// Stable `disk_id=2`: optional IDE primary master ATA disk.
    pub fn disk_id_ide_primary_master() -> u32 {
        aero_machine::Machine::DISK_ID_IDE_PRIMARY_MASTER
    }

    /// Configure the snapshot overlay reference for `disk_id=0` (primary HDD).
    pub fn set_ahci_port0_disk_overlay_ref(&mut self, base_image: &str, overlay_image: &str) {
        self.inner
            .set_ahci_port0_disk_overlay_ref(base_image, overlay_image);
    }

    /// Clear the snapshot overlay reference for `disk_id=0` (primary HDD).
    pub fn clear_ahci_port0_disk_overlay_ref(&mut self) {
        self.inner.clear_ahci_port0_disk_overlay_ref();
    }

    /// Configure the snapshot overlay reference for `disk_id=1` (install media / CD-ROM).
    pub fn set_ide_secondary_master_atapi_overlay_ref(
        &mut self,
        base_image: &str,
        overlay_image: &str,
    ) {
        self.inner
            .set_ide_secondary_master_atapi_overlay_ref(base_image, overlay_image);
    }

    /// Clear the snapshot overlay reference for `disk_id=1` (install media / CD-ROM).
    pub fn clear_ide_secondary_master_atapi_overlay_ref(&mut self) {
        self.inner.clear_ide_secondary_master_atapi_overlay_ref();
    }

    /// Configure the snapshot overlay reference for `disk_id=2` (optional IDE primary master ATA).
    pub fn set_ide_primary_master_ata_overlay_ref(
        &mut self,
        base_image: &str,
        overlay_image: &str,
    ) {
        self.inner
            .set_ide_primary_master_ata_overlay_ref(base_image, overlay_image);
    }

    /// Clear the snapshot overlay reference for `disk_id=2` (optional IDE primary master ATA).
    pub fn clear_ide_primary_master_ata_overlay_ref(&mut self) {
        self.inner.clear_ide_primary_master_ata_overlay_ref();
    }

    /// Take disk overlay refs captured from the most recent snapshot restore.
    ///
    /// Storage controller snapshots intentionally drop any attached host backends during restore;
    /// the JS host/coordinator is responsible for reopening and reattaching the referenced
    /// disks/ISOs based on these refs.
    ///
    /// Returns `null` if no overlays were captured.
    #[cfg(target_arch = "wasm32")]
    pub fn take_restored_disk_overlays(&mut self) -> JsValue {
        let Some(overlays) = self.inner.take_restored_disk_overlays() else {
            return JsValue::NULL;
        };

        let arr = js_sys::Array::new();
        for disk in overlays.disks {
            let obj = Object::new();
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("disk_id"),
                &JsValue::from_f64(disk.disk_id as f64),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("base_image"),
                &JsValue::from_str(&disk.base_image),
            );
            let _ = Reflect::set(
                &obj,
                &JsValue::from_str("overlay_image"),
                &JsValue::from_str(&disk.overlay_image),
            );
            arr.push(obj.as_ref());
        }

        arr.into()
    }

    /// Reattach any disks/ISOs referenced by the most recent snapshot restore by treating the
    /// `base_image`/`overlay_image` strings as OPFS paths.
    ///
    /// Snapshot restore intentionally drops storage controller host backends; callers can invoke
    /// this helper after `Machine::restore_snapshot_from_opfs(...)` to reopen and attach all
    /// restored media in one call (no JS-side `disk_id` switch/case).
    ///
    /// This consumes the restored overlay refs via `take_restored_disk_overlays`. If the machine
    /// has not performed a snapshot restore (or the snapshot did not include a `DISKS` section),
    /// this is a no-op.
    ///
    /// ## `disk_id` mapping
    /// - `disk_id=0`: primary HDD (AHCI port 0 / canonical shared disk)
    /// - `disk_id=1`: install media / CD-ROM (IDE secondary master ATAPI)
    /// - `disk_id=2`: IDE primary master ATA disk
    ///
    /// ## Notes
    /// - Empty `base_image` + `overlay_image` entries are treated as "nothing attached" and are
    ///   skipped. (The snapshot format always emits entries for some canonical slots.)
    /// - `disk_id=1` does not currently support overlays; a non-empty `overlay_image` is treated
    ///   as an error.
    ///
    /// Note: OPFS sync access handles are worker-only, so this requires running the WASM module in
    /// a dedicated worker (not the main thread).
    #[cfg(target_arch = "wasm32")]
    pub async fn reattach_restored_disks_from_opfs(&mut self) -> Result<(), JsValue> {
        let Some(overlays) = self.inner.take_restored_disk_overlays() else {
            return Ok(());
        };

        let opfs_error = |disk_id: u32,
                          which: &str,
                          path: &str,
                          err: aero_opfs::DiskError|
         -> JsValue {
            let op = format!(
                "Machine.reattach_restored_disks_from_opfs: open OPFS {which} for disk_id={disk_id}"
            );
            opfs_disk_error_to_js(&op, path, err)
        };

        for disk in overlays.disks {
            let disk_id = disk.disk_id;
            let base_image = disk.base_image;
            let overlay_image = disk.overlay_image;

            // The snapshot format always emits entries for the canonical disks (e.g. `disk_id=0`
            // and `disk_id=1`). Treat empty refs as "slot unused" instead of as an error.
            if base_image.is_empty() {
                if overlay_image.is_empty() {
                    continue;
                }
                return Err(js_error(format!(
                    "reattach_restored_disks_from_opfs: disk_id={disk_id} has empty base_image but non-empty overlay_image={overlay_image:?}"
                )));
            }

            match disk_id {
                aero_machine::Machine::DISK_ID_PRIMARY_HDD => {
                    let base_backend = match aero_opfs::OpfsByteStorage::open(&base_image, false)
                        .await
                    {
                        Ok(backend) => backend,
                        Err(aero_opfs::DiskError::InUse) => {
                            // OPFS sync access handles are exclusive per file. When snapshot
                            // restore is performed on an existing machine instance, the machine's
                            // canonical shared disk backend may still hold an open handle for the
                            // primary HDD base/overlay. In that case, re-opening from OPFS will
                            // fail with `InUse`, but the bytes are already available; we only need
                            // to re-attach the `AtaDrive` to AHCI (snapshot restore clears it).
                            if aero_opfs::opfs_sync_handle_is_open(&base_image) {
                                self.inner.attach_shared_disk_to_ahci_port0().map_err(|e| {
                                    js_error(format!(
                                        "reattach_restored_disks_from_opfs: disk_id=0 base_image={base_image:?} already open; failed to reattach shared disk to AHCI: {e}"
                                    ))
                                })?;
                                continue;
                            }
                            return Err(opfs_error(
                                disk_id,
                                "base_image",
                                &base_image,
                                aero_opfs::DiskError::InUse,
                            ));
                        }
                        Err(e) => return Err(opfs_error(disk_id, "base_image", &base_image, e)),
                    };
                    let base_disk = aero_storage::DiskImage::open_auto(base_backend).map_err(|e| {
                        js_error(format!(
                            "reattach_restored_disks_from_opfs: failed to open disk image for disk_id=0 base_image={base_image:?}: {e}"
                        ))
                    })?;

                    if overlay_image.is_empty() {
                        self.inner
                            .set_disk_backend(Box::new(base_disk))
                            .map_err(|e| {
                                js_error(format!(
                                    "reattach_restored_disks_from_opfs: attach failed for disk_id=0 base_image={base_image:?}: {e}"
                                ))
                            })?;
                        continue;
                    }

                    let overlay_backend = aero_opfs::OpfsByteStorage::open(&overlay_image, false)
                        .await
                        .map_err(|e| opfs_error(disk_id, "overlay_image", &overlay_image, e))?;
                    let cow =
                        aero_storage::AeroCowDisk::open(base_disk, overlay_backend).map_err(|e| {
                            js_error(format!(
                                "reattach_restored_disks_from_opfs: failed to open COW disk for disk_id=0 base_image={base_image:?} overlay_image={overlay_image:?}: {e}"
                            ))
                        })?;
                    self.inner.set_disk_backend(Box::new(cow)).map_err(|e| {
                        js_error(format!(
                            "reattach_restored_disks_from_opfs: attach failed for disk_id=0 base_image={base_image:?} overlay_image={overlay_image:?}: {e}"
                        ))
                    })?;
                }

                aero_machine::Machine::DISK_ID_INSTALL_MEDIA => {
                    if !overlay_image.is_empty() {
                        return Err(js_error(format!(
                            "reattach_restored_disks_from_opfs: disk_id=1 (install media) does not support overlay_image (got {overlay_image:?})"
                        )));
                    }

                    // Only reopen install media when the restored guest state still reports a disc
                    // inserted. Guests can eject media (ATAPI START STOP UNIT), and keeping an
                    // OPFS sync access handle open for an already-ejected ISO can block later
                    // re-attach attempts because handles are exclusive per file.
                    if !self.inner.install_media_is_inserted() {
                        continue;
                    }

                    let disk = aero_opfs::OpfsBackend::open_existing(&base_image)
                        .await
                        .map_err(|e| opfs_error(disk_id, "base_image", &base_image, e))?;
                    self.inner
                        .attach_ide_secondary_master_iso_for_restore(Box::new(disk))
                        .map_err(|e| {
                            js_error(format!(
                                "reattach_restored_disks_from_opfs: attach failed for disk_id=1 base_image={base_image:?}: {e}"
                            ))
                        })?;
                }

                aero_machine::Machine::DISK_ID_IDE_PRIMARY_MASTER => {
                    let base_backend =
                        aero_opfs::OpfsByteStorage::open(&base_image, false)
                            .await
                            .map_err(|e| opfs_error(disk_id, "base_image", &base_image, e))?;
                    let base_disk = aero_storage::DiskImage::open_auto(base_backend).map_err(|e| {
                        js_error(format!(
                            "reattach_restored_disks_from_opfs: failed to open disk image for disk_id=2 base_image={base_image:?}: {e}"
                        ))
                    })?;

                    if overlay_image.is_empty() {
                        self.inner
                            .attach_ide_primary_master_disk(Box::new(base_disk))
                            .map_err(|e| {
                                js_error(format!(
                                    "reattach_restored_disks_from_opfs: attach failed for disk_id=2 base_image={base_image:?}: {e}"
                                ))
                            })?;
                        continue;
                    }

                    let overlay_backend = aero_opfs::OpfsByteStorage::open(&overlay_image, false)
                        .await
                        .map_err(|e| opfs_error(disk_id, "overlay_image", &overlay_image, e))?;
                    let cow =
                        aero_storage::AeroCowDisk::open(base_disk, overlay_backend).map_err(|e| {
                            js_error(format!(
                                "reattach_restored_disks_from_opfs: failed to open COW disk for disk_id=2 base_image={base_image:?} overlay_image={overlay_image:?}: {e}"
                            ))
                        })?;
                    self.inner
                        .attach_ide_primary_master_disk(Box::new(cow))
                        .map_err(|e| {
                            js_error(format!(
                                "reattach_restored_disks_from_opfs: attach failed for disk_id=2 base_image={base_image:?} overlay_image={overlay_image:?}: {e}"
                            ))
                        })?;
                }

                other => {
                    return Err(js_error(format!(
                        "reattach_restored_disks_from_opfs: unknown disk_id {other} (base_image={base_image:?}, overlay_image={overlay_image:?})"
                    )));
                }
            }
        }

        Ok(())
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
            .map_err(|e| opfs_io_error_to_js("Machine.snapshot_full_to_opfs", &path, e))?;

        self.inner
            .save_snapshot_full_to(&mut file)
            .map_err(|e| opfs_snapshot_error_to_js("Machine.snapshot_full_to_opfs", &path, e))?;

        file.close()
            .map_err(|e| opfs_io_error_to_js("Machine.snapshot_full_to_opfs", &path, e))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn snapshot_dirty_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| opfs_io_error_to_js("Machine.snapshot_dirty_to_opfs", &path, e))?;

        self.inner
            .save_snapshot_dirty_to(&mut file)
            .map_err(|e| opfs_snapshot_error_to_js("Machine.snapshot_dirty_to_opfs", &path, e))?;

        file.close()
            .map_err(|e| opfs_io_error_to_js("Machine.snapshot_dirty_to_opfs", &path, e))?;
        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<(), JsValue> {
        let mut file = OpfsSyncFile::open(&path, false)
            .await
            .map_err(|e| opfs_io_error_to_js("Machine.restore_snapshot_from_opfs", &path, e))?;

        self.inner
            .restore_snapshot_from_checked(&mut file)
            .map_err(|e| {
                opfs_snapshot_error_to_js("Machine.restore_snapshot_from_opfs", &path, e)
            })?;

        file.close()
            .map_err(|e| opfs_io_error_to_js("Machine.restore_snapshot_from_opfs", &path, e))?;
        // Restoring rewinds machine device state; we no longer know the current mouse buttons.
        self.mouse_buttons_known = false;
        Ok(())
    }
}

impl Machine {
    /// Rust-native constructor for tests/tools that want to supply a full `aero_machine::MachineConfig`
    /// without going through JS object parsing.
    pub fn new_with_machine_config(cfg: aero_machine::MachineConfig) -> Result<Self, JsValue> {
        Self::new_with_native_config(cfg)
    }

    /// Rust-only debug helper: number of pending virtio-input keyboard events buffered for delivery.
    ///
    /// This is intentionally *not* a wasm-bindgen export; it exists to support lightweight tests.
    pub fn virtio_input_keyboard_pending_events(&mut self) -> u32 {
        let Some(dev) = self.inner.virtio_input_keyboard() else {
            return 0;
        };
        let mut dev = dev.borrow_mut();
        let Some(input) = dev.device_mut::<aero_virtio::devices::input::VirtioInput>() else {
            return 0;
        };
        u32::try_from(input.pending_len()).unwrap_or(u32::MAX)
    }

    /// Rust-only debug helper: number of pending virtio-input mouse events buffered for delivery.
    pub fn virtio_input_mouse_pending_events(&mut self) -> u32 {
        let Some(dev) = self.inner.virtio_input_mouse() else {
            return 0;
        };
        let mut dev = dev.borrow_mut();
        let Some(input) = dev.device_mut::<aero_virtio::devices::input::VirtioInput>() else {
            return 0;
        };
        u32::try_from(input.pending_len()).unwrap_or(u32::MAX)
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod machine_opfs_disk_tests {
    use super::*;

    use wasm_bindgen::JsCast;
    use wasm_bindgen_test::wasm_bindgen_test;

    use aero_storage::VirtualDisk as _;

    fn unique_path(prefix: &str, ext: &str) -> String {
        let now = js_sys::Date::now() as u64;
        let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
        format!("tests/{prefix}-{now:x}-{rand:x}.{ext}")
    }

    fn fill_deterministic(buf: &mut [u8], seed: u32) {
        let mut x = seed;
        for b in buf {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *b = (x & 0xff) as u8;
        }
    }

    fn js_error_to_string(err: &JsValue) -> String {
        err.as_string()
            .or_else(|| {
                err.dyn_ref::<js_sys::Error>()
                    .and_then(|e| e.message().as_string())
            })
            .unwrap_or_else(|| format!("{err:?}"))
    }

    fn should_skip_opfs(msg: &str) -> bool {
        // `aero_storage::DiskError` `Display` strings.
        msg.contains("backend not supported")
            || msg.contains("backend unavailable")
            || msg.contains("storage quota exceeded")
    }

    #[wasm_bindgen_test(async)]
    async fn machine_disk_aerospar_opfs_roundtrip() {
        let path = unique_path("machine-aerospar", "aerospar");

        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
        if let Err(err) = m
            .set_disk_aerospar_opfs_create(path.clone(), 1024 * 1024, 32 * 1024)
            .await
        {
            let msg = js_error_to_string(&err);
            if should_skip_opfs(&msg) {
                return;
            }
            panic!("set_disk_aerospar_opfs_create failed: {msg}");
        }

        let mut write_buf = vec![0u8; 4096];
        fill_deterministic(&mut write_buf, 0x1234_5678);

        let mut disk = m.inner.shared_disk();
        disk.write_sectors(7, &write_buf).unwrap();
        disk.flush().unwrap();
        drop(m);

        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
        if let Err(err) = m.set_disk_aerospar_opfs_open(path.clone()).await {
            let msg = js_error_to_string(&err);
            if should_skip_opfs(&msg) {
                return;
            }
            panic!("set_disk_aerospar_opfs_open failed: {msg}");
        }

        let mut disk = m.inner.shared_disk();
        let mut read_buf = vec![0u8; write_buf.len()];
        disk.read_sectors(7, &mut read_buf).unwrap();
        assert_eq!(read_buf, write_buf);
    }

    #[wasm_bindgen_test(async)]
    async fn machine_disk_cow_opfs_roundtrip() {
        let base_path = unique_path("machine-cow-base", "img");
        let overlay_path = unique_path("machine-cow-overlay", "aerospar");
        let size_bytes = 1024 * 1024u64;

        // Create a small base disk image. If OPFS sync access handles are unavailable, verify the
        // Machine API reports NotSupported and skip.
        let storage = match aero_opfs::OpfsByteStorage::open(&base_path, true).await {
            Ok(s) => s,
            Err(aero_opfs::DiskError::NotSupported(_))
            | Err(aero_opfs::DiskError::BackendUnavailable) => {
                let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
                let err = m
                    .set_disk_cow_opfs_create(base_path.clone(), overlay_path.clone(), 32 * 1024)
                    .await
                    .expect_err("expected NotSupported when OPFS sync access handles unavailable");
                let msg = js_error_to_string(&err);
                assert!(
                    should_skip_opfs(&msg),
                    "expected NotSupported/BackendUnavailable error, got: {msg}"
                );
                return;
            }
            Err(aero_opfs::DiskError::QuotaExceeded) => return,
            Err(e) => panic!("OpfsByteStorage::open failed: {e:?}"),
        };

        let mut base_disk = aero_storage::RawDisk::create(storage, size_bytes).unwrap();
        let mut base_contents = vec![0u8; 4096];
        fill_deterministic(&mut base_contents, 0x1111_2222);
        base_disk.write_sectors(7, &base_contents).unwrap();
        base_disk.flush().unwrap();
        drop(base_disk);

        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
        if let Err(err) = m
            .set_disk_cow_opfs_create(base_path.clone(), overlay_path.clone(), 32 * 1024)
            .await
        {
            let msg = js_error_to_string(&err);
            if should_skip_opfs(&msg) {
                return;
            }
            panic!("set_disk_cow_opfs_create failed: {msg}");
        }

        let mut overlay_contents = vec![0u8; 4096];
        fill_deterministic(&mut overlay_contents, 0x3333_4444);

        let mut disk = m.inner.shared_disk();
        disk.write_sectors(7, &overlay_contents).unwrap();
        disk.flush().unwrap();
        drop(m);

        // Base disk should remain unchanged (COW writes go to the overlay).
        let base_storage = aero_opfs::OpfsByteStorage::open(&base_path, false)
            .await
            .unwrap();
        let mut base_disk = aero_storage::DiskImage::open_auto(base_storage).unwrap();
        let mut read_base = vec![0u8; base_contents.len()];
        base_disk.read_sectors(7, &mut read_base).unwrap();
        assert_eq!(read_base, base_contents);
        drop(base_disk);

        // Reopen overlay and verify the data persisted.
        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
        if let Err(err) = m
            .set_disk_cow_opfs_open(base_path.clone(), overlay_path.clone())
            .await
        {
            let msg = js_error_to_string(&err);
            if should_skip_opfs(&msg) {
                return;
            }
            panic!("set_disk_cow_opfs_open failed: {msg}");
        }

        let mut disk = m.inner.shared_disk();
        let mut read_overlay = vec![0u8; overlay_contents.len()];
        disk.read_sectors(7, &mut read_overlay).unwrap();
        assert_eq!(read_overlay, overlay_contents);
    }

    #[wasm_bindgen_test(async)]
    async fn machine_disk_aerospar_opfs_create_rejects_invalid_block_size() {
        let path = unique_path("machine-aerospar-invalid-block", "aerospar");

        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");

        // 24KiB is a multiple of 512 but *not* a power of two; it should be rejected by the
        // AeroSparse header validator.
        let res = m
            .set_disk_aerospar_opfs_create(path, 1024 * 1024, 24 * 1024)
            .await;

        match res {
            Ok(()) => panic!("expected invalid block size to be rejected"),
            Err(err) => {
                let msg = js_error_to_string(&err);
                if should_skip_opfs(&msg) {
                    return;
                }
                assert!(
                    msg.contains("block_size") && msg.contains("power of two"),
                    "unexpected error message: {msg}"
                );
            }
        }
    }

    #[wasm_bindgen_test(async)]
    async fn machine_disk_cow_opfs_open_rejects_overlay_size_mismatch() {
        let base_path = unique_path("machine-cow-base-mismatch", "img");
        let overlay_path = unique_path("machine-cow-overlay-mismatch", "aerospar");
        let size_bytes = 1024 * 1024u64;

        // Create a small raw base disk.
        let base_storage = match aero_opfs::OpfsByteStorage::open(&base_path, true).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if should_skip_opfs(&msg) {
                    return;
                }
                panic!("OpfsByteStorage::open(base) failed unexpectedly: {msg}");
            }
        };
        let mut base_disk = aero_storage::RawDisk::create(base_storage, size_bytes).unwrap();
        base_disk.flush().unwrap();
        drop(base_disk);

        // Create an overlay with a *different* virtual disk size.
        let overlay_storage = match aero_opfs::OpfsByteStorage::open(&overlay_path, true).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                if should_skip_opfs(&msg) {
                    return;
                }
                panic!("OpfsByteStorage::open(overlay) failed unexpectedly: {msg}");
            }
        };
        let mut overlay_disk = aero_storage::AeroSparseDisk::create(
            overlay_storage,
            aero_storage::AeroSparseConfig {
                disk_size_bytes: size_bytes * 2,
                block_size_bytes: 32 * 1024,
            },
        )
        .unwrap();
        overlay_disk.flush().unwrap();
        drop(overlay_disk);

        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new");
        let res = m.set_disk_cow_opfs_open(base_path, overlay_path).await;

        match res {
            Ok(()) => panic!("expected overlay size mismatch to be rejected"),
            Err(err) => {
                let msg = js_error_to_string(&err);
                if should_skip_opfs(&msg) {
                    return;
                }
                assert!(
                    msg.contains("overlay size does not match base disk size"),
                    "unexpected error message: {msg}"
                );
            }
        }
    }
}

#[cfg(test)]
mod machine_primary_hdd_cow_disk_tests {
    use super::*;

    use aero_storage::{MemBackend, RawDisk, SECTOR_SIZE, VirtualDisk};

    #[test]
    fn creates_overlay_then_reopens_without_mutating_base() {
        let base_size = 1024 * 1024u64;

        let mut base_bytes = vec![0u8; base_size as usize];
        base_bytes[..SECTOR_SIZE].fill(0xAA);

        let base_disk =
            RawDisk::open(MemBackend::from_vec(base_bytes)).expect("RawDisk::open base");
        let overlay_backend = MemBackend::new();

        let mut cow =
            open_or_create_cow_disk(base_disk, overlay_backend, 4096).expect("create cow disk");

        // Reads should come from the base until a block is allocated in the overlay.
        let mut sector = [0u8; SECTOR_SIZE];
        cow.read_sectors(0, &mut sector).unwrap();
        assert_eq!(sector, [0xAA; SECTOR_SIZE]);

        // Writes should land in the overlay (and preserve the base bytes).
        sector.fill(0x55);
        cow.write_sectors(0, &sector).unwrap();
        cow.flush().unwrap();

        let (base, overlay) = cow.into_parts();
        let base_bytes_after = base.into_backend().into_vec();
        assert_eq!(&base_bytes_after[..SECTOR_SIZE], &[0xAA; SECTOR_SIZE]);

        // Reopen from the persisted overlay bytes and verify the overlay contents are visible.
        let overlay_bytes = overlay.into_backend().into_vec();
        let base_disk2 =
            RawDisk::open(MemBackend::from_vec(base_bytes_after)).expect("RawDisk::open base2");
        let overlay_backend2 = MemBackend::from_vec(overlay_bytes);
        let mut cow2 =
            open_or_create_cow_disk(base_disk2, overlay_backend2, 0).expect("open cow disk");
        let mut read = [0u8; SECTOR_SIZE];
        cow2.read_sectors(0, &mut read).unwrap();
        assert_eq!(read, [0x55; SECTOR_SIZE]);
    }

    #[test]
    fn open_rejects_block_size_mismatch() {
        let base_size = 2 * 1024 * 1024u64;

        let base_disk =
            RawDisk::open(MemBackend::with_len(base_size).unwrap()).expect("RawDisk::open base");
        let overlay_backend = MemBackend::new();
        let cow = open_or_create_cow_disk(base_disk, overlay_backend, 4096).unwrap();

        let (_base, overlay) = cow.into_parts();
        let overlay_bytes = overlay.into_backend().into_vec();

        let base_disk2 =
            RawDisk::open(MemBackend::with_len(base_size).unwrap()).expect("RawDisk::open base2");
        let overlay_backend2 = MemBackend::from_vec(overlay_bytes);
        let err = open_or_create_cow_disk(base_disk2, overlay_backend2, 8192)
            .err()
            .expect("expected block size mismatch error");
        assert!(
            err.to_string().contains("block_size_bytes"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn create_rejects_zero_block_size() {
        let base_size = 1024 * 1024u64;
        let base_disk =
            RawDisk::open(MemBackend::with_len(base_size).unwrap()).expect("RawDisk::open base");
        let overlay_backend = MemBackend::new();
        let err = open_or_create_cow_disk(base_disk, overlay_backend, 0)
            .err()
            .expect("expected missing block size error");
        assert!(
            err.to_string().contains("overlay_block_size_bytes"),
            "unexpected error: {err}"
        );
    }
}

#[cfg(test)]
mod machine_mouse_button_cache_tests {
    use super::*;

    #[test]
    fn mouse_button_cache_tracks_transitions_and_invalidates_on_snapshot_restore() {
        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // New machines start with a known "all released" state.
        assert!(m.mouse_buttons_known);
        assert_eq!(m.mouse_buttons, 0);

        // Individual helpers update the cache.
        m.inject_mouse_left(true);
        assert!(m.mouse_buttons_known);
        assert_eq!(m.mouse_buttons, 0x01);

        m.inject_mouse_right(true);
        assert_eq!(m.mouse_buttons, 0x03);

        m.inject_mouse_left(false);
        assert_eq!(m.mouse_buttons, 0x02);

        m.inject_mouse_back(true);
        assert_eq!(m.mouse_buttons, 0x0a);

        m.inject_mouse_forward(true);
        assert_eq!(m.mouse_buttons, 0x1a);

        m.inject_mouse_back(false);
        assert_eq!(m.mouse_buttons, 0x12);

        // Absolute mask injection should update the cache as well.
        m.inject_mouse_buttons_mask(0x1f);
        assert!(m.mouse_buttons_known);
        assert_eq!(m.mouse_buttons, 0x1f);

        // Taking/restoring a snapshot should invalidate the cache because device state rewinds.
        let snap = m.snapshot_full().expect("snapshot_full ok");
        m.inject_mouse_buttons_mask(0x00);
        assert_eq!(m.mouse_buttons, 0x00);

        m.restore_snapshot(&snap).expect("restore_snapshot ok");
        assert!(!m.mouse_buttons_known);

        // Individual transitions should not flip the state back to "known" (we only know the state
        // of the specific button we touched; other buttons may differ inside the restored guest).
        m.inject_mouse_right(true);
        assert!(!m.mouse_buttons_known);

        // The next mask call should re-establish a known cache.
        m.inject_mouse_buttons_mask(0x01);
        assert!(m.mouse_buttons_known);
        assert_eq!(m.mouse_buttons, 0x01);

        // Reset should also bring us back to a known released state.
        m.reset();
        assert!(m.mouse_buttons_known);
        assert_eq!(m.mouse_buttons, 0x00);
    }

    #[test]
    fn mouse_buttons_mask_resyncs_after_guest_mouse_reset() {
        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Enable mouse reporting so button injections generate stream packets.
        m.inner.io_write(0x64, 1, 0xD4);
        m.inner.io_write(0x60, 1, 0xF4);
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xFA); // ACK

        // Set left pressed; this should generate a packet.
        m.inject_mouse_buttons_mask(0x01);
        let pressed_packet: Vec<u8> = (0..3).map(|_| m.inner.io_read(0x60, 1) as u8).collect();
        assert_eq!(pressed_packet, vec![0x09, 0x00, 0x00]);

        // Guest resets the mouse (D4 FF). This clears the device-side button image.
        m.inner.io_write(0x64, 1, 0xD4);
        m.inner.io_write(0x60, 1, 0xFF);
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xFA); // ACK
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xAA); // self-test pass
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0x00); // device id

        // Re-enable reporting after reset (D4 F4).
        m.inner.io_write(0x64, 1, 0xD4);
        m.inner.io_write(0x60, 1, 0xF4);
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xFA); // ACK

        // Re-apply the same absolute button mask. This should not be a no-op: the device state was
        // reset, so we expect a new packet with left pressed.
        m.inject_mouse_buttons_mask(0x01);
        let packet: Vec<u8> = (0..3).map(|_| m.inner.io_read(0x60, 1) as u8).collect();
        assert_eq!(packet, vec![0x09, 0x00, 0x00]);
    }

    #[test]
    fn inject_mouse_buttons_mask_encodes_back_forward_bits_when_intellimouse_explorer_enabled() {
        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        fn write_mouse_byte(m: &mut Machine, byte: u8) {
            m.inner.io_write(0x64, 1, 0xD4);
            m.inner.io_write(0x60, 1, u32::from(byte));
            assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xFA);
        }

        // Enable IntelliMouse Explorer (5-button) extension: 200, 200, 80.
        write_mouse_byte(&mut m, 0xF3);
        write_mouse_byte(&mut m, 200);
        write_mouse_byte(&mut m, 0xF3);
        write_mouse_byte(&mut m, 200);
        write_mouse_byte(&mut m, 0xF3);
        write_mouse_byte(&mut m, 80);

        // Verify the guest-visible device id.
        m.inner.io_write(0x64, 1, 0xD4);
        m.inner.io_write(0x60, 1, 0xF2);
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0xFA);
        assert_eq!(m.inner.io_read(0x60, 1) as u8, 0x04);

        // Enable reporting.
        write_mouse_byte(&mut m, 0xF4);

        // Back button (DOM bit3) should set bit4 in the fourth PS/2 packet byte.
        m.inject_mouse_buttons_mask(0x08);
        let packet: Vec<u8> = (0..4).map(|_| m.inner.io_read(0x60, 1) as u8).collect();
        assert_eq!(packet, vec![0x08, 0x00, 0x00, 0x10]);

        // Forward button (DOM bit4) should set bit5 (and preserve bit4 while back is held).
        m.inject_mouse_buttons_mask(0x18);
        let packet: Vec<u8> = (0..4).map(|_| m.inner.io_read(0x60, 1) as u8).collect();
        assert_eq!(packet, vec![0x08, 0x00, 0x00, 0x30]);
    }
}

#[cfg(test)]
mod machine_cpu_count_tests {
    use super::Machine;

    #[test]
    fn new_with_cpu_count_rejects_zero() {
        let res = Machine::new_with_cpu_count(16 * 1024 * 1024, 0);
        assert!(res.is_err(), "expected cpu_count=0 to be rejected");
    }

    #[test]
    fn new_with_cpu_count_rejects_too_large() {
        let res = Machine::new_with_cpu_count(16 * 1024 * 1024, 256);
        assert!(res.is_err(), "expected cpu_count=256 to be rejected");
    }

    #[test]
    fn new_with_cpu_count_sets_config() {
        // `aero_machine::Machine::read_lapic_u32` asserts that `cpu_index < cfg.cpu_count`.
        let mut m = Machine::new_with_cpu_count(16 * 1024 * 1024, 2)
            .expect("Machine::new_with_cpu_count should succeed");
        let _ = m.inner.read_lapic_u32(1, 0);
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod machine_opfs_ide_primary_master_tests {
    use super::{Machine, storage_capabilities};

    use js_sys::Reflect;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn unique_path(prefix: &str) -> String {
        let now = js_sys::Date::now() as u64;
        let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
        format!("tests/{prefix}-{now:x}-{rand:x}.img")
    }

    #[wasm_bindgen_test(async)]
    async fn attach_ide_primary_master_disk_opfs_apis_compile_and_skip_when_unsupported() {
        let existing_path = unique_path("ide-primary-master-existing");
        let create_path = unique_path("ide-primary-master-create");
        let size_bytes = 1024 * 1024u64;

        // Seed an on-disk file so we can exercise the `_existing` API when OPFS sync handles are
        // supported. Node-based wasm-bindgen tests (and browsers without OPFS) will hit the
        // `NotSupported` path; treat that as a runtime skip.
        let mut seed = match aero_opfs::OpfsBackend::open(&existing_path, true, size_bytes).await {
            Ok(backend) => backend,
            Err(aero_opfs::DiskError::NotSupported(_))
            | Err(aero_opfs::DiskError::BackendUnavailable)
            | Err(aero_opfs::DiskError::QuotaExceeded) => return,
            Err(e) => panic!("unexpected OPFS error while seeding disk: {e:?}"),
        };
        seed.close().expect("seed disk close should succeed");

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        m.attach_ide_primary_master_disk_opfs_existing(existing_path)
            .await
            .expect("attach_ide_primary_master_disk_opfs_existing should succeed");

        m.attach_ide_primary_master_disk_opfs(create_path, true, size_bytes)
            .await
            .expect("attach_ide_primary_master_disk_opfs should succeed");
    }

    #[wasm_bindgen_test(async)]
    async fn opfs_attach_returns_actionable_js_error_when_sync_handles_unavailable() {
        // If OPFS sync access handles are supported, skip: this test is specifically asserting the
        // error mapping when the environment *cannot* provide FileSystemSyncAccessHandle (e.g.
        // Node, main thread, non-Chromium browsers).
        let caps = storage_capabilities();
        let bool_field = |key: &str| -> bool {
            Reflect::get(&caps, &JsValue::from_str(key))
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };
        let opfs_supported = bool_field("opfsSupported");
        let opfs_sync_supported = bool_field("opfsSyncAccessSupported");
        let is_worker_scope = bool_field("isWorkerScope");
        if opfs_supported && opfs_sync_supported && is_worker_scope {
            return;
        }

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        let err = m
            .attach_ide_primary_master_disk_opfs_existing("tests/nonexistent.img".to_string())
            .await
            .expect_err("expected OPFS attach to fail when sync handles are unavailable");

        assert!(
            err.is_instance_of::<js_sys::Error>(),
            "expected a JS Error; got {err:?}"
        );

        let e: js_sys::Error = err
            .dyn_into()
            .expect("error JsValue should be a js_sys::Error");
        let msg: String = e.message().into();
        assert!(
            msg.contains("DedicatedWorker"),
            "error message should mention DedicatedWorker; got: {msg}"
        );
        assert!(
            msg.contains("storage_capabilities"),
            "error message should reference storage_capabilities(); got: {msg}"
        );
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod reattach_restored_disks_from_opfs_tests {
    use super::*;

    use aero_opfs::{DiskError, OpfsByteStorage};
    use aero_snapshot::{DiskOverlayRef, DiskOverlayRefs, SnapshotTarget as _};
    use aero_storage::VirtualDisk as _;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn unique_path(prefix: &str, ext: &str) -> String {
        let now = js_sys::Date::now() as u64;
        let rand = (js_sys::Math::random() * 1_000_000.0) as u64;
        format!("tests/{prefix}-{now:x}-{rand:x}.{ext}")
    }

    async fn create_raw_file(path: &str, size: u64) -> Result<(), DiskError> {
        let mut storage = OpfsByteStorage::open(path, true).await?;
        storage.set_len(size)?;
        storage.write_at(0, &[0xA5, 0x5A, 0x01, 0x02])?;
        storage.flush()?;
        storage.close()?;
        Ok(())
    }

    async fn create_sparse_overlay(path: &str, disk_size_bytes: u64) -> Result<(), DiskError> {
        let storage = OpfsByteStorage::open(path, true).await?;
        let mut disk = aero_storage::AeroSparseDisk::create(
            storage,
            aero_storage::AeroSparseConfig {
                disk_size_bytes,
                block_size_bytes: 512,
            },
        )?;
        disk.flush()?;

        let mut backend = disk.into_backend();
        backend.close()?;
        Ok(())
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_with_no_restored_overlays_is_ok() {
        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");
        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should be a no-op when no restore overlays exist");
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_restored_install_media_from_opfs_is_ok_when_opfs_available() {
        let iso1 = unique_path("reattach-iso1-only", "iso");
        // ISO for disk_id=1 (install media). Must be a multiple of 2048 bytes.
        match create_raw_file(&iso1, 2048).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({iso1:?}) failed: {e:?}"),
        }

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Simulate snapshot restore populating `restored_disk_overlays`.
        m.inner.restore_disk_overlays(DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: aero_machine::Machine::DISK_ID_INSTALL_MEDIA,
                base_image: iso1.clone(),
                overlay_image: String::new(),
            }],
        });

        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should succeed when referenced OPFS ISO exists");
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_restored_primary_hdd_base_only_from_opfs_is_ok_when_opfs_available() {
        // Creating OPFS sync access handles is worker-only and not universally available in all
        // wasm-bindgen-test runtimes. Skip gracefully when unsupported.
        let base0 = unique_path("reattach-base0-base-only", "img");
        let disk_size = 4096u64;
        match create_raw_file(&base0, disk_size).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({base0:?}) failed: {e:?}"),
        }

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Simulate snapshot restore populating `restored_disk_overlays` for the canonical primary HDD.
        m.inner.restore_disk_overlays(DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: aero_machine::Machine::DISK_ID_PRIMARY_HDD,
                base_image: base0.clone(),
                overlay_image: String::new(),
            }],
        });

        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should succeed when referenced OPFS base disk exists");

        // Ensure the shared disk backend now reflects the reattached disk bytes.
        let mut disk = m.inner.shared_disk();
        assert_eq!(disk.capacity_bytes(), disk_size);
        let mut buf = [0u8; 4];
        disk.read_at(0, &mut buf)
            .expect("read from reattached shared disk should succeed");
        assert_eq!(buf, [0xA5, 0x5A, 0x01, 0x02]);
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_restored_primary_hdd_is_ok_when_disk_already_open() {
        // Creating OPFS sync access handles is worker-only and not universally available in all
        // wasm-bindgen-test runtimes. Skip gracefully when unsupported.
        let base0 = unique_path("reattach-base0-already-open", "img");
        // Use a 1-sector disk to ensure the "already open" path does not depend on heuristics based
        // on disk capacity (the default shared disk is also 1 sector, but it is not OPFS-backed).
        let disk_size = 512u64;
        match create_raw_file(&base0, disk_size).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({base0:?}) failed: {e:?}"),
        }

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Attach the base disk directly so the machine holds an open sync access handle.
        let base_backend = match OpfsByteStorage::open(&base0, false).await {
            Ok(backend) => backend,
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("OpfsByteStorage::open({base0:?}) failed: {e:?}"),
        };
        let base_disk = aero_storage::DiskImage::open_auto(base_backend)
            .expect("DiskImage::open_auto(raw base) should succeed");
        m.inner
            .set_disk_backend(Box::new(base_disk))
            .expect("set_disk_backend should succeed");
        m.inner.set_ahci_port0_disk_overlay_ref(base0.clone(), "");

        let ahci = m.inner.ahci().expect("browser_defaults enables AHCI");
        assert!(
            ahci.borrow().drive_attached(0),
            "expected AHCI port0 to have a drive attached before snapshot"
        );

        // Snapshot + restore on the same machine instance should clear the AHCI `AtaDrive` while
        // keeping the underlying shared disk backend alive (the snapshot does not serialize the
        // OPFS handle).
        let snap = m
            .inner
            .take_snapshot_full()
            .expect("take_snapshot_full should succeed");
        m.inner
            .restore_snapshot_bytes(&snap)
            .expect("restore_snapshot_bytes should succeed");

        assert!(
            !ahci.borrow().drive_attached(0),
            "expected snapshot restore to clear the AHCI drive backend"
        );

        // Reattach must not try to reopen the already-open base image (OPFS handles are exclusive);
        // it should instead reattach the existing shared disk to AHCI.
        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should succeed when base image is already open");

        assert!(
            ahci.borrow().drive_attached(0),
            "expected AHCI port0 drive to be reattached after helper call"
        );

        // Sanity check that the shared disk still reflects the on-disk contents.
        let mut disk = m.inner.shared_disk();
        assert_eq!(disk.capacity_bytes(), disk_size);
        let mut buf = [0u8; 4];
        disk.read_at(0, &mut buf)
            .expect("read from reattached shared disk should succeed");
        assert_eq!(buf, [0xA5, 0x5A, 0x01, 0x02]);
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_restored_install_media_is_skipped_when_media_not_present() {
        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Ensure the guest-visible ATAPI state reports no media present. This simulates a guest-
        // initiated eject (START STOP UNIT) without clearing host overlay refs.
        m.inner.detach_ide_secondary_master_iso();
        assert!(
            !m.inner.install_media_is_inserted(),
            "expected install media to report not inserted"
        );

        // Simulate snapshot restore populating `restored_disk_overlays` with a stale base_image ref.
        // If `reattach_restored_disks_from_opfs` tried to open this path, it would fail.
        m.inner.restore_disk_overlays(DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: aero_machine::Machine::DISK_ID_INSTALL_MEDIA,
                base_image: "tests/nonexistent-install-media.iso".to_string(),
                overlay_image: String::new(),
            }],
        });

        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should be a no-op when install media is not present");
    }

    #[wasm_bindgen_test(async)]
    async fn reattach_restored_disks_from_opfs_attaches_when_opfs_available() {
        // Creating OPFS sync access handles is worker-only and not universally available in all
        // wasm-bindgen-test runtimes. Skip gracefully when unsupported.
        let base0 = unique_path("reattach-base0", "img");
        let overlay0 = unique_path("reattach-overlay0", "aerospar");
        let base2 = unique_path("reattach-base2", "img");
        let iso1 = unique_path("reattach-iso1", "iso");

        let disk_size = 4096u64;

        // Base disk for disk_id=0 (primary HDD).
        match create_raw_file(&base0, disk_size).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({base0:?}) failed: {e:?}"),
        }

        // Sparse overlay for disk_id=0.
        match create_sparse_overlay(&overlay0, disk_size).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_sparse_overlay({overlay0:?}) failed: {e:?}"),
        }

        // Base disk for disk_id=2 (IDE primary master).
        match create_raw_file(&base2, disk_size).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({base2:?}) failed: {e:?}"),
        }

        // ISO for disk_id=1 (install media). Must be a multiple of 2048 bytes.
        match create_raw_file(&iso1, 2048).await {
            Ok(()) => {}
            Err(DiskError::NotSupported(_)) | Err(DiskError::BackendUnavailable) => return,
            Err(DiskError::QuotaExceeded) => return,
            Err(e) => panic!("create_raw_file({iso1:?}) failed: {e:?}"),
        }

        let mut m = Machine::new(16 * 1024 * 1024).expect("Machine::new should succeed");

        // Simulate snapshot restore populating `restored_disk_overlays`.
        m.inner.restore_disk_overlays(DiskOverlayRefs {
            disks: vec![
                DiskOverlayRef {
                    disk_id: aero_machine::Machine::DISK_ID_PRIMARY_HDD,
                    base_image: base0.clone(),
                    overlay_image: overlay0.clone(),
                },
                DiskOverlayRef {
                    disk_id: aero_machine::Machine::DISK_ID_INSTALL_MEDIA,
                    base_image: iso1.clone(),
                    overlay_image: String::new(),
                },
                DiskOverlayRef {
                    disk_id: aero_machine::Machine::DISK_ID_IDE_PRIMARY_MASTER,
                    base_image: base2.clone(),
                    overlay_image: String::new(),
                },
            ],
        });

        m.reattach_restored_disks_from_opfs()
            .await
            .expect("reattach should succeed when referenced OPFS files exist");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod aerogpu_defaults_tests {
    use super::*;

    use aero_devices::pci::PciBdf;
    use aero_devices::pci::profile::{PCI_DEVICE_ID_AERO_AEROGPU, PCI_VENDOR_ID_AERO};

    #[test]
    fn machine_new_exposes_aerogpu_pci_identity_at_00_07_0() {
        let mut m = Machine::new(2 * 1024 * 1024).expect("Machine::new should succeed");

        // Verify the PCI device identity directly through the in-memory PCI bus view.
        let (vendor, device) = {
            let pci_cfg = m
                .inner
                .pci_config_ports()
                .expect("canonical Machine::new enables the PC platform");
            let mut pci_cfg = pci_cfg.borrow_mut();
            let bus = pci_cfg.bus_mut();

            let bdf = PciBdf::new(0, 0x07, 0);
            let vendor = bus.read_config(bdf, 0x00, 2) as u16;
            let device = bus.read_config(bdf, 0x02, 2) as u16;
            (vendor, device)
        };

        assert_eq!(vendor, PCI_VENDOR_ID_AERO);
        assert_eq!(device, PCI_DEVICE_ID_AERO_AEROGPU);

        // Also verify the I/O port based config mechanism (#1) path that the wasm-facing helper
        // uses.
        let id = m.pci_config_read_u32(0, 0x07, 0, 0);
        assert_eq!(id & 0xFFFF, u32::from(PCI_VENDOR_ID_AERO));
        assert_eq!((id >> 16) & 0xFFFF, u32::from(PCI_DEVICE_ID_AERO_AEROGPU));
    }
}
