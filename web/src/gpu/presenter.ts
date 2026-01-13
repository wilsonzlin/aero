export type PresenterBackendKind = 'webgpu' | 'webgl2_wgpu' | 'webgl2_raw';
export type PresenterScaleMode = 'stretch' | 'fit' | 'integer';

export interface PresenterScreenshot {
  width: number;
  height: number;
  /**
   * RGBA8 pixel bytes in row-major order with a top-left origin
   * (i.e. the first row is the top scanline).
   *
   * ## Semantics: source framebuffer readback (NOT presented output)
   *
   * `Presenter.screenshot()` is defined as a readback of the presenter's **source
   * framebuffer**: the same RGBA8 pixels most recently passed to `present()`
   * (or uploaded into the backend's source texture).
   *
   * This is intentionally **not** a capture of the on-screen/presented canvas.
   * In particular, callers should not expect the screenshot to include:
   *
   * - scaling / filtering / integer-fit logic
   * - letterboxing / clearColor
   * - `outputWidth`/`outputHeight` or `devicePixelRatio` sizing
   *   (the screenshot `width/height` are the **source** framebuffer dimensions)
   * - sRGB encode / browser color management
   * - cursor composition or other overlays
   *
   * Hash-based tests rely on this contract being deterministic and matching the
   * source bytes. If a "what the user sees" capture is needed, add a separate
   * API for **presented output** readback instead of changing this one.
   *
   * Note: some legacy implementations (e.g. `webgl2_raw`) may currently read back
   * the default framebuffer. Treat those results as best-effort/debug-only until
   * the backend is aligned with the source-framebuffer contract above.
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
  /**
   * Capture a screenshot of the current frame.
   *
   * See `PresenterScreenshot.pixels` for the contract (source framebuffer readback,
   * not presented output).
   */
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
