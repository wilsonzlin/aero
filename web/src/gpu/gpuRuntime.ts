import type {
  GpuRuntimeEventsMessage,
  GpuRuntimeInitOptions as WorkerGpuInitOptions,
  GpuRuntimeOutMessage,
  GpuRuntimeReadyMessage,
  GpuRuntimeStatsMessage,
} from "../ipc/gpu-protocol";
import { createGpuWorker, type GpuWorkerHandle } from "../main/createGpuWorker";
import { RawWebGL2Presenter } from './raw-webgl2-presenter';

export type GpuRuntimeMode = 'worker' | 'main';
export type GpuRuntimeExecutionMode = 'auto' | 'worker' | 'main';

export interface GpuRuntimeInitOptions {
  /**
   * Controls where the GPU presenter runs.
   *
   * - "worker": preferred (uses OffscreenCanvas + worker IPC) when supported
   * - "main": force main-thread execution
   * - "auto": pick the best available option (default)
   */
  mode?: GpuRuntimeExecutionMode;

  /**
   * Worker-specific init options (WebGPU preference, forced WebGL2 fallback, etc).
   *
   * These are forwarded to the worker init message in worker mode. Main-thread mode
   * currently uses the raw WebGL2 presenter for maximum browser compatibility.
   */
  gpuOptions?: WorkerGpuInitOptions;

  /**
   * Worker-side errors (presenter init failures, WebGL context loss, etc).
   */
  onError?: (msg: Extract<GpuRuntimeOutMessage, { type: "error" }>) => void;

  /**
   * Periodic low-rate stats from the GPU worker (best-effort).
   */
  onStats?: (msg: GpuRuntimeStatsMessage) => void;

  /**
   * Structured error/event stream from the GPU worker (best-effort).
   */
  onEvents?: (msg: GpuRuntimeEventsMessage) => void;
}

export function supportsWorkerOffscreenCanvas(canvas: HTMLCanvasElement): boolean {
  return (
    typeof OffscreenCanvas !== 'undefined' &&
    typeof (canvas as unknown as { transferControlToOffscreen?: unknown }).transferControlToOffscreen ===
      'function'
  );
}

function clampNonZero(n: number): number {
  if (!Number.isFinite(n)) return 1;
  return Math.max(1, Math.round(n));
}

function cssToPixelSize(width: number, height: number, dpr: number): { width: number; height: number } {
  const ratio = dpr || 1;
  return { width: clampNonZero(width * ratio), height: clampNonZero(height * ratio) };
}

function createTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const isLeft = x < halfW;
      const isTop = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      let r = 0;
      let g = 0;
      let b = 0;
      if (isTop && isLeft) {
        r = 255;
      } else if (isTop && !isLeft) {
        g = 255;
      } else if (!isTop && isLeft) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      out[i] = r;
      out[i + 1] = g;
      out[i + 2] = b;
      out[i + 3] = 255;
    }
  }

  return out;
}

function readPixelsTopLeft(
  gl: WebGL2RenderingContext,
  width: number,
  height: number,
): Uint8ClampedArray<ArrayBuffer> {
  const pixels = new Uint8Array(width * height * 4);
  gl.readPixels(0, 0, width, height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

  // WebGL readPixels origin is bottom-left; convert to top-left.
  const rowStride = width * 4;
  const flipped = new Uint8ClampedArray(pixels.length) as Uint8ClampedArray<ArrayBuffer>;
  for (let y = 0; y < height; y += 1) {
    const srcStart = (height - 1 - y) * rowStride;
    const dstStart = y * rowStride;
    flipped.set(pixels.subarray(srcStart, srcStart + rowStride), dstStart);
  }

  return flipped;
}

function measureCanvasCssSize(
  canvas: HTMLCanvasElement,
  fallbackW: number,
  fallbackH: number,
): { w: number; h: number; dpr: number } {
  const dpr = Math.max(1, typeof window !== 'undefined' ? window.devicePixelRatio || 1 : 1);
  const rect = canvas.getBoundingClientRect?.();
  const w = rect?.width ? Math.round(rect.width) : fallbackW;
  const h = rect?.height ? Math.round(rect.height) : fallbackH;
  return { w: Math.max(1, w), h: Math.max(1, h), dpr };
}

interface GpuRuntimeImpl {
  readonly mode: GpuRuntimeMode;
  readonly backendKind: string;
  resize(w: number, h: number, dpr: number): Promise<void>;
  present(): Promise<void>;
  screenshot(): Promise<ImageData>;
}

class MainThreadGpuRuntimeImpl implements GpuRuntimeImpl {
  readonly mode: GpuRuntimeMode = 'main';
  readonly backendKind = "webgl2";

  private readonly canvas: HTMLCanvasElement;
  private presenter: RawWebGL2Presenter;
  private pixelWidth = 1;
  private pixelHeight = 1;
  private pattern: Uint8Array = new Uint8Array(4);

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    this.presenter = new RawWebGL2Presenter(canvas, {
      framebufferColorSpace: 'linear',
      outputColorSpace: 'srgb',
      alphaMode: 'opaque',
      flipY: false,
    });
  }

  async resize(w: number, h: number, dpr: number): Promise<void> {
    const { width, height } = cssToPixelSize(w, h, dpr);
    this.pixelWidth = width;
    this.pixelHeight = height;
    this.canvas.width = width;
    this.canvas.height = height;
    this.pattern = createTestPattern(width, height);
  }

  async present(): Promise<void> {
    this.presenter.setSourceRgba8(this.pattern, this.pixelWidth, this.pixelHeight);
    this.presenter.present();
  }

  async screenshot(): Promise<ImageData> {
    const gl = this.presenter.gl as WebGL2RenderingContext;
    gl.finish();
    const rgba8 = readPixelsTopLeft(gl, this.pixelWidth, this.pixelHeight);
    return new ImageData(rgba8, this.pixelWidth, this.pixelHeight);
  }
}

