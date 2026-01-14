import type { PresenterBackendKind, PresenterInitOptions } from "../gpu/presenter";

/**
 * Canonical postMessage protocol between the main thread and the GPU worker.
 *
 * This file intentionally replaces:
 * - `web/src/ipc/gpu-messages.ts` (legacy wasm presenter types)
 * - `web/src/shared/frameProtocol.ts` (frame pacing SharedArrayBuffer layout)
 * - `web/src/workers/gpu_runtime_protocol.ts` (runtime worker messages)
 *
 * Keep the protocol explicitly versioned so we can evolve it without requiring
 * lockstep changes across all callers (Playwright harnesses, demos, etc).
 */

export const GPU_PROTOCOL_NAME = "aero.gpu" as const;
export const GPU_PROTOCOL_VERSION = 1 as const;

export type GpuProtocolVersion = typeof GPU_PROTOCOL_VERSION;

export type GpuWorkerMessageBase = {
  protocol: typeof GPU_PROTOCOL_NAME;
  protocolVersion: GpuProtocolVersion;
};

export function isGpuWorkerMessageBase(msg: unknown): msg is GpuWorkerMessageBase {
  if (!msg || typeof msg !== "object") return false;
  const record = msg as Record<string, unknown>;
  return record.protocol === GPU_PROTOCOL_NAME && record.protocolVersion === GPU_PROTOCOL_VERSION;
}

export type BackendKind = "webgpu" | "webgl2";

export interface GpuAdapterInfo {
  vendor?: string;
  renderer?: string;
  description?: string;
}

// -----------------------------------------------------------------------------
// Optional scanout / presentation source telemetry.
// -----------------------------------------------------------------------------

/**
 * Which buffer the GPU worker is currently presenting from.
 *
 * - `framebuffer`: legacy SharedArrayBuffer-backed framebuffer (VGA/VBE path).
 * - `aerogpu`: AeroGPU host-side executor output (ACMD).
 * - `wddm_scanout`: a WDDM-programmed scanout buffer selected via `ScanoutState` (future/optional).
 */
export type GpuRuntimeOutputSource = "framebuffer" | "aerogpu" | "wddm_scanout";

/**
 * Best-effort snapshot of the shared `ScanoutState`.
 *
 * Notes:
 * - `base_paddr` is stringified (hex) to preserve full u64 precision without requiring BigInt
 *   handling in all telemetry consumers.
 */
export type GpuRuntimeScanoutSnapshotV1 = {
  source: number;
  base_paddr: string;
  width: number;
  height: number;
  pitchBytes: number;
  format: number;
  generation: number;
};

export type GpuRuntimePresentUploadV1 = {
  /**
   * How the presenter uploaded pixels for the most recent present.
   *
   * - `none`: no upload occurred (e.g. reusing the previous output).
   * - `full`: full-frame upload.
   * - `dirty_rects`: uploaded only dirty rectangles.
   */
  kind: "none" | "full" | "dirty_rects";
  dirtyRectCount?: number;
};

export interface FrameTimingsReport {
  frame_index: number;
  backend: BackendKind;
  cpu_encode_us: number;
  cpu_submit_us: number;
  gpu_us?: number;
}

/**
 * Init options used by the legacy `aero-gpu` wasm presenter shim.
 *
 * The runtime uses the richer `GpuRuntimeInitOptions` below.
 */
export interface GpuWorkerInitOptions {
  preferWebGpu?: boolean;
  disableWebGpu?: boolean;
  requiredFeatures?: string[];
}

// -----------------------------------------------------------------------------
// Shared frame pacing state (SharedArrayBuffer + Atomics).
// -----------------------------------------------------------------------------

export const FRAME_STATUS_INDEX = 0;
export const FRAME_SEQ_INDEX = 1;
export const FRAME_METRICS_RECEIVED_INDEX = 2;
export const FRAME_METRICS_PRESENTED_INDEX = 3;
export const FRAME_METRICS_DROPPED_INDEX = 4;

export const FRAME_PRESENTED = 0;
export const FRAME_DIRTY = 1;
export const FRAME_PRESENTING = 2;

// -----------------------------------------------------------------------------
// Runtime GPU worker protocol (main -> worker).
// -----------------------------------------------------------------------------

