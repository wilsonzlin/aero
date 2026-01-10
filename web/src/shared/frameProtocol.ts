export const FRAME_STATUS_INDEX = 0;
export const FRAME_SEQ_INDEX = 1;
export const FRAME_METRICS_RECEIVED_INDEX = 2;
export const FRAME_METRICS_PRESENTED_INDEX = 3;
export const FRAME_METRICS_DROPPED_INDEX = 4;

export const FRAME_PRESENTED = 0;
export const FRAME_DIRTY = 1;
export const FRAME_PRESENTING = 2;

export type DirtyRect = { x: number; y: number; w: number; h: number };
export type FrameTimingsReport = {
  frame_index: number;
  backend: 'webgpu' | 'webgl2';
  cpu_encode_us: number;
  cpu_submit_us: number;
  gpu_us?: number;
};

export type GpuWorkerInitMessage = {
  type: 'init';
  sharedFrameState?: SharedArrayBuffer;
  /**
   * Optional shared-memory region containing the emulator framebuffer.
   *
   * This is a zero-copy alternative to sending full frames via `postMessage`.
   * The GPU worker (or its presenter module) can read pixels directly from this
   * buffer using `Atomics` + typed array views.
   */
  sharedFramebuffer?: SharedArrayBuffer;
  /**
   * Byte offset within `sharedFramebuffer` where the framebuffer header begins.
   * Allows embedding the framebuffer region within a larger `SharedArrayBuffer`.
   */
  sharedFramebufferOffsetBytes?: number;
  wasmModuleUrl?: string;
};

export type GpuWorkerTickMessage = {
  type: 'tick';
  frameTimeMs: number;
};

export type GpuWorkerFrameDirtyMessage = {
  type: 'frame_dirty';
  dirtyRects?: DirtyRect[];
};

export type GpuWorkerRequestTimingsMessage = {
  type: 'request_timings';
};

export type GpuWorkerMessageFromMain =
  | GpuWorkerInitMessage
  | GpuWorkerTickMessage
  | GpuWorkerFrameDirtyMessage
  | GpuWorkerRequestTimingsMessage;

export type GpuWorkerMetricsMessage = {
  type: 'metrics';
  framesReceived: number;
  framesPresented: number;
  framesDropped: number;
  telemetry?: unknown;
};

export type GpuWorkerErrorMessage = {
  type: 'error';
  message: string;
};

export type GpuWorkerTimingsMessage = {
  type: 'timings';
  timings: FrameTimingsReport | null;
};

export type GpuWorkerMessageToMain =
  | GpuWorkerMetricsMessage
  | GpuWorkerTimingsMessage
  | GpuWorkerErrorMessage;
