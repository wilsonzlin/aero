import type { DirtyRect } from '../ipc/shared-layout';

export type PresenterBackendKind = 'webgpu' | 'webgl2_wgpu' | 'webgl2_raw';
export type PresenterScaleMode = 'stretch' | 'fit' | 'integer';

export interface PresenterScreenshot {
  width: number;
  height: number;
  /**
   * RGBA8 pixel bytes in row-major order with a top-left origin
   * (i.e. the first row is the top scanline).
   *
   * ## Semantics
   *
   * This type is returned by two different capture paths:
   *
   * - `Presenter.screenshot()` (**deterministic source framebuffer readback**):
   *   - Returns the same RGBA8 pixels most recently passed to `present()` (or uploaded
   *     into the backend's source texture).
   *   - The buffer is **tight-packed**: `byteLength === width * height * 4` and each row
   *     is exactly `width * 4` bytes (no per-row padding/stride).
   *   - `width/height` are the **source** framebuffer dimensions.
   *   - Callers should NOT expect the result to include:
   *     - scaling / filtering / integer-fit logic
   *     - letterboxing / clearColor
   *     - `outputWidth`/`outputHeight` or `devicePixelRatio` sizing
   *     - sRGB encode / browser color management
   *     - cursor composition or other overlays
   *   - Hash-based tests rely on this contract being stable. Do not “fix” this to read back
   *     the presented canvas output.
   *
   * - `Presenter.screenshotPresented()` (**debug-only presented output readback**; optional):
   *   - Reads back the final pixels rendered to the canvas/surface **after** presentation
   *     policy (scaling/letterboxing, sRGB/alpha policy, cursor composition, etc).
   *   - `width/height` are the **canvas** physical pixel dimensions (post-DPR).
   *   - This is intended for debug/validation only; it is not suitable for deterministic
   *     hashing because it mixes presentation policy + color management concerns.
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
   * Request an alpha channel on the underlying canvas when using a WebGL2 presenter.
   *
   * Default: false (opaque canvas).
   *
   * This is primarily a diagnostic aid for pages that want to visualize alpha semantics
   * (e.g. XRGB vs ARGB scanout formats) by letting the page background show through.
   *
   * Note: when enabled, the raw WebGL2 presenter preserves the source alpha channel in the
   * presented output instead of forcing opaque alpha.
   */
  canvasAlpha?: boolean;
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
  /**
   * Present the provided framebuffer bytes to the output surface.
   *
   * ## Return value (drop vs presented)
   *
   * Backends may intentionally *drop* a frame (e.g. surface acquire timeout, or
   * recoverable surface loss/outdated handling). In these cases they should
   * return `false` to indicate the frame was not actually presented.
   *
   * Returning `undefined` (the historical behavior) is treated as success.
   */
  present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void | boolean;
  /**
   * Optional dirty-rect optimized present path.
   *
   * When implemented, should follow the same return-value semantics as `present()`:
   * `false` indicates the frame was dropped (not presented).
   */
  presentDirtyRects?(
    frame: number | ArrayBuffer | ArrayBufferView,
    stride: number,
    dirtyRects: DirtyRect[],
  ): void | boolean;
  /**
   * Capture a screenshot of the current frame.
   *
   * See `PresenterScreenshot.pixels` for the contract: this is the **source framebuffer**
   * readback used for deterministic hashing (not presented output).
   */
  screenshot(): Promise<PresenterScreenshot> | PresenterScreenshot;
  /**
   * Debug-only: capture a screenshot of the **presented output** (canvas/surface pixels).
   *
   * This is useful for validating presentation policy (scaling/letterboxing, sRGB/alpha)
   * but is intentionally separate from `screenshot()` so hash-based tests remain stable.
   */
  screenshotPresented?: () => Promise<PresenterScreenshot> | PresenterScreenshot;
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
