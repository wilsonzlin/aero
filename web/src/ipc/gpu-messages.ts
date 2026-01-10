export type BackendKind = 'webgpu' | 'webgl2';

export interface GpuWorkerInitOptions {
  /**
   * Prefer attempting WebGPU first. If WebGPU initialization fails, the worker
   * should fall back to WebGL2 when possible.
   */
  preferWebGpu?: boolean;

  /**
   * WebGPU required features (if any). When supplied, the backend may fail to
   * initialize if the adapter cannot satisfy these.
   */
  requiredFeatures?: string[];
}

export interface GpuWorkerInitMessage {
  type: 'init';
  canvas: OffscreenCanvas;
  /** CSS pixel width. */
  width: number;
  /** CSS pixel height. */
  height: number;
  devicePixelRatio: number;
  gpuOptions?: GpuWorkerInitOptions;
}

export interface GpuWorkerResizeMessage {
  type: 'resize';
  /** CSS pixel width. */
  width: number;
  /** CSS pixel height. */
  height: number;
  devicePixelRatio: number;
}

export interface GpuWorkerPresentTestPatternMessage {
  type: 'present_test_pattern';
}

export interface GpuWorkerRequestScreenshotMessage {
  type: 'request_screenshot';
  requestId: number;
}

export interface GpuWorkerShutdownMessage {
  type: 'shutdown';
}

export type GpuWorkerIncomingMessage =
  | GpuWorkerInitMessage
  | GpuWorkerResizeMessage
  | GpuWorkerPresentTestPatternMessage
  | GpuWorkerRequestScreenshotMessage
  | GpuWorkerShutdownMessage;

export interface GpuAdapterInfo {
  vendor?: string;
  renderer?: string;
  description?: string;
}

export interface GpuWorkerReadyMessage {
  type: 'ready';
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
  type: 'screenshot';
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
  origin: 'top-left';
}

export type GpuWorkerErrorKind =
  | 'wasm_init_failed'
  | 'webgpu_not_supported'
  | 'webgpu_init_failed'
  | 'webgl2_not_supported'
  | 'webgl2_init_failed'
  | 'unexpected';

export interface GpuWorkerErrorPayload {
  kind: GpuWorkerErrorKind;
  message: string;
  stack?: string;
  hints?: string[];
}

export interface GpuWorkerGpuErrorMessage {
  type: 'gpu_error';
  fatal: boolean;
  error: GpuWorkerErrorPayload;
}

export type GpuWorkerOutgoingMessage =
  | GpuWorkerReadyMessage
  | GpuWorkerScreenshotMessage
  | GpuWorkerGpuErrorMessage;

