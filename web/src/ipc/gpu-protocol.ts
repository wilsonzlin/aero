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
   * Default: false (cursor excluded) so screenshot hashing stays deterministic even
   * when the guest is actively moving the hardware cursor.
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
   * The table maps `alloc_id -> { gpa, size_bytes }`, where `gpa` is a guest physical
   * byte offset into the shared guest RAM view supplied to the worker via
   * `WorkerInitMessage.guestMemory`.
   */
  allocTable?: ArrayBuffer;
};

export type GpuRuntimeShutdownMessage = GpuWorkerMessageBase & { type: "shutdown" };

export type GpuRuntimeInMessage =
  | GpuRuntimeInitMessage
  | GpuRuntimeResizeMessage
  | GpuRuntimeTickMessage
  | GpuRuntimeScreenshotRequestMessage
  | GpuRuntimeCursorSetImageMessage
  | GpuRuntimeCursorSetStateMessage
  | GpuRuntimeSubmitAerogpuMessage
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
  width: number;
  height: number;
  rgba8: ArrayBuffer;
  origin: "top-left";
  /**
   * Optional producer frame sequence number when the framebuffer layout exposes
   * one.
   */
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
  | GpuRuntimeSubmitCompleteMessage
  | GpuRuntimeStatsMessage
  | GpuRuntimeEventsMessage;
