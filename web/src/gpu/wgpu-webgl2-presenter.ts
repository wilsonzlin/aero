import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';

import initAeroGpuWasm, {
  destroy_gpu,
  get_gpu_stats,
  has_present_rgba8888_dirty_rects_with_result,
  has_present_rgba8888_with_result,
  has_present_rgba8888_dirty_rects,
  init_gpu,
  present_rgba8888,
  present_rgba8888_dirty_rects_with_result,
  present_rgba8888_with_result,
  present_rgba8888_dirty_rects,
  request_screenshot,
  resize as resize_gpu,
} from '../wasm/aero-gpu';

import type { DirtyRect } from '../ipc/shared-layout';
import { formatOneLineError, formatOneLineUtf8, truncateUtf8 } from '../text';

function hardenWebGl2PresentState(gl: WebGL2RenderingContext): void {
  // Best-effort deterministic state hardening: wgpu uses WebGL2 state under the hood, and
  // some drivers enable dithering by default. Disable sources of output variance so
  // presented-output readbacks are more stable across environments.
  try {
    gl.disable(gl.DITHER);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.disable(gl.SAMPLE_ALPHA_TO_COVERAGE);
    gl.disable(gl.SAMPLE_COVERAGE);
    gl.colorMask(true, true, true, true);
  } catch {
    // Ignore; best-effort only.
  }
}

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
  private hasPresentWithResult = false;
  private hasDirtyRectPresentWithResult = false;

  public async init(canvas: OffscreenCanvas, width: number, height: number, dpr: number, opts?: PresenterInitOptions): Promise<void> {
    this.opts = opts ?? {};
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr || 1;
    this.canvas = canvas;
    this.gl = null;

    await initAeroGpuWasm();
    this.hasPresentWithResult = has_present_rgba8888_with_result();
    this.hasDirtyRectPresent = has_present_rgba8888_dirty_rects();
    this.hasDirtyRectPresentWithResult = has_present_rgba8888_dirty_rects_with_result();

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
    if (this.gl) {
      hardenWebGl2PresentState(this.gl);
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

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): boolean {
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
      if (this.hasPresentWithResult) {
        return present_rgba8888_with_result(data, stride);
      }

      // Back-compat fallback: older wasm bundles always returned `Ok(())` even when a frame
      // was intentionally dropped due to surface acquire failures. When the structured stats
      // export is available, detect the drop by observing whether `presents_succeeded`
      // advanced.
      const before = tryReadWasmPresentsSucceeded();
      present_rgba8888(data, stride);
      const after = tryReadWasmPresentsSucceeded();
      if (before !== null && after !== null) return after > before;
      return true;
    } catch (err) {
      // wasm panics manifest as `WebAssembly.RuntimeError: unreachable`. In some headless /
      // blocklisted GPU configurations, the wgpu WebGL2 backend can hit internal panics during
      // presentation even though `init_gpu()` succeeded. Treat these traps as an intentional
      // frame drop (return false) so the caller can continue running and telemetry can still be
      // collected, matching the present() "drop vs presented" contract.
      if (err instanceof WebAssembly.RuntimeError && `${err.message}`.includes('unreachable')) {
        return false;
      }
      const message = formatOneLineError(err, 512);
      const suffix = message && message !== 'Error' ? `: ${message}` : '';
      const cause = sanitizePresenterErrorCause(err);
      throw new PresenterError('wgpu_present_failed', `Failed to present frame via wgpu WebGL2 presenter${suffix}`, cause);
    }
  }

  public presentDirtyRects(
    frame: number | ArrayBuffer | ArrayBufferView,
    stride: number,
    dirtyRects: DirtyRect[],
  ): boolean {
    if (!this.initialized) {
      throw new PresenterError('not_initialized', 'WgpuWebGl2Presenter.presentDirtyRects() called before init()');
    }

    if (!this.hasDirtyRectPresent) {
      return this.present(frame, stride);
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `presentDirtyRects() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError('invalid_stride', `presentDirtyRects() stride (${stride}) smaller than width*4 (${tightRowBytes})`);
    }

    if (!dirtyRects || dirtyRects.length === 0) {
      return this.present(frame, stride);
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
      if (this.hasDirtyRectPresentWithResult) {
        return present_rgba8888_dirty_rects_with_result(data, stride, rectWords);
      }

      const before = tryReadWasmPresentsSucceeded();
      present_rgba8888_dirty_rects(data, stride, rectWords);
      const after = tryReadWasmPresentsSucceeded();
      if (before !== null && after !== null) return after > before;
      return true;
    } catch (err) {
      if (err instanceof WebAssembly.RuntimeError && `${err.message}`.includes('unreachable')) {
        return false;
      }
      const message = formatOneLineError(err, 512);
      const suffix = message && message !== 'Error' ? `: ${message}` : '';
      const cause = sanitizePresenterErrorCause(err);
      throw new PresenterError(
        'wgpu_present_failed',
        `Failed to present dirty rects via wgpu WebGL2 presenter${suffix}`,
        cause,
      );
    }
  }

  // Screenshot is provided by the wasm-backed *legacy* presenter and is defined as a readback
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

    // Ensure any re-render we trigger (see below) runs with our deterministic state.
    hardenWebGl2PresentState(gl);

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

    const prevReadFbo = gl.getParameter(gl.READ_FRAMEBUFFER_BINDING) as WebGLFramebuffer | null;
    const prevReadBuffer = gl.getParameter(gl.READ_BUFFER) as number;
    const prevPackBuffer = gl.getParameter(gl.PIXEL_PACK_BUFFER_BINDING) as WebGLBuffer | null;
    const prevPackAlignment = gl.getParameter(gl.PACK_ALIGNMENT) as number;
    const prevPackRowLength = gl.getParameter(gl.PACK_ROW_LENGTH) as number;
    const prevPackSkipPixels = gl.getParameter(gl.PACK_SKIP_PIXELS) as number;
    const prevPackSkipRows = gl.getParameter(gl.PACK_SKIP_ROWS) as number;
    try {
      // Ensure we read from the default framebuffer (the actual canvas output), not whatever
      // internal FBO wgpu last used.
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, null);
      // Ensure readPixels writes into client memory (not a PIXEL_PACK_BUFFER).
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, null);
      gl.pixelStorei(gl.PACK_ALIGNMENT, 1);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, 0);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, 0);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, 0);
      try {
        gl.readBuffer(gl.BACK);
      } catch {
        // Ignore: some browsers are strict about readBuffer on default framebuffers.
      }
      gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, raw);
    } finally {
      // Restore state to avoid surprising the wasm/wgpu rendering pipeline.
      gl.pixelStorei(gl.PACK_ALIGNMENT, prevPackAlignment);
      gl.pixelStorei(gl.PACK_ROW_LENGTH, prevPackRowLength);
      gl.pixelStorei(gl.PACK_SKIP_PIXELS, prevPackSkipPixels);
      gl.pixelStorei(gl.PACK_SKIP_ROWS, prevPackSkipRows);
      gl.bindBuffer(gl.PIXEL_PACK_BUFFER, prevPackBuffer);
      gl.bindFramebuffer(gl.READ_FRAMEBUFFER, prevReadFbo);
      try {
        gl.readBuffer(prevReadBuffer);
      } catch {
        // Ignore.
      }
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

function sanitizePresenterErrorCause(err: unknown): unknown {
  if (err instanceof Error) {
    const name = formatOneLineUtf8(err.name, 128) || 'Error';
    const message = formatOneLineUtf8(err.message, 512) || 'Error';
    const stack = typeof err.stack === 'string' ? truncateUtf8(err.stack, 8 * 1024) : undefined;
    return { name, message, stack };
  }

  if (err && typeof err === 'object') {
    const rec = err as Record<string, unknown>;
    const message = rec['message'];
    if (typeof message === 'string') {
      const nameRaw = rec['name'];
      let name = typeof nameRaw === 'string' ? nameRaw : 'Error';
      if (name === 'Error') {
        const ctor = rec['constructor'];
        if (typeof ctor === 'function' && typeof ctor.name === 'string') {
          name = ctor.name;
        }
      }
      const stackRaw = rec['stack'];
      const safeName = formatOneLineUtf8(name, 128) || 'Error';
      const safeMessage = formatOneLineUtf8(message, 512) || 'Error';
      const stack = typeof stackRaw === 'string' ? truncateUtf8(stackRaw, 8 * 1024) : undefined;
      return { name: safeName, message: safeMessage, stack };
    }
  }

  return err;
}

function tryReadWasmPresentsSucceeded(): number | null {
  try {
    const stats = get_gpu_stats();
    // Some telemetry exports are async/promises in older bundles. If we can't read the stats
    // synchronously, fall back to treating the present as successful.
    if (stats && typeof (stats as { then?: unknown }).then === "function") return null;
    if (!stats || typeof stats !== "object") return null;
    const raw = (stats as Record<string, unknown>)["presents_succeeded"] ?? (stats as Record<string, unknown>)["presentsSucceeded"];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : null;
  } catch {
    return null;
  }
}