export type GpuRuntimeInitOptions = {
  /**
   * Force a specific presenter backend.
   *
   * When unset, the worker will try WebGPU first (when allowed) and fall back to
   * WebGL2 (raw) when possible.
   */
  forceBackend?: PresenterBackendKind;

  /**
   * Treat WebGPU as unavailable.
   *
   * Useful for smoke tests and for environments where `navigator.gpu` exists but
   * is not usable.
   */
  disableWebGpu?: boolean;

  /**
   * Hint for backend selection when `forceBackend` is not specified.
   *
   * When false, the worker will prefer the WebGL2 backend first.
   */
  preferWebGpu?: boolean;

  /**
   * Destination canvas sizing (CSS pixels + devicePixelRatio).
   *
   * If unset, defaults to the framebuffer size and dpr=1. The page can still
   * scale the `<canvas>` via CSS.
   */
  outputWidth?: number;
  outputHeight?: number;
  dpr?: number;

  /**
   * Low-level presenter configuration (scaleMode, filter, clearColor, etc).
   *
   * `outputWidth/outputHeight/dpr` fields above take precedence.
   */
  presenter?: PresenterInitOptions;

  /**
   * Optional dynamic module URL that exports `present()` and may optionally export
   * telemetry helpers:
   *
   * - `get_frame_timings()` (optional)
   * - `get_gpu_stats()` / `getGpuStats()` (optional): extra stats for periodic `stats`
   *   messages. Payload may be a JSON string or an object.
   * - `drain_gpu_events()` (optional; also accepts `drain_gpu_error_events()`,
   *   `take_gpu_events()`, etc): error/diagnostic events forwarded as `events`
   *   messages. Payload may be a JSON string, a single object, or an array; the
   *   worker normalizes best-effort.
   *
   * When provided, the worker will call into this module instead of using the
   * built-in presenter backends.
   */
  wasmModuleUrl?: string;
};

export type GpuRuntimeInitMessage = GpuWorkerMessageBase & {
  type: "init";
  canvas?: OffscreenCanvas;
  sharedFrameState: SharedArrayBuffer;
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes: number;
  options?: GpuRuntimeInitOptions;
};

export type GpuRuntimeResizeMessage = GpuWorkerMessageBase & {
  type: "resize";
  width: number;
  height: number;
  dpr: number;
};

export type GpuRuntimeTickMessage = GpuWorkerMessageBase & {
  type: "tick";
  frameTimeMs: number;
};

export type GpuRuntimeScreenshotRequestMessage = GpuWorkerMessageBase & {
  type: "screenshot";
  requestId: number;
  /**
   * Whether the screenshot should include the cursor overlay.
   *
   * Screenshot data is defined as a readback of the *source framebuffer* pixels
   * (deterministic bytes for hashing/tests), not a capture of the presented canvas.
   *
   * When `true`, the worker will composite the current cursor image over the source
   * framebuffer in the returned RGBA8 buffer (best-effort).
   *
   * Default: false (cursor excluded) so screenshot hashing stays deterministic even
   * when the guest is actively moving the hardware cursor.
   */
  includeCursor?: boolean;
};

/**
 * Debug-only: attempt to read back the *presented* pixels (after presentation policy such as
 * scaling/letterboxing, sRGB/alpha handling, and cursor composition).
 *
 * This is distinct from `screenshot`, which returns a deterministic readback of the **source
 * framebuffer** bytes for hashing/tests.
 */
export type GpuRuntimeScreenshotPresentedRequestMessage = GpuWorkerMessageBase & {
  type: "screenshot_presented";
  requestId: number;
  /**
   * Whether the readback should include the cursor overlay.
   *
   * Default: false (cursor excluded) so hashes remain deterministic.
   */
  includeCursor?: boolean;
};

export type GpuRuntimeCursorSetImageMessage = GpuWorkerMessageBase & {
  type: "cursor_set_image";
  width: number;
  height: number;
  rgba8: ArrayBuffer;
};

export type GpuRuntimeCursorSetStateMessage = GpuWorkerMessageBase & {
  type: "cursor_set_state";
  enabled: boolean;
  x: number;
  y: number;
  hotX: number;
  hotY: number;
};