class WorkerGpuRuntimeImpl implements GpuRuntimeImpl {
  readonly mode: GpuRuntimeMode = 'worker';
  readonly backendKind: string;
  private readonly handle: GpuWorkerHandle;

  constructor(
    handle: GpuWorkerHandle,
    ready: GpuRuntimeReadyMessage,
  ) {
    this.handle = handle;
    this.backendKind = ready.backendKind;
  }

  async resize(w: number, h: number, dpr: number): Promise<void> {
    this.handle.resize(w, h, dpr);
  }

  async present(): Promise<void> {
    this.handle.presentTestPattern();
  }

  async screenshot(): Promise<ImageData> {
    // `GpuRuntime.screenshot()` is intended to return what the user sees on the canvas.
    // Request a presented-output readback so:
    // - the dimensions match the canvas (post-DPR)
    // - sRGB/alpha presentation policy is applied
    // - cursor composition is included when the presenter backend applies it
    const shot = await this.handle.requestPresentedScreenshot();
    return new ImageData(new Uint8ClampedArray(shot.rgba8), shot.width, shot.height);
  }
}

export class GpuRuntime {
  mode: GpuRuntimeMode | null = null;
  backendKind: string | null = null;
  workerReady: GpuRuntimeReadyMessage | null = null;

  private impl: GpuRuntimeImpl | null = null;
  private canvas: HTMLCanvasElement | null = null;
  private w = 0;
  private h = 0;
  private dpr = 1;

  async init(
    canvas: HTMLCanvasElement,
    w: number,
    h: number,
    dpr: number,
    opts: GpuRuntimeInitOptions = {},
  ): Promise<void> {
    if (this.impl) throw new Error('GpuRuntime already initialized');

    this.canvas = canvas;
    this.w = w;
    this.h = h;
    this.dpr = dpr;

    const desiredMode = opts.mode ?? 'auto';
    const canUseWorker = desiredMode !== 'main' && supportsWorkerOffscreenCanvas(canvas);

    if (canUseWorker) {
      const handle = createGpuWorker({
        canvas,
        width: w,
        height: h,
        devicePixelRatio: dpr,
        gpuOptions: opts.gpuOptions,
        onError: opts.onError,
        onStats: opts.onStats,
        onEvents: opts.onEvents,
      });

      const ready = await handle.ready;
      this.workerReady = ready;
      this.impl = new WorkerGpuRuntimeImpl(handle, ready);
      this.mode = this.impl.mode;
      this.backendKind = this.impl.backendKind;
      return;
    }

    const impl = new MainThreadGpuRuntimeImpl(canvas);
    await impl.resize(w, h, dpr);
    this.impl = impl;
    this.mode = impl.mode;
    this.backendKind = impl.backendKind;
    this.workerReady = null;
  }

  async resize(): Promise<void> {
    if (!this.impl || !this.canvas) throw new Error('GpuRuntime not initialized');
    const { w, h, dpr } = measureCanvasCssSize(this.canvas, this.w, this.h);
    this.w = w;
    this.h = h;
    this.dpr = dpr;
    await this.impl.resize(w, h, dpr);
  }

  async present(): Promise<void> {
    if (!this.impl) throw new Error('GpuRuntime not initialized');
    await this.impl.present();
  }

  async screenshot(): Promise<ImageData> {
    if (!this.impl) throw new Error('GpuRuntime not initialized');
    return await this.impl.screenshot();
  }
}
