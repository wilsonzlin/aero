import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';

import initAeroGpuWasm, {
  destroy_gpu,
  has_present_rgba8888_dirty_rects,
  init_gpu,
  present_rgba8888,
  present_rgba8888_dirty_rects,
  request_screenshot,
  resize as resize_gpu,
} from '../wasm/aero-gpu';

import type { DirtyRect } from '../ipc/shared-layout';

export class WgpuWebGl2Presenter implements Presenter {
  public readonly backend = 'webgl2_wgpu' as const;

  private canvas: OffscreenCanvas | null = null;
  private gl: WebGL2RenderingContext | null = null;
  private opts: PresenterInitOptions = {};
  private srcWidth = 0;
  private srcHeight = 0;
  private dpr = 1;
  private initialized = false;
  private dirtyRectScratch: Uint32Array | null = null;
  private hasDirtyRectPresent = false;

  public async init(canvas: OffscreenCanvas, width: number, height: number, dpr: number, opts?: PresenterInitOptions): Promise<void> {
    this.opts = opts ?? {};
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr || 1;
    this.canvas = canvas;
    this.gl = null;

    await initAeroGpuWasm();
    this.hasDirtyRectPresent = has_present_rgba8888_dirty_rects();

    // Ensure stale state from a previous init is cleared before creating the new surface.
    try {
      destroy_gpu();
    } catch {
      // Ignore; module may not have been initialized yet.
    }

    try {
      await init_gpu(canvas, width, height, this.dpr, {
        // Force the wgpu GL backend (WebGL2).
        preferWebGpu: false,
        disableWebGpu: true,
        ...(this.opts.outputWidth != null ? { outputWidth: this.opts.outputWidth } : {}),
        ...(this.opts.outputHeight != null ? { outputHeight: this.opts.outputHeight } : {}),
        ...(this.opts.scaleMode != null ? { scaleMode: this.opts.scaleMode } : {}),
        ...(this.opts.filter != null ? { filter: this.opts.filter } : {}),
        ...(this.opts.clearColor != null ? { clearColor: this.opts.clearColor } : {}),
      });
    } catch (err) {
      throw new PresenterError('wgpu_init_failed', 'Failed to initialize wgpu WebGL2 presenter', err);
    }

    // Best-effort: obtain the underlying WebGL2 context so we can implement a debug-only
    // presented-output readback via readPixels(). If this fails, `screenshotPresented()`
    // will still attempt to retrieve the context lazily.
    try {
      this.gl = canvas.getContext('webgl2') as WebGL2RenderingContext | null;
    } catch {
      this.gl = null;
    }

    this.initialized = true;
  }

  public resize(width: number, height: number, dpr: number): void {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.resize() called before init()');
    }

    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr || 1;

