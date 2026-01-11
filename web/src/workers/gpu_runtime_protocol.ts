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
};

export type GpuRuntimeShutdownMessage = { type: "shutdown" };

export type GpuRuntimeInMessage =
  | GpuRuntimeInitMessage
  | GpuRuntimeResizeMessage
  | GpuRuntimeTickMessage
  | GpuRuntimeScreenshotRequestMessage
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

export type GpuRuntimeOutMessage =
  | GpuRuntimeReadyMessage
  | GpuRuntimeMetricsMessage
  | GpuRuntimeErrorMessage
  | GpuRuntimeScreenshotResponseMessage;
