export type PresenterBackendKind = 'webgpu' | 'webgl2_wgpu' | 'webgl2_raw';
export type PresenterScaleMode = 'stretch' | 'fit' | 'integer';

export interface PresenterScreenshot {
  width: number;
  height: number;
  /**
   * RGBA8 pixel bytes in row-major order with a top-left origin
   * (i.e. the first row is the top scanline).
   */
  pixels: ArrayBuffer;
}

export interface PresenterInitOptions {
  /**
   * How the framebuffer should be mapped into the canvas when their sizes
   * differ.
   */
  scaleMode?: PresenterScaleMode;
  /**
   * When specified, the presenter will set the canvas physical size to
   * `outputWidth/outputHeight * dpr`. Defaults to matching the framebuffer
   * size.
   */
  outputWidth?: number;
  outputHeight?: number;
  /**
   * Texture filtering mode; `nearest` is preferred for pixel-accurate output.
   */
  filter?: 'nearest' | 'linear';
  /**
   * Clear color used for letterboxing/pillarboxing.
   */
  clearColor?: [number, number, number, number];
  /**
   * WebAssembly memory for `present(ptr, stride)` calls.
   */
  wasmMemory?: WebAssembly.Memory;
  /**
   * WebGPU-only: required device features to request during init.
   *
   * WebGL2 backends ignore this.
   */
  requiredFeatures?: GPUFeatureName[];
  /**
   * Receives recoverable errors (forwards to main thread in worker integration).
   */
  onError?: (error: PresenterError) => void;
}

export interface Presenter {
  readonly backend: PresenterBackendKind;
  init(
    canvas: OffscreenCanvas,
    width: number,
    height: number,
    dpr: number,
    opts?: PresenterInitOptions,
  ): Promise<void> | void;
  resize(width: number, height: number, dpr: number): void;
  present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void;
  screenshot(): Promise<PresenterScreenshot> | PresenterScreenshot;
  destroy?(): void;
}

export class PresenterError extends Error {
  public readonly code: string;
  public readonly cause: unknown;

  constructor(code: string, message: string, cause?: unknown) {
    super(message);
    this.name = 'PresenterError';
    this.code = code;
    this.cause = cause;
  }
}
