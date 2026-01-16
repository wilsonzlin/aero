import blitShaderSource from './shaders/blit.wgsl?raw';
import type { Presenter, PresenterInitOptions, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';
import type { DirtyRect } from '../ipc/shared-layout';
import { packRgba8RectToAlignedBuffer, type PackedRect } from './webgpu-rect-pack';
import { computeViewport } from './viewport';
import { formatOneLineError } from '../text';

function alignUp(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

function isBgraFormat(format: GPUTextureFormat): boolean {
  return format === 'bgra8unorm' || format === 'bgra8unorm-srgb';
}

function bgraToRgbaInPlace(bytes: Uint8Array) {
  for (let i = 0; i < bytes.length; i += 4) {
    const b = bytes[i + 0];
    const r = bytes[i + 2];
    bytes[i + 0] = r;
    bytes[i + 2] = b;
  }
}

const webGpuGlobals = globalThis as unknown as {
  GPUTextureUsage?: {
    TEXTURE_BINDING?: number;
    COPY_DST?: number;
    COPY_SRC?: number;
    RENDER_ATTACHMENT?: number;
  };
  GPUBufferUsage?: { COPY_DST?: number; MAP_READ?: number; UNIFORM?: number };
  GPUMapMode?: { READ?: number };
};
export class WebGpuPresenterBackend implements Presenter {
  public readonly backend = 'webgpu' as const;

  private canvas: OffscreenCanvas | null = null;
  private opts: PresenterInitOptions = {};
  private srcWidth = 0;
  private srcHeight = 0;
  private dpr = 1;

  private ctx: any = null;
  private gpu: any = null;
  private device: any = null;
  private queue: any = null;
  private canvasFormat: any = null;
  private viewFormat: any = null;
  private srgbEncodeInShader = true;
  private pipelineFormat: any = null;

  private pipeline: any = null;
  private sampler: any = null;
  private frameTexture: any = null;
  private frameView: any = null;
  private bindGroup: any = null;

  private cursorTexture: any = null;
  private cursorView: any = null;
  private cursorUniformBuffer: any = null;
  private cursorTextureWidth = 0;
  private cursorTextureHeight = 0;

  // Staging buffer for non-256-aligned rows.
  private staging: Uint8Array | null = null;
  private stagingBytesPerRow = 0;

  // Staging buffer for dirty-rect uploads (width/height varies per rect).
  private dirtyRectStaging: Uint8Array | null = null;
  private dirtyRectPack: PackedRect = { x: 0, y: 0, w: 0, h: 0, bytesPerRow: 0, byteLength: 0 };

  // Cursor upload staging (same alignment constraints as the main frame upload).
  private cursorStaging: Uint8Array | null = null;
  private cursorStagingBytesPerRow = 0;

  private cursorImage: Uint8Array | null = null;
  private cursorWidth = 0;
  private cursorHeight = 0;
  private cursorEnabled = false;
  private cursorRenderEnabled = true;
  private cursorX = 0;
  private cursorY = 0;
  private cursorHotX = 0;
  private cursorHotY = 0;

  private destroyed = false;

  // Keep these bit values in sync with:
  // - `web/src/gpu/webgpu-presenter.ts`
  // - `crates/aero-gpu/src/present.rs`
  private static readonly FLAG_APPLY_SRGB_ENCODE = 1;
  private static readonly FLAG_FORCE_OPAQUE_ALPHA = 4;

  private uncapturedErrorDevice: any = null;
  private onUncapturedError: ((ev: any) => void) | null = null;
  private seenUncapturedErrorKeys: Set<string> = new Set();

  public async init(
    canvas: OffscreenCanvas,
    width: number,
    height: number,
    dpr: number,
    opts?: PresenterInitOptions,
  ): Promise<void> {
    this.destroyed = false;
    this.canvas = canvas;
    this.opts = opts ?? {};
    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr;

    try {
      const outputWidth = this.opts.outputWidth ?? width;
      const outputHeight = this.opts.outputHeight ?? height;
      this.resizeCanvas(outputWidth, outputHeight, dpr);

      const gpu = (navigator as unknown as { gpu?: GPU }).gpu;
      if (!gpu) {
        throw new PresenterError('webgpu_unavailable', 'WebGPU is not available in this environment');
      }
      this.gpu = gpu;

      const userAgent = (navigator as unknown as { userAgent?: unknown }).userAgent;
      const isHeadless = /HeadlessChrome/.test(typeof userAgent === 'string' ? userAgent : '');
      // Prefer the fallback adapter in headless/test runs for stability and deterministic output.
      // (We still try the "high-performance" path as a backup so real browsers can use hardware.)
      let adapter = await gpu.requestAdapter?.(
        isHeadless ? { forceFallbackAdapter: true } : { powerPreference: 'high-performance' },
      );
      if (!adapter) {
        adapter = await gpu.requestAdapter?.(
          isHeadless ? { powerPreference: 'high-performance' } : { forceFallbackAdapter: true },
        );
      }
      if (!adapter) {
        throw new PresenterError('webgpu_no_adapter', 'navigator.gpu.requestAdapter() returned null');
      }

      const requiredFeatures = (this.opts.requiredFeatures ?? []) as GPUFeatureName[];
      const device = await adapter.requestDevice?.(
        requiredFeatures.length ? { requiredFeatures } : undefined,
      );
      if (!device) {
        throw new PresenterError('webgpu_no_device', 'adapter.requestDevice() returned null');
      }
      this.device = device;
      this.queue = device.queue;

      const ctx = (canvas as unknown as { getContext: (type: string) => unknown }).getContext('webgpu');
      if (!ctx) {
        throw new PresenterError('webgpu_context_unavailable', 'Failed to create a WebGPU canvas context');
      }
      this.ctx = ctx;

      this.installUncapturedErrorHandler(device);

      // Report device loss asynchronously.
      (device.lost as Promise<any> | undefined)?.then((info) => {
        if (this.destroyed) return;
        if (this.device !== device) return;
        const reason = info?.reason ?? 'unknown';
        const message = info?.message ? `: ${info.message}` : '';
        this.opts.onError?.(new PresenterError('webgpu_device_lost', `WebGPU device lost (${reason})${message}`));
      });

      this.canvasFormat = gpu.getPreferredCanvasFormat?.() ?? 'bgra8unorm';
      this.configureContext();
      this.createFrameResources(width, height);
    } catch (err) {
      // Ensure partially initialized devices/handlers don't leak if init() fails and the worker
      // falls back to another backend.
      try {
        this.destroy();
      } catch {
        // Ignore; rethrow original error.
      }
      throw err;
    }
  }

  public resize(width: number, height: number, dpr: number): void {
    if (!this.canvas || !this.device) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.resize() called before init()');
    }

    this.srcWidth = width;
    this.srcHeight = height;
    this.dpr = dpr;

    const outputWidth = this.opts.outputWidth ?? width;
    const outputHeight = this.opts.outputHeight ?? height;
    this.resizeCanvas(outputWidth, outputHeight, dpr);

    this.configureContext();
    this.createFrameResources(width, height);
  }

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): boolean {
    if (!this.canvas || !this.device || !this.queue || !this.ctx || !this.pipeline || !this.bindGroup || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.present() called before init()');
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `present() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError(
        'invalid_stride',
        `present() stride (${stride}) smaller than width*4 (${tightRowBytes})`,
      );
    }

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    const bytesPerRowAligned = stride % 256 === 0 ? stride : alignUp(tightRowBytes, 256);
    const upload = bytesPerRowAligned === stride ? data : this.copyToStaging(data, stride, bytesPerRowAligned);

    this.queue.writeTexture(
      { texture: this.frameTexture },
      upload,
      { bytesPerRow: bytesPerRowAligned, rowsPerImage: this.srcHeight },
      { width: this.srcWidth, height: this.srcHeight, depthOrArrayLayers: 1 },
    );

    return this.renderToCanvas();
  }

  public presentDirtyRects(
    frame: number | ArrayBuffer | ArrayBufferView,
    stride: number,
    dirtyRects: DirtyRect[],
  ): boolean {
    if (!this.canvas || !this.device || !this.queue || !this.ctx || !this.pipeline || !this.bindGroup || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.presentDirtyRects() called before init()');
    }

    if (stride <= 0) {
      throw new PresenterError('invalid_stride', `presentDirtyRects() stride must be > 0; got ${stride}`);
    }

    const tightRowBytes = this.srcWidth * 4;
    if (stride < tightRowBytes) {
      throw new PresenterError(
        'invalid_stride',
        `presentDirtyRects() stride (${stride}) smaller than width*4 (${tightRowBytes})`,
      );
    }

    if (!dirtyRects || dirtyRects.length === 0) return this.present(frame, stride);

    const expectedBytes = stride * this.srcHeight;
    const data = this.resolveFrameData(frame, expectedBytes);

    const origin = { x: 0, y: 0 };
    const dst = { texture: this.frameTexture, origin };
    const layout = { bytesPerRow: 0, rowsPerImage: 0 };
    const size = { width: 0, height: 0, depthOrArrayLayers: 1 };

    let anyUploaded = false;
    for (const rect of dirtyRects) {
      const buffer = packRgba8RectToAlignedBuffer(
        data,
        stride,
        this.srcWidth,
        this.srcHeight,
        rect,
        this.dirtyRectStaging,
        this.dirtyRectPack,
      );
      if (!buffer) continue;
      this.dirtyRectStaging = buffer;
      anyUploaded = true;

      origin.x = this.dirtyRectPack.x;
      origin.y = this.dirtyRectPack.y;
      layout.bytesPerRow = this.dirtyRectPack.bytesPerRow;
      layout.rowsPerImage = this.dirtyRectPack.h;
      size.width = this.dirtyRectPack.w;
      size.height = this.dirtyRectPack.h;

      this.queue.writeTexture(
        dst,
        // `packRgba8RectToAlignedBuffer` may reuse an oversized scratch buffer. Pass only the
        // bytes we just populated so browsers don't have to consider/copy unrelated tail data.
        buffer.subarray(0, this.dirtyRectPack.byteLength),
        layout,
        size,
      );
    }

    if (!anyUploaded) {
      // Defensive fallback: if all rects were invalid after clamping, do a full upload so the
      // consumer does not keep presenting a stale frame.
      return this.present(frame, stride);
    }

    return this.renderToCanvas();
  }

  public setCursorImageRgba8(rgba: Uint8Array, width: number, height: number): void {
    if (!this.device || !this.queue) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.setCursorImageRgba8() called before init()');
    }

    const w = Math.max(0, width | 0);
    const h = Math.max(0, height | 0);
    if (w === 0 || h === 0) {
      throw new PresenterError('invalid_cursor_image', 'cursor width/height must be non-zero');
    }
    const required = w * h * 4;
    if (rgba.byteLength < required) {
      throw new PresenterError(
        'invalid_cursor_image',
        `cursor RGBA buffer too small: expected at least ${required} bytes, got ${rgba.byteLength}`,
      );
    }

    this.cursorImage = rgba;
    this.cursorWidth = w;
    this.cursorHeight = h;

    const usage =
      (webGpuGlobals.GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) | (webGpuGlobals.GPUTextureUsage?.COPY_DST ?? 0x02);

    // Reallocate when dimensions change (bind groups cannot be updated in place).
    const needsRealloc =
      !this.cursorTexture || !this.cursorView || this.cursorTextureWidth !== w || this.cursorTextureHeight !== h;
    if (needsRealloc) {
      this.cursorTexture?.destroy?.();
      this.cursorTexture = this.device.createTexture({
        size: { width: w, height: h, depthOrArrayLayers: 1 },
        format: 'rgba8unorm',
        usage,
      });
      this.cursorView = this.cursorTexture.createView();
      this.cursorTextureWidth = w;
      this.cursorTextureHeight = h;
      this.rebuildBindGroup();
    }

    // Upload cursor pixels (pad rows to 256 bytes as required by WebGPU).
    const tightRowBytes = w * 4;
    const bytesPerRow = alignUp(tightRowBytes, 256);
    const upload =
      bytesPerRow === tightRowBytes
        ? rgba.subarray(0, required)
        : this.copyCursorToStaging(rgba.subarray(0, required), tightRowBytes, bytesPerRow, w, h);

    this.queue.writeTexture(
      { texture: this.cursorTexture },
      upload,
      { bytesPerRow, rowsPerImage: h },
      { width: w, height: h, depthOrArrayLayers: 1 },
    );
  }

  public setCursorState(enabled: boolean, x: number, y: number, hotX: number, hotY: number): void {
    this.cursorEnabled = !!enabled;
    this.cursorX = x | 0;
    this.cursorY = y | 0;
    this.cursorHotX = Math.max(0, hotX | 0);
    this.cursorHotY = Math.max(0, hotY | 0);
  }

  public setCursorRenderEnabled(enabled: boolean): void {
    this.cursorRenderEnabled = !!enabled;
  }

  public redraw(): void {
    this.renderToCanvas();
  }

  // Screenshot reads back the source texture (`frameTexture`), not the presented
  // canvas output. This avoids any scaling/color-space ambiguity and matches the
  // deterministic hashing contract used by smoke tests.
  public async screenshot(): Promise<PresenterScreenshot> {
    if (!this.device || !this.queue || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.screenshot() called before init()');
    }

    const bytesPerRowTight = this.srcWidth * 4;
    const bytesPerRow = alignUp(bytesPerRowTight, 256);
    const bufferSize = bytesPerRow * this.srcHeight;

    const readback = this.device.createBuffer({
      size: bufferSize,
      usage: (webGpuGlobals.GPUBufferUsage?.COPY_DST ?? 0x08) | (webGpuGlobals.GPUBufferUsage?.MAP_READ ?? 0x01),
    });

    const encoder = this.device.createCommandEncoder();
    encoder.copyTextureToBuffer(
      { texture: this.frameTexture },
      { buffer: readback, bytesPerRow, rowsPerImage: this.srcHeight },
      { width: this.srcWidth, height: this.srcHeight, depthOrArrayLayers: 1 },
    );

    this.queue.submit([encoder.finish()]);

    const mapModeRead = webGpuGlobals.GPUMapMode?.READ ?? 0x0001;
    await readback.mapAsync(mapModeRead);

    const mapped = new Uint8Array(readback.getMappedRange());
    const out = new Uint8Array(bytesPerRowTight * this.srcHeight);
    for (let y = 0; y < this.srcHeight; y++) {
      out.set(mapped.subarray(y * bytesPerRow, y * bytesPerRow + bytesPerRowTight), y * bytesPerRowTight);
    }

    readback.unmap();
    readback.destroy?.();

    return { width: this.srcWidth, height: this.srcHeight, pixels: out.buffer };
  }

  /**
   * Debug-only: read back the *presented* canvas pixels as RGBA8 (top-left origin).
   *
   * Unlike `screenshot()`, which returns the source framebuffer texture bytes, this captures the
   * final post-blit output (including gamma/alpha policy and letterboxing).
   */
  public async screenshotPresented(): Promise<PresenterScreenshot> {
    if (
      !this.canvas ||
      !this.device ||
      !this.queue ||
      !this.ctx ||
      !this.pipeline ||
      !this.bindGroup ||
      !this.cursorUniformBuffer
    ) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.screenshotPresented() called before init()');
    }

    const width = this.canvas.width;
    const height = this.canvas.height;
    if (width <= 0 || height <= 0) {
      throw new PresenterError('invalid_size', `canvas has invalid size ${width}x${height}`);
    }

    // WebGPU requires bytesPerRow to be a multiple of 256.
    const unpaddedBytesPerRow = width * 4;
    const bytesPerRow = alignUp(unpaddedBytesPerRow, 256);
    const bufferSize = bytesPerRow * height;

    const readback = this.device.createBuffer({
      size: bufferSize,
      usage: (webGpuGlobals.GPUBufferUsage?.COPY_DST ?? 0x08) | (webGpuGlobals.GPUBufferUsage?.MAP_READ ?? 0x01),
    });

    const cursorEnable =
      this.cursorRenderEnabled && this.cursorEnabled && this.cursorWidth > 0 && this.cursorHeight > 0 ? 1 : 0;
    const cursorParams = new Int32Array(12);
    cursorParams[0] = this.srcWidth | 0;
    cursorParams[1] = this.srcHeight | 0;
    cursorParams[2] = cursorEnable;
    // cursorParams[3] (flags) is filled after we resolve which view format is in use.
    cursorParams[4] = this.cursorX | 0;
    cursorParams[5] = this.cursorY | 0;
    cursorParams[6] = this.cursorHotX | 0;
    cursorParams[7] = this.cursorHotY | 0;
    cursorParams[8] = this.cursorWidth | 0;
    cursorParams[9] = this.cursorHeight | 0;
    cursorParams[10] = 0;
    cursorParams[11] = 0;

    const scaleMode = this.opts.scaleMode ?? 'fit';
    const vp = computeViewport(width, height, this.srcWidth, this.srcHeight, scaleMode);
    const [r, g, b] = this.opts.clearColor ?? [0, 0, 0, 1];
    const srgbEncodeChannel = (x: number): number => {
      const v = Math.min(1, Math.max(0, x));
      if (v <= 0.0031308) return v * 12.92;
      return 1.055 * Math.pow(v, 1.0 / 2.4) - 0.055;
    };

    let currentTexture: any = null;
    let currentTextureError: unknown = null;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        currentTexture = this.ctx.getCurrentTexture();
        currentTextureError = null;
        break;
      } catch (err) {
        currentTextureError = err;
        if (attempt === 0) {
          // Surface errors (Lost/Outdated) are expected. Reconfigure and retry once.
          try {
            this.configureContext();
          } catch {
            // Ignore and retry acquire; if it still fails we'll surface the original error.
          }
          continue;
        }
      }
    }

    if (!currentTexture) {
      const message = formatOneLineError(currentTextureError, 512, 'Unknown error');
      throw new PresenterError('webgpu_surface_error', `WebGPU getCurrentTexture() failed: ${message}`, currentTextureError);
    }

    let view: any = null;
    let viewError: unknown = null;
    try {
      view =
        this.viewFormat && this.canvasFormat && this.viewFormat !== this.canvasFormat
          ? currentTexture.createView({ format: this.viewFormat })
          : currentTexture.createView();
    } catch (err) {
      viewError = err;
    }

    if (!view) {
      if (this.viewFormat && this.canvasFormat && this.viewFormat !== this.canvasFormat) {
        this.viewFormat = this.canvasFormat;
        this.srgbEncodeInShader = true;
        if (this.pipelineFormat !== this.viewFormat || !this.pipeline) {
          this.pipelineFormat = this.viewFormat;
          this.createPipelineAndSampler();
          this.rebuildBindGroup();
        }
        try {
          view = currentTexture.createView();
          viewError = null;
        } catch (err) {
          viewError = err;
        }
      }
    }

    if (!view) {
      const message = formatOneLineError(viewError, 512, 'Unknown error');
      throw new PresenterError('webgpu_surface_error', `WebGPU currentTexture.createView() failed: ${message}`, viewError);
    }

    const flags =
      WebGpuPresenterBackend.FLAG_FORCE_OPAQUE_ALPHA |
      (this.srgbEncodeInShader ? WebGpuPresenterBackend.FLAG_APPLY_SRGB_ENCODE : 0);
    cursorParams[3] = flags | 0;
    this.queue.writeBuffer(this.cursorUniformBuffer, 0, cursorParams);

    const clearR = this.srgbEncodeInShader ? srgbEncodeChannel(r) : r;
    const clearG = this.srgbEncodeInShader ? srgbEncodeChannel(g) : g;
    const clearB = this.srgbEncodeInShader ? srgbEncodeChannel(b) : b;

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          // Alpha is forced opaque in shader; keep clear alpha opaque too so letterboxing doesn't
          // accidentally blend with the page background.
          clearValue: { r: clearR, g: clearG, b: clearB, a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    });

    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setViewport(vp.x, vp.y, vp.w, vp.h, 0, 1);
    pass.draw(3);
    pass.end();

    encoder.copyTextureToBuffer(
      { texture: currentTexture },
      { buffer: readback, bytesPerRow, rowsPerImage: height },
      { width, height, depthOrArrayLayers: 1 },
    );

    this.queue.submit([encoder.finish()]);

    const mapModeRead = webGpuGlobals.GPUMapMode?.READ ?? 0x0001;
    await readback.mapAsync(mapModeRead);
    const mapped = new Uint8Array(readback.getMappedRange());

    const out = new Uint8Array(unpaddedBytesPerRow * height);
    for (let y = 0; y < height; y++) {
      out.set(mapped.subarray(y * bytesPerRow, y * bytesPerRow + unpaddedBytesPerRow), y * unpaddedBytesPerRow);
    }

    readback.unmap();
    readback.destroy?.();

    // Convert swapchain storage order -> RGBA8 for stable hashing.
    if (isBgraFormat(this.canvasFormat as GPUTextureFormat)) {
      bgraToRgbaInPlace(out);
    }

    return { width, height, pixels: out.buffer };
  }

  public destroy(): void {
    this.destroyed = true;
    this.uninstallUncapturedErrorHandler();
    this.frameTexture?.destroy?.();
    this.cursorTexture?.destroy?.();
    try {
      this.ctx?.unconfigure?.();
    } catch {
      // Ignore.
    }
    try {
      this.device?.destroy?.();
    } catch {
      // Ignore.
    }
    this.frameTexture = null;
    this.frameView = null;
    this.bindGroup = null;
    this.cursorTexture = null;
    this.cursorView = null;
    this.cursorUniformBuffer = null;
    this.cursorTextureWidth = 0;
    this.cursorTextureHeight = 0;
    this.pipeline = null;
    this.sampler = null;
    this.canvasFormat = null;
    this.viewFormat = null;
    this.srgbEncodeInShader = true;
    this.pipelineFormat = null;
    this.device = null;
    this.queue = null;
    this.ctx = null;
    this.gpu = null;
    this.canvas = null;
    this.staging = null;
    this.stagingBytesPerRow = 0;
    this.dirtyRectStaging = null;
    this.cursorStaging = null;
    this.cursorStagingBytesPerRow = 0;
  }

  private installUncapturedErrorHandler(device: any): void {
    this.uninstallUncapturedErrorHandler();
    this.seenUncapturedErrorKeys.clear();

    // WebGPU validation errors can surface as `GPUUncapturedErrorEvent`s rather than thrown
    // exceptions. Forward them via the presenter's `onError` callback so the worker can surface
    // structured diagnostics.
    const handler = (ev: any) => {
      try {
        if (this.destroyed) return;
        if (this.device !== device) return;

        // Best-effort: avoid double-reporting (console + diagnostics) when the event is cancelable.
        ev?.preventDefault?.();

        const err = (ev as { error?: unknown } | undefined)?.error;
        const ctor = err && typeof err === 'object' ? (err as { constructor?: unknown }).constructor : undefined;
        const ctorName = typeof ctor === 'function' ? ctor.name : '';
        const errorName =
          (err && typeof err === 'object' && typeof (err as { name?: unknown }).name === 'string' ? (err as { name: string }).name : '') ||
          ctorName;
        const errorMessage =
          err && typeof err === 'object' && typeof (err as { message?: unknown }).message === 'string'
            ? (err as { message: string }).message
            : '';
        let msg = errorMessage || formatOneLineError(err ?? 'WebGPU uncaptured error', 512);
        if (errorName && msg && !msg.toLowerCase().startsWith(errorName.toLowerCase())) {
          msg = `${errorName}: ${msg}`;
        }

        // Avoid flooding: emit each unique (name, message) pair at most once per init().
        const key = `${errorName}:${msg}`;
        if (this.seenUncapturedErrorKeys.has(key)) return;
        this.seenUncapturedErrorKeys.add(key);
        // Defensive bound: if the error stream is producing unique messages (e.g. with IDs),
        // don't let the set grow without limit.
        if (this.seenUncapturedErrorKeys.size > 128) {
          this.seenUncapturedErrorKeys.clear();
          this.seenUncapturedErrorKeys.add(key);
        }

        const details: Record<string, unknown> = {
          name: errorName || undefined,
          message: errorMessage || msg,
        };
        if (err && typeof err === 'object' && typeof (err as { stack?: unknown }).stack === 'string') {
          details.stack = (err as { stack: string }).stack;
        }

        this.opts.onError?.(new PresenterError('webgpu_uncaptured_error', msg, details));
      } catch {
        // Never throw from an uncaptured error callback; best-effort diagnostics only.
      }
    };

    this.uncapturedErrorDevice = device;
    this.onUncapturedError = handler;

    try {
      if (typeof device.addEventListener === 'function') {
        device.addEventListener('uncapturederror', handler);
        return;
      }
    } catch {
      // Fall through to the onuncapturederror IDL.
    }

    // Fall back to the older onuncapturederror IDL if needed.
    try {
      (device as unknown as { onuncapturederror?: unknown }).onuncapturederror = handler;
    } catch {
      // Ignore.
    }
  }

  private uninstallUncapturedErrorHandler(): void {
    const device = this.uncapturedErrorDevice;
    const handler = this.onUncapturedError;
    if (device && handler) {
      try {
        device.removeEventListener?.('uncapturederror', handler);
      } catch {
        // Ignore.
      }
      try {
        const anyDevice = device as unknown as { onuncapturederror?: unknown };
        if (anyDevice.onuncapturederror === handler) {
          anyDevice.onuncapturederror = null;
        }
      } catch {
        // Ignore.
      }
    }
    this.uncapturedErrorDevice = null;
    this.onUncapturedError = null;
    this.seenUncapturedErrorKeys.clear();
  }

  private resizeCanvas(outputWidth: number, outputHeight: number, dpr: number): void {
    if (!this.canvas) return;
    this.canvas.width = Math.max(1, Math.round(outputWidth * dpr));
    this.canvas.height = Math.max(1, Math.round(outputHeight * dpr));
  }

  private configureContext(): void {
    if (!this.ctx || !this.device || !this.canvasFormat) return;
    try {
      this.ctx.unconfigure?.();
    } catch {
      // Ignore.
    }

    const renderUsage = webGpuGlobals.GPUTextureUsage?.RENDER_ATTACHMENT ?? 0x10;
    const copySrcUsage = webGpuGlobals.GPUTextureUsage?.COPY_SRC ?? 0x01;

    const toSrgbFormat = (format: unknown): unknown => {
      if (format === 'bgra8unorm') return 'bgra8unorm-srgb';
      if (format === 'rgba8unorm') return 'rgba8unorm-srgb';
      return null;
    };
    const isSrgbFormat = (format: unknown): boolean =>
      typeof format === 'string' && (format as string).toLowerCase().endsWith('-srgb');

    // Explicit presentation color policy (docs/04):
    // - Input framebuffer is linear (`rgba8unorm`)
    // - Output is sRGB whenever possible
    // - Prefer an sRGB view format via `viewFormats`; fall back to shader encoding when rejected.
    const srgbFormat = toSrgbFormat(this.canvasFormat);
    let viewFormat: any = this.canvasFormat;
    let srgbEncodeInShader = true;

    if (isSrgbFormat(this.canvasFormat)) {
      // Some future implementations may return an sRGB canvas format directly.
      // In that case, the surface itself will perform encoding; do not double-encode in shader.
      this.ctx.configure({
        device: this.device,
        format: this.canvasFormat,
        usage: renderUsage | copySrcUsage,
        alphaMode: 'opaque',
      });
      viewFormat = this.canvasFormat;
      srgbEncodeInShader = false;
    } else if (srgbFormat) {
      try {
        this.ctx.configure({
          device: this.device,
          format: this.canvasFormat,
          usage: renderUsage | copySrcUsage,
          alphaMode: 'opaque',
          // TS libdefs may not include `viewFormats` yet.
          viewFormats: [srgbFormat],
        });
        viewFormat = srgbFormat;
        srgbEncodeInShader = false;
      } catch {
        // Fall back to a linear swapchain view and perform encoding in shader.
        this.ctx.configure({
          device: this.device,
          format: this.canvasFormat,
          usage: renderUsage | copySrcUsage,
          alphaMode: 'opaque',
        });
        viewFormat = this.canvasFormat;
        srgbEncodeInShader = true;
      }
    } else {
      this.ctx.configure({
        device: this.device,
        format: this.canvasFormat,
        usage: renderUsage | copySrcUsage,
        alphaMode: 'opaque',
      });
      viewFormat = this.canvasFormat;
      srgbEncodeInShader = true;
    }

    this.viewFormat = viewFormat;
    this.srgbEncodeInShader = srgbEncodeInShader;

    // Ensure the render pipeline target matches the swapchain view format.
    if (this.pipelineFormat !== viewFormat || !this.pipeline) {
      this.pipelineFormat = viewFormat;
      this.createPipelineAndSampler();
      // The pipeline layout comes from `layout: 'auto'`; rebuild bind groups against the new layout.
      this.rebuildBindGroup();
    }
  }

  private createPipelineAndSampler(): void {
    if (!this.device || !this.viewFormat) return;

    const module = this.device.createShaderModule({ code: blitShaderSource });
    this.pipeline = this.device.createRenderPipeline({
      layout: 'auto',
      vertex: {
        module,
        entryPoint: 'vs_main',
      },
      fragment: {
        module,
        entryPoint: 'fs_main',
        targets: [{ format: this.viewFormat }],
      },
      primitive: {
        topology: 'triangle-list',
      },
    });

    const filter = this.opts.filter ?? 'nearest';
    const mode = filter === 'linear' ? 'linear' : 'nearest';
    this.sampler = this.device.createSampler({
      magFilter: mode,
      minFilter: mode,
      addressModeU: 'clamp-to-edge',
      addressModeV: 'clamp-to-edge',
    });
  }

  private createFrameResources(width: number, height: number): void {
    if (!this.device || !this.pipeline) return;

    // Release old resources.
    this.frameTexture?.destroy?.();
    this.frameTexture = null;
    this.frameView = null;
    this.bindGroup = null;
    this.staging = null;
    this.stagingBytesPerRow = 0;
    this.dirtyRectStaging = null;

    const usage =
      (webGpuGlobals.GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) |
      (webGpuGlobals.GPUTextureUsage?.COPY_DST ?? 0x02) |
      (webGpuGlobals.GPUTextureUsage?.COPY_SRC ?? 0x01);

    this.frameTexture = this.device.createTexture({
      size: { width, height, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage,
    });
    this.frameView = this.frameTexture.createView();
    this.ensureCursorResources();
    this.rebuildBindGroup();
  }

  private renderToCanvas(): boolean {
    if (
      !this.canvas ||
      !this.device ||
      !this.queue ||
      !this.ctx ||
      !this.pipeline ||
      !this.bindGroup ||
      !this.cursorUniformBuffer
    ) {
      return false;
    }

    const cursorEnable =
      this.cursorRenderEnabled && this.cursorEnabled && this.cursorWidth > 0 && this.cursorHeight > 0 ? 1 : 0;
    const cursorParams = new Int32Array(12);
    cursorParams[0] = this.srcWidth | 0;
    cursorParams[1] = this.srcHeight | 0;
    cursorParams[2] = cursorEnable;
    // cursorParams[3] (flags) is filled after we resolve which view format is in use.
    cursorParams[4] = this.cursorX | 0;
    cursorParams[5] = this.cursorY | 0;
    cursorParams[6] = this.cursorHotX | 0;
    cursorParams[7] = this.cursorHotY | 0;
    cursorParams[8] = this.cursorWidth | 0;
    cursorParams[9] = this.cursorHeight | 0;
    cursorParams[10] = 0;
    cursorParams[11] = 0;

    const canvasW = this.canvas.width;
    const canvasH = this.canvas.height;
    const scaleMode = this.opts.scaleMode ?? 'fit';

    const vp = computeViewport(canvasW, canvasH, this.srcWidth, this.srcHeight, scaleMode);
    const [r, g, b] = this.opts.clearColor ?? [0, 0, 0, 1];

    // The canvas swapchain is usually configured with a linear format (`bgra8unorm`) even when
    // the presentation pipeline expects sRGB output. When we are in the "shader encodes sRGB"
    // fallback path, make sure letterboxing clear colors follow the same policy (encode on CPU),
    // otherwise non-black clear colors would appear washed out.
    const srgbEncodeChannel = (x: number): number => {
      const v = Math.min(1, Math.max(0, x));
      if (v <= 0.0031308) return v * 12.92;
      return 1.055 * Math.pow(v, 1.0 / 2.4) - 0.055;
    };

    let currentTexture: any = null;
    let currentTextureError: unknown = null;
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        currentTexture = this.ctx.getCurrentTexture();
        currentTextureError = null;
        break;
      } catch (err) {
        currentTextureError = err;
        if (attempt === 0) {
          // Surface errors (Lost/Outdated) are expected. Reconfigure and retry once.
          try {
            this.configureContext();
          } catch {
            // Ignore and retry acquire; if it still fails we'll surface the original error.
          }
          continue;
        }
      }
    }

    if (!currentTexture) {
      const message = formatOneLineError(currentTextureError, 512, "Unknown error");
      this.opts.onError?.(
        new PresenterError('webgpu_surface_error', `WebGPU getCurrentTexture() failed: ${message}`, currentTextureError),
      );
      return false;
    }

    let view: any = null;
    let viewError: unknown = null;
    try {
      view =
        this.viewFormat && this.canvasFormat && this.viewFormat !== this.canvasFormat
          ? currentTexture.createView({ format: this.viewFormat })
          : currentTexture.createView();
    } catch (err) {
      viewError = err;
    }

    if (!view) {
      // If we attempted an sRGB view format and the browser rejected it, fall back to the
      // default (linear) view and enable shader sRGB encoding so presentation remains correct.
      if (this.viewFormat && this.canvasFormat && this.viewFormat !== this.canvasFormat) {
        this.viewFormat = this.canvasFormat;
        this.srgbEncodeInShader = true;
        if (this.pipelineFormat !== this.viewFormat || !this.pipeline) {
          this.pipelineFormat = this.viewFormat;
          this.createPipelineAndSampler();
          this.rebuildBindGroup();
        }
        try {
          view = currentTexture.createView();
          viewError = null;
        } catch (err) {
          viewError = err;
        }
      }
    }

    if (!view) {
      const message = formatOneLineError(viewError, 512, "Unknown error");
      this.opts.onError?.(
        new PresenterError('webgpu_surface_error', `WebGPU currentTexture.createView() failed: ${message}`, viewError),
      );
      return false;
    }

    const flags =
      WebGpuPresenterBackend.FLAG_FORCE_OPAQUE_ALPHA |
      (this.srgbEncodeInShader ? WebGpuPresenterBackend.FLAG_APPLY_SRGB_ENCODE : 0);
    cursorParams[3] = flags | 0;
    this.queue.writeBuffer(this.cursorUniformBuffer, 0, cursorParams);

    const clearR = this.srgbEncodeInShader ? srgbEncodeChannel(r) : r;
    const clearG = this.srgbEncodeInShader ? srgbEncodeChannel(g) : g;
    const clearB = this.srgbEncodeInShader ? srgbEncodeChannel(b) : b;

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          // Alpha is forced opaque in shader; keep clear alpha opaque too so letterboxing doesn't
          // accidentally blend with the page background.
          clearValue: { r: clearR, g: clearG, b: clearB, a: 1 },
          loadOp: 'clear',
          storeOp: 'store',
        },
      ],
    });

    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.setViewport(vp.x, vp.y, vp.w, vp.h, 0, 1);
    pass.draw(3);
    pass.end();

    this.queue.submit([encoder.finish()]);
    return true;
  }

  private ensureCursorResources(): void {
    if (!this.device || !this.queue) return;

    if (!this.cursorUniformBuffer) {
      const usage =
        (webGpuGlobals.GPUBufferUsage?.UNIFORM ?? 0x10) | (webGpuGlobals.GPUBufferUsage?.COPY_DST ?? 0x08);
      // CursorUniforms is 3x vec4<i32> = 48 bytes.
      this.cursorUniformBuffer = this.device.createBuffer({ size: 48, usage });
    }

    if (!this.cursorTexture || !this.cursorView) {
      const usage =
        (webGpuGlobals.GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) | (webGpuGlobals.GPUTextureUsage?.COPY_DST ?? 0x02);
      this.cursorTexture = this.device.createTexture({
        size: { width: 1, height: 1, depthOrArrayLayers: 1 },
        format: 'rgba8unorm',
        usage,
      });
      this.cursorView = this.cursorTexture.createView();
      this.cursorTextureWidth = 1;
      this.cursorTextureHeight = 1;
    }
  }

  private rebuildBindGroup(): void {
    if (!this.device || !this.pipeline || !this.sampler || !this.frameView) return;
    if (!this.cursorView || !this.cursorUniformBuffer) return;
    this.bindGroup = this.device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: this.sampler },
        { binding: 1, resource: this.frameView },
        { binding: 2, resource: this.cursorView },
        { binding: 3, resource: { buffer: this.cursorUniformBuffer } },
      ],
    });
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

  private copyToStaging(data: Uint8Array, srcStride: number, dstStride: number): Uint8Array {
    const tightRowBytes = this.srcWidth * 4;
    const total = dstStride * this.srcHeight;
    if (!this.staging || this.staging.byteLength !== total || this.stagingBytesPerRow !== dstStride) {
      this.staging = new Uint8Array(total);
      this.stagingBytesPerRow = dstStride;
    }

    for (let y = 0; y < this.srcHeight; y++) {
      const srcOff = y * srcStride;
      const dstOff = y * dstStride;
      this.staging.set(data.subarray(srcOff, srcOff + tightRowBytes), dstOff);
      // Any padding bytes in the staging row remain from previous frames; make it deterministic.
      this.staging.fill(0, dstOff + tightRowBytes, dstOff + dstStride);
    }

    return this.staging;
  }

  private copyCursorToStaging(
    data: Uint8Array,
    srcStride: number,
    dstStride: number,
    width: number,
    height: number,
  ): Uint8Array {
    const tightRowBytes = width * 4;
    const total = dstStride * height;
    if (
      !this.cursorStaging ||
      this.cursorStaging.byteLength !== total ||
      this.cursorStagingBytesPerRow !== dstStride
    ) {
      this.cursorStaging = new Uint8Array(total);
      this.cursorStagingBytesPerRow = dstStride;
    }

    for (let y = 0; y < height; y++) {
      const srcOff = y * srcStride;
      const dstOff = y * dstStride;
      this.cursorStaging.set(data.subarray(srcOff, srcOff + tightRowBytes), dstOff);
      this.cursorStaging.fill(0, dstOff + tightRowBytes, dstOff + dstStride);
    }

    return this.cursorStaging;
  }
}
