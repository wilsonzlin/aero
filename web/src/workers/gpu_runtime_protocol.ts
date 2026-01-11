import type { PresenterBackendKind, PresenterInitOptions } from "../gpu/presenter";

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
   * Optional dynamic module URL that exports `present()` (and optionally
   * `get_frame_timings()`).
   *
   * When provided, the worker will call into this module instead of using the
   * built-in presenter backends.
   */
  wasmModuleUrl?: string;
};

export type GpuRuntimeInitMessage = {
  type: "init";
  canvas?: OffscreenCanvas;
  sharedFrameState: SharedArrayBuffer;
  sharedFramebuffer: SharedArrayBuffer;
  sharedFramebufferOffsetBytes: number;
  options?: GpuRuntimeInitOptions;
};

export type GpuRuntimeResizeMessage = {
  type: "resize";
  width: number;
  height: number;
  dpr: number;
};

export type GpuRuntimeTickMessage = {
  type: "tick";
  frameTimeMs: number;
};

export type GpuRuntimeScreenshotRequestMessage = {
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

export type GpuRuntimeCursorSetImageMessage = {
  type: "cursor_set_image";
  width: number;
  height: number;
  rgba8: ArrayBuffer;
};

export type GpuRuntimeCursorSetStateMessage = {
  type: "cursor_set_state";
  enabled: boolean;
  x: number;
  y: number;
  hotX: number;
  hotY: number;
};

export type GpuRuntimeSubmitAerogpuMessage = {
  type: "submit_aerogpu";
  requestId: number;
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
   * Optional allocation table bytes (reserved for future guest-memory backing).
   */
  allocTable?: ArrayBuffer;
};

export type GpuRuntimeShutdownMessage = { type: "shutdown" };

export type GpuRuntimeInMessage =
  | GpuRuntimeInitMessage
  | GpuRuntimeResizeMessage
  | GpuRuntimeTickMessage
  | GpuRuntimeScreenshotRequestMessage
  | GpuRuntimeCursorSetImageMessage
  | GpuRuntimeCursorSetStateMessage
  | GpuRuntimeSubmitAerogpuMessage
  | GpuRuntimeShutdownMessage;

export type GpuRuntimeFallbackInfo = {
  from: PresenterBackendKind;
  to: PresenterBackendKind;
  reason: string;
  originalErrorMessage?: string;
};

export type GpuRuntimeReadyMessage = {
  type: "ready";
  backendKind: PresenterBackendKind | "headless";
  fallback?: GpuRuntimeFallbackInfo;
};

export type GpuRuntimeMetricsMessage = {
  type: "metrics";
  framesReceived: number;
  framesPresented: number;
  framesDropped: number;
  telemetry?: unknown;
};

export type GpuRuntimeErrorMessage = {
  type: "error";
  message: string;
  code?: string;
  backend?: PresenterBackendKind;
};

export type GpuRuntimeScreenshotResponseMessage = {
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

export type GpuRuntimeSubmitCompleteMessage = {
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

export type GpuRuntimeStatsMessage = {
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

export type GpuRuntimeEventsMessage = {
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
