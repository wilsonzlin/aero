export type BackendKind = "webgpu" | "webgl2";

export interface FrameTimingsReport {
  frame_index: number;
  backend: BackendKind;
  cpu_encode_us: number;
  cpu_submit_us: number;
  gpu_us?: number;
}

export interface GpuWorkerInitOptions {
  /**
   * Prefer attempting WebGPU first. If WebGPU initialization fails, the worker
   * should fall back to WebGL2 when possible.
   */
  preferWebGpu?: boolean;

  /**
   * Test/debug hook: treat WebGPU as unavailable even if `navigator.gpu` exists.
   *
   * This forces the WebGL2 fallback path and ensures the rest of the worker
   * (message loop, screenshot requests, etc) remains operational.
   */
  disableWebGpu?: boolean;

  /**
   * WebGPU required features (if any). When supplied, the backend may fail to
   * initialize if the adapter cannot satisfy these.
   */
  requiredFeatures?: string[];
}

export interface GpuWorkerInitMessage {
  type: "init";
  canvas: OffscreenCanvas;
  /** CSS pixel width. */
  width: number;
  /** CSS pixel height. */
  height: number;
  devicePixelRatio: number;
  gpuOptions?: GpuWorkerInitOptions;
}

export interface GpuWorkerResizeMessage {
  type: "resize";
  /** CSS pixel width. */
  width: number;
  /** CSS pixel height. */
  height: number;
  devicePixelRatio: number;
}

export interface GpuWorkerPresentTestPatternMessage {
  type: "present_test_pattern";
}

export interface GpuWorkerRequestScreenshotMessage {
  type: "request_screenshot";
  requestId: number;
}

export interface GpuWorkerRequestTimingsMessage {
  type: "request_timings";
  requestId: number;
}

export interface GpuWorkerShutdownMessage {
  type: "shutdown";
}

export type GpuWorkerIncomingMessage =
  | GpuWorkerInitMessage
  | GpuWorkerResizeMessage
  | GpuWorkerPresentTestPatternMessage
  | GpuWorkerRequestScreenshotMessage
  | GpuWorkerRequestTimingsMessage
  | GpuWorkerShutdownMessage;

export interface GpuAdapterInfo {
  vendor?: string;
  renderer?: string;
  description?: string;
}

export interface GpuWorkerReadyMessage {
  type: "ready";
  backendKind: BackendKind;
  capabilities: unknown;
  adapterInfo?: GpuAdapterInfo;
  /**
   * Present when the worker had to fall back from a requested/preferred backend
   * to another backend that successfully initialized.
   */
  fallback?: {
    from: BackendKind;
    to: BackendKind;
    reason: string;
    originalErrorMessage?: string;
  };
}

export interface GpuWorkerScreenshotMessage {
  type: "screenshot";
  requestId: number;
  /** Physical pixel width. */
  width: number;
  /** Physical pixel height. */
  height: number;
  rgba8: ArrayBuffer;
  /**
   * Pixel origin for `rgba8`. Always top-left (row-major, left-to-right, then
   * top-to-bottom).
   */
  origin: "top-left";
}

export interface GpuWorkerTimingsMessage {
  type: "timings";
  requestId: number;
  timings: FrameTimingsReport | null;
}

export type GpuWorkerErrorKind =
  | "wasm_init_failed"
  | "webgpu_not_supported"
  | "webgpu_init_failed"
  | "webgl2_not_supported"
  | "webgl2_init_failed"
  | "unexpected";

export interface GpuWorkerErrorPayload {
  kind: GpuWorkerErrorKind;
  message: string;
  stack?: string;
  hints?: string[];
}

export interface GpuWorkerGpuErrorMessage {
  type: "gpu_error";
  fatal: boolean;
  error: GpuWorkerErrorPayload;
}

// -----------------------------------------------------------------------------
// Structured GPU reliability events (shared with Rust).
// -----------------------------------------------------------------------------

export type GpuErrorSeverity = "Info" | "Warning" | "Error" | "Fatal";

export type GpuErrorCategory =
  | "Init"
  | "DeviceLost"
  | "Surface"
  | "ShaderCompile"
  | "PipelineCreate"
  | "Validation"
  | "OutOfMemory"
  | "Unknown";

export interface GpuErrorEvent {
  time_ms: number;
  backend_kind: BackendKind;
  severity: GpuErrorSeverity;
  category: GpuErrorCategory;
  message: string;
  details?: Record<string, unknown>;
}

export type GpuStats = {
  presents_attempted: number;
  presents_succeeded: number;
  recoveries_attempted: number;
  recoveries_succeeded: number;
  surface_reconfigures: number;
};

export interface GpuWorkerErrorEventMessage {
  type: "gpu_error_event";
  event: GpuErrorEvent;
}

export interface GpuWorkerStatsMessage {
  type: "gpu_stats";
  stats: GpuStats;
}

export type GpuWorkerOutgoingMessage =
  | GpuWorkerReadyMessage
  | GpuWorkerScreenshotMessage
  | GpuWorkerTimingsMessage
  | GpuWorkerGpuErrorMessage
  | GpuWorkerErrorEventMessage
  | GpuWorkerStatsMessage;