export type GpuRuntimeSubmitAerogpuMessage = GpuWorkerMessageBase & {
  type: "submit_aerogpu";
  requestId: number;
  /**
   * Per-submission context ID (u32).
   *
   * Corresponds to `aerogpu_submit_desc.context_id`.
   *
   * This is required for correctness when multiple guest processes submit
   * interleaved AeroGPU command streams: host-side execution state (resources,
   * bindings, pipeline state, etc) must be isolated per context.
   *
   * Note: the runtime-wide monotonic `presentCount` reported in `submit_complete`
   * is global across all contexts.
   */
  contextId: number;
  /**
   * Guest-provided fence value to report as completed once the submission finishes.
   *
   * Uses BigInt to preserve full u64 fidelity across JS/worker IPC.
   */
  signalFence: bigint;
  /**
   * Raw command stream bytes (includes `aerogpu_cmd_stream_header` / "ACMD" magic).
   *
   * This buffer should be transferred to the worker (`postMessage(..., [cmdStream])`)
   * to avoid an extra copy.
   */
  cmdStream: ArrayBuffer;
  /**
   * Optional `aerogpu_alloc_table` bytes used to resolve `backing_alloc_id` uploads.
   *
   * The table maps `alloc_id -> { gpa, size_bytes }`, where `gpa` is a **guest physical address**.
   *
   * Note: when the configured guest RAM exceeds the PCIe ECAM base (`0xB000_0000`), the PC/Q35
   * E820 layout remaps the "high" portion of RAM above 4 GiB, leaving an ECAM/PCI hole below
   * 4 GiB. The worker must translate guest physical addresses back into the contiguous guest RAM
   * backing store before indexing the `Uint8Array` view supplied via `WorkerInitMessage.guestMemory`.
   *
   * Translation helpers:
   * - `web/src/arch/guest_ram_translate.ts` (raw `ramBytes`-based helpers)
   * - `web/src/runtime/shared_layout.ts` (thin wrappers for `GuestRamLayout`)
   */
  allocTable?: ArrayBuffer;
};

/**
 * Dev-only: best-effort hook for manually simulating a WebGL2 context loss on the
 * raw WebGL2 presenter backend (uses WEBGL_lose_context when available).
 *
 * Production builds may ignore this message.
 */
export type GpuRuntimeDebugContextLossMessage = GpuWorkerMessageBase & {
  type: "debug_context_loss";
  action: "lose" | "restore";
};

export type GpuRuntimeShutdownMessage = GpuWorkerMessageBase & { type: "shutdown" };

export type GpuRuntimeInMessage =
  | GpuRuntimeInitMessage
  | GpuRuntimeResizeMessage
  | GpuRuntimeTickMessage
  | GpuRuntimeScreenshotRequestMessage
  | GpuRuntimeScreenshotPresentedRequestMessage
  | GpuRuntimeCursorSetImageMessage
  | GpuRuntimeCursorSetStateMessage
  | GpuRuntimeSubmitAerogpuMessage
  | GpuRuntimeDebugContextLossMessage
  | GpuRuntimeShutdownMessage;

// -----------------------------------------------------------------------------
// Runtime GPU worker protocol (worker -> main).
// -----------------------------------------------------------------------------

export type GpuRuntimeFallbackInfo = {
  from: PresenterBackendKind;
  to: PresenterBackendKind;
  reason: string;
  originalErrorMessage?: string;
};

export type GpuRuntimeReadyMessage = GpuWorkerMessageBase & {
  type: "ready";
  backendKind: PresenterBackendKind | "headless";
  fallback?: GpuRuntimeFallbackInfo;
};

export type GpuRuntimeMetricsMessage = GpuWorkerMessageBase & {
  type: "metrics";
  framesReceived: number;
  framesPresented: number;
  framesDropped: number;
  telemetry?: unknown;
  /**
   * Optional scanout/presentation diagnostics.
   */
  scanout?: GpuRuntimeScanoutSnapshotV1;
  outputSource?: GpuRuntimeOutputSource;
  presentUpload?: GpuRuntimePresentUploadV1;
};

export type GpuRuntimeErrorMessage = GpuWorkerMessageBase & {
  type: "error";
  message: string;
  code?: string;
  backend?: PresenterBackendKind;
};