    try {
      const outputWidth = this.opts.outputWidth;
      const outputHeight = this.opts.outputHeight;
      resize_gpu(width, height, this.dpr, outputWidth, outputHeight);
    } catch (err) {
      throw new PresenterError('wgpu_resize_failed', 'Failed to resize wgpu WebGL2 presenter', err);
    }
  }

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.present() called before init()');
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `present() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError('invalid_stride', `present() stride (${stride}) smaller than width*4 (${tightRowBytes})`);
    }

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    try {
      present_rgba8888(data, stride);
    } catch (err) {
      throw new PresenterError('wgpu_present_failed', 'Failed to present frame via wgpu WebGL2 presenter', err);
    }
  }

  public presentDirtyRects(frame: number | ArrayBuffer | ArrayBufferView, stride: number, dirtyRects: DirtyRect[]): void {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.presentDirtyRects() called before init()');
    }

    if (!this.hasDirtyRectPresent) {
      this.present(frame, stride);
      return;
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `presentDirtyRects() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError('invalid_stride', `presentDirtyRects() stride (${stride}) smaller than width*4 (${tightRowBytes})`);
    }

    if (!dirtyRects || dirtyRects.length === 0) {
      this.present(frame, stride);
      return;
    }

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    const words = dirtyRects.length * 4;
    if (!this.dirtyRectScratch || this.dirtyRectScratch.length < words) {
      this.dirtyRectScratch = new Uint32Array(words);
    }
    const rectWords = this.dirtyRectScratch.subarray(0, words);
    for (let i = 0; i < dirtyRects.length; i += 1) {
      const rect = dirtyRects[i];
      const base = i * 4;
      // Clamp to non-negative integers; wasm side clamps to framebuffer bounds.
      rectWords[base + 0] = Math.max(0, rect.x | 0) >>> 0;
      rectWords[base + 1] = Math.max(0, rect.y | 0) >>> 0;
      rectWords[base + 2] = Math.max(0, rect.w | 0) >>> 0;
      rectWords[base + 3] = Math.max(0, rect.h | 0) >>> 0;
    }

    try {
      present_rgba8888_dirty_rects(data, stride, rectWords);
    } catch (err) {
      throw new PresenterError('wgpu_present_failed', 'Failed to present dirty rects via wgpu WebGL2 presenter', err);
    }
  }

  // Screenshot is provided by the wasm-backed presenter and is defined as a readback
  // of the source framebuffer bytes (not a readPixels() of the presented canvas).
  public async screenshot(): Promise<PresenterScreenshot> {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.screenshot() called before init()');
    }

    try {
      const bytes = await request_screenshot();
      // wasm-bindgen models typed array buffers as `ArrayBufferLike`, but the
      // screenshot contract expects an ArrayBuffer. Copy if needed.
      const pixels: ArrayBuffer =
        bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength
          ? bytes.buffer
          : (() => {
              const out = new Uint8Array(bytes.byteLength);
              out.set(new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength));
              return out.buffer;
            })();
      return { width: this.srcWidth, height: this.srcHeight, pixels };
    } catch (err) {
      throw new PresenterError('wgpu_screenshot_failed', 'Failed to capture screenshot from wgpu WebGL2 presenter', err);
    }
  }

  /**
   * Debug-only: read back the *presented* canvas pixels (RGBA8, top-left origin).
   *
   * This is intentionally distinct from `screenshot()`, which returns the deterministic
   * source framebuffer bytes from the wasm module.
   */
  public async screenshotPresented(): Promise<PresenterScreenshot> {
    if (!this.initialized || !this.canvas) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.screenshotPresented() called before init()');
    }

    const canvas = this.canvas;
    const gl = this.gl ?? (canvas.getContext('webgl2') as WebGL2RenderingContext | null);
    if (!gl) {
      throw new PresenterError('webgl2_unavailable', 'Failed to access the WebGL2 context for presented screenshot readback');
    }
    this.gl = gl;

    const w = canvas.width;
    const h = canvas.height;
    if (w <= 0 || h <= 0) {
      throw new PresenterError('invalid_size', `canvas has invalid size ${w}x${h}`);
    }

    // Best-effort: redraw the last source framebuffer right before readback so we don't depend
    // on `preserveDrawingBuffer` behavior of the underlying WebGL swap chain.
    //
    // We use the existing deterministic screenshot export as the "last frame" source, then
    // re-present it through the wasm presenter (so we still validate the presenter's
    // sRGB/alpha/Y-flip policy).
    try {
      const src = await request_screenshot();
      present_rgba8888(src, this.srcWidth * 4);
    } catch {
      // Ignore: if screenshot readback fails, fall back to reading whatever is currently in the
      // canvas. This is debug-only and should not break normal presenter operation.
    }

    // Ensure rendering is complete before readback (best-effort; this is debug-only).
    try {
      gl.finish();
    } catch {
      // Ignore.
    }

    const raw = new Uint8Array(w * h * 4);

    const prevPackAlignment = gl.getParameter(gl.PACK_ALIGNMENT) as number;
    try {
      gl.pixelStorei(gl.PACK_ALIGNMENT, 1);
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, raw);
    } finally {
      // Restore state to avoid surprising the wasm/wgpu rendering pipeline.
      gl.pixelStorei(gl.PACK_ALIGNMENT, prevPackAlignment);
    }

    // WebGL readPixels returns bottom-to-top rows; normalize to a top-left origin.
    const rowBytes = w * 4;
    const out = new Uint8Array(raw.length);
    for (let y = 0; y < h; y += 1) {
      const src = (h - 1 - y) * rowBytes;
      out.set(raw.subarray(src, src + rowBytes), y * rowBytes);
    }

    return { width: w, height: h, pixels: out.buffer as ArrayBuffer };
  }

  public destroy(): void {
    if (!this.initialized) return;
    try {
      destroy_gpu();
    } catch {
      // Ignore; best-effort cleanup.
    } finally {
      this.initialized = false;
      this.canvas = null;
      this.gl = null;
    }
  }

  private resolveFrameData(frame: number | ArrayBuffer | ArrayBufferView, byteLength: number): Uint8Array {
    if (typeof frame === 'number') {
      const memory = this.opts.wasmMemory;
      if (!memory) {
        throw new PresenterError(
          'missing_wasm_memory',
          'present() called with a pointer but init opts did not include wasmMemory',
        );
      }
      const buf = memory.buffer;
      if (frame < 0 || frame + byteLength > buf.byteLength) {
        throw new PresenterError(
          'wasm_oob',
          `present() pointer range [${frame}, ${frame + byteLength}) is outside wasm memory (size=${buf.byteLength})`,
        );
      }
      return new Uint8Array(buf, frame, byteLength);
    }

    if (frame instanceof ArrayBuffer) {
      if (frame.byteLength < byteLength) {
        throw new PresenterError(
          'frame_too_small',
          `present() buffer too small: expected at least ${byteLength} bytes, got ${frame.byteLength}`,
        );
      }
      return new Uint8Array(frame, 0, byteLength);
    }

    const view = frame as ArrayBufferView;
    if (view.byteLength < byteLength) {
      throw new PresenterError(
        'frame_too_small',
        `present() view too small: expected at least ${byteLength} bytes, got ${view.byteLength}`,
      );
    }
    return new Uint8Array(view.buffer, view.byteOffset, byteLength);
  }
}
