import blitShaderSource from './shaders/blit.wgsl?raw';
import type { Presenter, PresenterInitOptions, PresenterScaleMode, PresenterScreenshot } from './presenter';
import { PresenterError } from './presenter';
import type { DirtyRect } from '../ipc/shared-layout';
import { packRgba8RectToAlignedBuffer, type PackedRect } from './webgpu-rect-pack';

type Viewport = { x: number; y: number; w: number; h: number };

function alignUp(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

function computeViewport(
  canvasWidthPx: number,
  canvasHeightPx: number,
  srcWidth: number,
  srcHeight: number,
  mode: PresenterScaleMode,
): Viewport {
  if (canvasWidthPx <= 0 || canvasHeightPx <= 0 || srcWidth <= 0 || srcHeight <= 0) {
    return { x: 0, y: 0, w: 0, h: 0 };
  }

  if (mode === 'stretch') {
    return { x: 0, y: 0, w: canvasWidthPx, h: canvasHeightPx };
  }

  const scaleFit = Math.min(canvasWidthPx / srcWidth, canvasHeightPx / srcHeight);
  let scale = scaleFit;

  if (mode === 'integer') {
    const integerScale = Math.floor(scaleFit);
    scale = integerScale >= 1 ? integerScale : scaleFit;
  }

  const w = Math.max(1, Math.floor(srcWidth * scale));
  const h = Math.max(1, Math.floor(srcHeight * scale));
  const x = Math.floor((canvasWidthPx - w) / 2);
  const y = Math.floor((canvasHeightPx - h) / 2);
  return { x, y, w, h };
}

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
  private format: any = null;

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

    const outputWidth = this.opts.outputWidth ?? width;
    const outputHeight = this.opts.outputHeight ?? height;
    this.resizeCanvas(outputWidth, outputHeight, dpr);

    const gpu = (navigator as any).gpu;
    if (!gpu) {
      throw new PresenterError('webgpu_unavailable', 'WebGPU is not available in this environment');
    }
    this.gpu = gpu;

    const isHeadless = /HeadlessChrome/.test((navigator as any).userAgent ?? '');
    // Prefer the fallback adapter in headless/test runs for stability and deterministic output.
    // (We still try the "high-performance" path as a backup so real browsers can use hardware.)
    let adapter = await gpu.requestAdapter?.(isHeadless ? { forceFallbackAdapter: true } : { powerPreference: 'high-performance' });
    if (!adapter) {
      adapter = await gpu.requestAdapter?.(isHeadless ? { powerPreference: 'high-performance' } : { forceFallbackAdapter: true });
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

    const ctx = (canvas as any).getContext('webgpu') as any;
    if (!ctx) {
      try {
        device.destroy?.();
      } catch {
        // Ignore.
      }
      throw new PresenterError('webgpu_context_unavailable', 'Failed to create a WebGPU canvas context');
    }
    this.ctx = ctx;

    // Report device loss asynchronously.
    (device.lost as Promise<any> | undefined)?.then((info) => {
      if (this.destroyed) return;
      if (this.device !== device) return;
      const reason = info?.reason ?? 'unknown';
      const message = info?.message ? `: ${info.message}` : '';
      this.opts.onError?.(new PresenterError('webgpu_device_lost', `WebGPU device lost (${reason})${message}`));
    });

    this.format = gpu.getPreferredCanvasFormat?.() ?? 'bgra8unorm';
    this.configureContext();
    this.createPipelineAndSampler();
    this.createFrameResources(width, height);
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

  public present(frame: number | ArrayBuffer | ArrayBufferView, stride: number): void {
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

    this.renderToCanvas();
  }

  public presentDirtyRects(
    frame: number | ArrayBuffer | ArrayBufferView,
    stride: number,
    dirtyRects: DirtyRect[],
  ): void {
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

    if (!dirtyRects || dirtyRects.length === 0) {
      this.present(frame, stride);
      return;
    }

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
        buffer,
        layout,
        size,
      );
    }

    if (!anyUploaded) {
      // Defensive fallback: if all rects were invalid after clamping, do a full upload so the
      // consumer does not keep presenting a stale frame.
      this.present(frame, stride);
      return;
    }

    this.renderToCanvas();
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
      ((globalThis as any).GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) |
      ((globalThis as any).GPUTextureUsage?.COPY_DST ?? 0x02);

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

  public async screenshot(): Promise<PresenterScreenshot> {
    if (!this.device || !this.queue || !this.frameTexture) {
      throw new PresenterError('not_initialized', 'WebGpuPresenterBackend.screenshot() called before init()');
    }

    const bytesPerRowTight = this.srcWidth * 4;
    const bytesPerRow = alignUp(bytesPerRowTight, 256);
    const bufferSize = bytesPerRow * this.srcHeight;

    const readback = this.device.createBuffer({
      size: bufferSize,
      usage: ((globalThis as any).GPUBufferUsage?.COPY_DST ?? 0x08) | ((globalThis as any).GPUBufferUsage?.MAP_READ ?? 0x01),
    });

    const encoder = this.device.createCommandEncoder();
    encoder.copyTextureToBuffer(
      { texture: this.frameTexture },
      { buffer: readback, bytesPerRow, rowsPerImage: this.srcHeight },
      { width: this.srcWidth, height: this.srcHeight, depthOrArrayLayers: 1 },
    );

    this.queue.submit([encoder.finish()]);

    const mapModeRead = (globalThis as any).GPUMapMode?.READ ?? 0x0001;
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

  public destroy(): void {
    this.destroyed = true;
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
    this.device = null;
    this.queue = null;
    this.ctx = null;
    this.canvas = null;
    this.staging = null;
    this.stagingBytesPerRow = 0;
    this.dirtyRectStaging = null;
    this.cursorStaging = null;
    this.cursorStagingBytesPerRow = 0;
  }

  private resizeCanvas(outputWidth: number, outputHeight: number, dpr: number): void {
    if (!this.canvas) return;
    this.canvas.width = Math.max(1, Math.round(outputWidth * dpr));
    this.canvas.height = Math.max(1, Math.round(outputHeight * dpr));
  }

  private configureContext(): void {
    if (!this.ctx || !this.device || !this.format) return;
    try {
      this.ctx.unconfigure?.();
    } catch {
      // Ignore.
    }
    this.ctx.configure({
      device: this.device,
      format: this.format,
      alphaMode: 'opaque',
    });
  }

  private createPipelineAndSampler(): void {
    if (!this.device) return;

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
        targets: [{ format: this.format }],
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
      ((globalThis as any).GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) |
      ((globalThis as any).GPUTextureUsage?.COPY_DST ?? 0x02) |
      ((globalThis as any).GPUTextureUsage?.COPY_SRC ?? 0x01);

    this.frameTexture = this.device.createTexture({
      size: { width, height, depthOrArrayLayers: 1 },
      format: 'rgba8unorm',
      usage,
    });
    this.frameView = this.frameTexture.createView();
    this.ensureCursorResources();
    this.rebuildBindGroup();
  }

  private renderToCanvas(): void {
    if (
      !this.canvas ||
      !this.device ||
      !this.queue ||
      !this.ctx ||
      !this.pipeline ||
      !this.bindGroup ||
      !this.cursorUniformBuffer
    ) {
      return;
    }

    const cursorEnable =
      this.cursorRenderEnabled && this.cursorEnabled && this.cursorWidth > 0 && this.cursorHeight > 0 ? 1 : 0;
    const cursorParams = new Int32Array([
      this.srcWidth | 0,
      this.srcHeight | 0,
      cursorEnable,
      0,
      this.cursorX | 0,
      this.cursorY | 0,
      this.cursorHotX | 0,
      this.cursorHotY | 0,
      this.cursorWidth | 0,
      this.cursorHeight | 0,
      0,
      0,
    ]);
    this.queue.writeBuffer(this.cursorUniformBuffer, 0, cursorParams);

    const canvasW = this.canvas.width;
    const canvasH = this.canvas.height;
    const scaleMode = this.opts.scaleMode ?? 'fit';

    const vp = computeViewport(canvasW, canvasH, this.srcWidth, this.srcHeight, scaleMode);
    const [r, g, b, a] = this.opts.clearColor ?? [0, 0, 0, 1];

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: this.ctx.getCurrentTexture().createView(),
          clearValue: { r, g, b, a },
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
  }

  private ensureCursorResources(): void {
    if (!this.device || !this.queue) return;

    if (!this.cursorUniformBuffer) {
      const usage =
        ((globalThis as any).GPUBufferUsage?.UNIFORM ?? 0x10) | ((globalThis as any).GPUBufferUsage?.COPY_DST ?? 0x08);
      // CursorUniforms is 3x vec4<i32> = 48 bytes.
      this.cursorUniformBuffer = this.device.createBuffer({ size: 48, usage });
    }

    if (!this.cursorTexture || !this.cursorView) {
      const usage =
        ((globalThis as any).GPUTextureUsage?.TEXTURE_BINDING ?? 0x04) |
        ((globalThis as any).GPUTextureUsage?.COPY_DST ?? 0x02);
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