export type GpuRuntimeScreenshotResponseMessage = GpuWorkerMessageBase & {
  type: "screenshot";
  requestId: number;
  /**
   * Screenshot dimensions in **source framebuffer** pixels (not presented/canvas pixels).
   */
  width: number;
  height: number;
  /**
   * RGBA8 bytes in row-major order with a top-left origin.
   *
   * Semantics: deterministic readback of the *source framebuffer* content (pre-scaling,
   * pre-sRGB/color-management, etc). This is intentionally not a capture of "what the
   * user sees" on the canvas.
   *
   * If requested with `includeCursor: true`, the cursor overlay is composited over
   * the source framebuffer (best-effort).
   *
   * Buffer is tight-packed: `byteLength === width * height * 4`.
   */
  rgba8: ArrayBuffer;
  origin: "top-left";
  /**
   * Optional producer frame sequence number when the framebuffer layout exposes
   * one.
   */
  frameSeq?: number;
};

export type GpuRuntimeScreenshotPresentedResponseMessage = GpuWorkerMessageBase & {
  type: "screenshot_presented";
  requestId: number;
  /**
   * Screenshot dimensions in **presented/canvas** pixels (post-DPR / output sizing).
   *
   * Note: this is a best-effort debug API; if the selected backend cannot read back
   * presented output yet, the worker may fall back to a source-framebuffer screenshot.
   */
  width: number;
  height: number;
  /**
   * RGBA8 bytes in row-major order with a top-left origin.
   *
   * Semantics: best-effort readback of the **presented output** pixels. This may include
   * scaling/letterboxing, color-space/alpha policy, and cursor composition (if requested).
   *
   * Not suitable for deterministic hashing across browsers/GPUs.
   *
   * Buffer is tight-packed: `byteLength === width * height * 4`.
   */
  rgba8: ArrayBuffer;
  origin: "top-left";
  frameSeq?: number;
};

export type GpuRuntimeSubmitCompleteMessage = GpuWorkerMessageBase & {
  type: "submit_complete";
  requestId: number;
  completedFence: bigint;
  /**
   * Monotonic present counter for the runtime (optional; only reported when the
   * submission contained at least one PRESENT packet).
   */
  presentCount?: bigint;
};

export type GpuRuntimeStatsCountersV1 = {
  presents_attempted: number;
  presents_succeeded: number;
  recoveries_attempted: number;
  recoveries_succeeded: number;
  surface_reconfigures: number;
  /**
   * Best-effort subset of the above counters, scoped to cases where the shared scanout
   * state reported `source=WDDM` when recovery was attempted/succeeded.
   *
   * These counters are purely diagnostic and may be absent in older builds.
   */
  recoveries_attempted_wddm?: number;
  recoveries_succeeded_wddm?: number;
};

export type GpuRuntimeStatsMessage = GpuWorkerMessageBase & {
  type: "stats";
  /**
   * Version tag for forward-compatible telemetry parsing.
   */
  version: 1;
  /**
   * `performance.now()` timestamp captured in the worker.
   */
  timeMs: number;
  backendKind?: PresenterBackendKind | "headless";
  /**
   * Always-present cheap counters maintained by the worker.
   */
  counters: GpuRuntimeStatsCountersV1;
  /**
   * Optional richer stats returned by the WASM runtime (best-effort).
   */
  wasm?: unknown;
  /**
   * Optional scanout/presentation diagnostics.
   */
  scanout?: GpuRuntimeScanoutSnapshotV1;
  outputSource?: GpuRuntimeOutputSource;
  presentUpload?: GpuRuntimePresentUploadV1;
};

export type GpuRuntimeErrorEventSeverity = "info" | "warn" | "error" | "fatal";

export type GpuRuntimeErrorEvent = {
  time_ms: number;
  backend_kind: string;
  severity: GpuRuntimeErrorEventSeverity;
  category: string;
  message: string;
  details?: unknown;
};

export type GpuRuntimeEventsMessage = GpuWorkerMessageBase & {
  type: "events";
  /**
   * Version tag for forward-compatible event parsing.
   */
  version: 1;
  events: GpuRuntimeErrorEvent[];
};

export type GpuRuntimeOutMessage =
  | GpuRuntimeReadyMessage
  | GpuRuntimeMetricsMessage
  | GpuRuntimeErrorMessage
  | GpuRuntimeScreenshotResponseMessage
  | GpuRuntimeScreenshotPresentedResponseMessage
  | GpuRuntimeSubmitCompleteMessage
  | GpuRuntimeStatsMessage
  | GpuRuntimeEventsMessage;
