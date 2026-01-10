import type {
  BackendCapabilities,
  BackendInitOptions,
  CapturedFrame,
  DirtyRect,
  FilterMode,
  PresentationBackend,
} from './backend';

type MaybeCanvas = HTMLCanvasElement | OffscreenCanvas;

function toU8View(buffer: ArrayBufferView): Uint8Array {
  return buffer instanceof Uint8Array
    ? buffer
    : new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
}

function align(value: number, alignment: number): number {
  return Math.ceil(value / alignment) * alignment;
}

function isBgraFormat(format: GPUTextureFormat): boolean {
  return format === 'bgra8unorm' || format === 'bgra8unorm-srgb';
}

function computeLetterboxViewport(
  canvasWidth: number,
  canvasHeight: number,
  contentWidth: number,
  contentHeight: number,
): { x: number; y: number; width: number; height: number } {
  if (canvasWidth <= 0 || canvasHeight <= 0 || contentWidth <= 0 || contentHeight <= 0) {
    return { x: 0, y: 0, width: canvasWidth, height: canvasHeight };
  }

  const canvasAspect = canvasWidth / canvasHeight;
  const contentAspect = contentWidth / contentHeight;

  if (canvasAspect > contentAspect) {
    const height = canvasHeight;
    const width = Math.round(height * contentAspect);
    const x = Math.floor((canvasWidth - width) / 2);
    const y = 0;
    return { x, y, width, height };
  }

  const width = canvasWidth;
  const height = Math.round(width / contentAspect);
  const x = 0;
  const y = Math.floor((canvasHeight - height) / 2);
  return { x, y, width, height };
}

export class WebGPUBackend implements PresentationBackend {
  private canvas: MaybeCanvas | null = null;
  private context: GPUCanvasContext | null = null;

  private device: GPUDevice | null = null;
  private queue: GPUQueue | null = null;
  private format: GPUTextureFormat | null = null;
  private configuredCanvasWidth = 0;
  private configuredCanvasHeight = 0;

  private pipeline: GPURenderPipeline | null = null;
  private sampler: GPUSampler | null = null;

  private frameTexture: GPUTexture | null = null;
  private frameTextureView: GPUTextureView | null = null;
  private bindGroup: GPUBindGroup | null = null;
  private captureTexture: GPUTexture | null = null;
  private captureTextureView: GPUTextureView | null = null;
  private captureWidth = 0;
  private captureHeight = 0;

  private frameWidth = 0;
  private frameHeight = 0;

  private filterMode: FilterMode = 'nearest';
  private preserveAspectRatio = true;

  async init(canvas: MaybeCanvas, options?: BackendInitOptions): Promise<void> {
    this.filterMode = options?.filter ?? 'nearest';
    this.preserveAspectRatio = options?.preserveAspectRatio ?? true;

    if (!navigator.gpu) throw new Error('WebGPU unavailable');

    const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
    if (!adapter) throw new Error('No WebGPU adapter available');

    const device = await adapter.requestDevice();

    const context = (canvas as any).getContext('webgpu') as GPUCanvasContext | null;
    if (!context) throw new Error('Failed to acquire WebGPU canvas context');

    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({
      device,
      format,
      alphaMode: 'opaque',
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });

    const shaderModule = device.createShaderModule({
      code: `
        struct VertexOutput {
          @builtin(position) position: vec4<f32>,
          @location(0) uv: vec2<f32>,
        }

        @vertex
        fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
          var positions = array<vec2<f32>, 6>(
            vec2<f32>(-1.0, -1.0),
            vec2<f32>( 1.0, -1.0),
            vec2<f32>(-1.0,  1.0),
            vec2<f32>(-1.0,  1.0),
            vec2<f32>( 1.0, -1.0),
            vec2<f32>( 1.0,  1.0),
          );
          var uvs = array<vec2<f32>, 6>(
            vec2<f32>(0.0, 1.0),
            vec2<f32>(1.0, 1.0),
            vec2<f32>(0.0, 0.0),
            vec2<f32>(0.0, 0.0),
            vec2<f32>(1.0, 1.0),
            vec2<f32>(1.0, 0.0),
          );

          var out: VertexOutput;
          out.position = vec4<f32>(positions[idx], 0.0, 1.0);
          out.uv = uvs[idx];
          return out;
        }

        @group(0) @binding(0) var u_sampler: sampler;
        @group(0) @binding(1) var u_texture: texture_2d<f32>;

        @fragment
        fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
          return textureSample(u_texture, u_sampler, in.uv);
        }
      `,
    });

    const pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: {
        module: shaderModule,
        entryPoint: 'vs_main',
      },
      fragment: {
        module: shaderModule,
        entryPoint: 'fs_main',
        targets: [{ format }],
      },
      primitive: {
        topology: 'triangle-list',
        cullMode: 'none',
      },
    });

    const filter = this.filterMode === 'linear' ? 'linear' : 'nearest';
    const sampler = device.createSampler({ magFilter: filter, minFilter: filter });

    this.canvas = canvas;
    this.context = context;
    this.device = device;
    this.queue = device.queue;
    this.format = format;
    this.configuredCanvasWidth = (canvas as any).width as number;
    this.configuredCanvasHeight = (canvas as any).height as number;
    this.pipeline = pipeline;
    this.sampler = sampler;
  }

  private ensureCanvasConfigured(): void {
    const device = this.device;
    const context = this.context;
    const canvas = this.canvas;
    const format = this.format;
    if (!device || !context || !canvas || !format) throw new Error('Backend not initialized');

    const width = (canvas as any).width as number;
    const height = (canvas as any).height as number;

    if (width === this.configuredCanvasWidth && height === this.configuredCanvasHeight) return;

    context.configure({
      device,
      format,
      alphaMode: 'opaque',
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });
    this.configuredCanvasWidth = width;
    this.configuredCanvasHeight = height;
  }

  uploadFrameRGBA(
    buffer: ArrayBufferView,
    width: number,
    height: number,
    dirtyRects?: readonly DirtyRect[],
  ): void {
    const device = this.device;
    const queue = this.queue;
    if (!device || !queue) throw new Error('Backend not initialized');

    const data = toU8View(buffer);

    if (!this.frameTexture || width !== this.frameWidth || height !== this.frameHeight) {
      this.frameTexture?.destroy();
      this.frameTexture = device.createTexture({
        size: { width, height },
        format: 'rgba8unorm',
        usage:
          GPUTextureUsage.TEXTURE_BINDING |
          GPUTextureUsage.COPY_DST |
          GPUTextureUsage.COPY_SRC,
      });
      this.frameTextureView = this.frameTexture.createView();
      this.frameWidth = width;
      this.frameHeight = height;

      if (!this.pipeline || !this.sampler) throw new Error('Backend not initialized');

      this.bindGroup = device.createBindGroup({
        layout: this.pipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: this.sampler },
          { binding: 1, resource: this.frameTextureView },
        ],
      });
    }

    if (!this.frameTexture) throw new Error('Frame texture unavailable');

    const fullWrite = !dirtyRects || dirtyRects.length === 0;
    if (fullWrite) {
      this.writeTextureRegion(queue, this.frameTexture, data, 0, 0, width, height, width);
      return;
    }

    for (const rect of dirtyRects) {
      this.writeTextureRegion(queue, this.frameTexture, data, rect.x, rect.y, rect.width, rect.height, width);
    }
  }

  private writeTextureRegion(
    queue: GPUQueue,
    texture: GPUTexture,
    data: Uint8Array,
    dstX: number,
    dstY: number,
    regionWidth: number,
    regionHeight: number,
    srcFullWidth: number,
  ) {
    const tightBytesPerRow = regionWidth * 4;
    const alignedBytesPerRow = align(tightBytesPerRow, 256);

    let upload: Uint8Array;
    if (alignedBytesPerRow === tightBytesPerRow && dstX === 0 && dstY === 0 && regionWidth === srcFullWidth) {
      upload = data;
    } else {
      upload = new Uint8Array(alignedBytesPerRow * regionHeight);
      const srcBytesPerRow = srcFullWidth * 4;
      for (let row = 0; row < regionHeight; row++) {
        const srcStart = ((dstY + row) * srcFullWidth + dstX) * 4;
        const dstStart = row * alignedBytesPerRow;
        upload.set(data.subarray(srcStart, srcStart + tightBytesPerRow), dstStart);
      }
    }

    queue.writeTexture(
      { texture, origin: { x: dstX, y: dstY } },
      upload,
      { bytesPerRow: alignedBytesPerRow, rowsPerImage: regionHeight },
      { width: regionWidth, height: regionHeight },
    );
  }

  async present(): Promise<void> {
    const device = this.device;
    const context = this.context;
    const pipeline = this.pipeline;
    const bindGroup = this.bindGroup;

    if (!device || !context || !pipeline || !bindGroup) throw new Error('Backend not initialized');

    const canvas = this.canvas;
    if (!canvas) throw new Error('Backend not initialized');

    if (typeof HTMLCanvasElement !== 'undefined' && canvas instanceof HTMLCanvasElement) {
      const dpr = window.devicePixelRatio || 1;
      const displayWidth = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const displayHeight = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== displayWidth || canvas.height !== displayHeight) {
        await device.queue.onSubmittedWorkDone();
        canvas.width = displayWidth;
        canvas.height = displayHeight;
      }
    }

    this.ensureCanvasConfigured();

    const textureView = context.getCurrentTexture().createView();

    const encoder = device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: textureView,
          loadOp: 'clear',
          storeOp: 'store',
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });

    const canvasWidth = (canvas as any).width as number;
    const canvasHeight = (canvas as any).height as number;
    const viewport = this.preserveAspectRatio
      ? computeLetterboxViewport(canvasWidth, canvasHeight, this.frameWidth, this.frameHeight)
      : { x: 0, y: 0, width: canvasWidth, height: canvasHeight };
    pass.setViewport(viewport.x, viewport.y, viewport.width, viewport.height, 0, 1);
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.draw(6, 1, 0, 0);
    pass.end();

    device.queue.submit([encoder.finish()]);
    await device.queue.onSubmittedWorkDone();
  }

  async captureFrame(): Promise<CapturedFrame> {
    const device = this.device;
    const texture = this.frameTexture;
    const pipeline = this.pipeline;
    const bindGroup = this.bindGroup;
    const canvas = this.canvas;
    const format = this.format;
    if (!device || !texture || !pipeline || !bindGroup || !canvas || !format) {
      throw new Error('Frame not available');
    }

    this.ensureCanvasConfigured();

    const width = (canvas as any).width as number;
    const height = (canvas as any).height as number;
    if (width <= 0 || height <= 0) throw new Error('Canvas size is invalid');

    if (!this.captureTexture || width !== this.captureWidth || height !== this.captureHeight) {
      this.captureTexture?.destroy();
      this.captureTexture = device.createTexture({
        size: { width, height },
        format,
        usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
      });
      this.captureTextureView = this.captureTexture.createView();
      this.captureWidth = width;
      this.captureHeight = height;
    }

    const captureView = this.captureTextureView;
    const captureTex = this.captureTexture;
    if (!captureView || !captureTex) throw new Error('Capture texture unavailable');

    const encoder = device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: captureView,
          loadOp: 'clear',
          storeOp: 'store',
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });

    const viewport = this.preserveAspectRatio
      ? computeLetterboxViewport(width, height, this.frameWidth, this.frameHeight)
      : { x: 0, y: 0, width, height };
    pass.setViewport(viewport.x, viewport.y, viewport.width, viewport.height, 0, 1);
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.draw(6, 1, 0, 0);
    pass.end();

    const bytesPerRow = align(width * 4, 256);
    const buffer = device.createBuffer({
      size: bytesPerRow * height,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });

    encoder.copyTextureToBuffer({ texture: captureTex }, { buffer, bytesPerRow }, { width, height });
    device.queue.submit([encoder.finish()]);

    await buffer.mapAsync(GPUMapMode.READ);
    const mapped = new Uint8Array(buffer.getMappedRange());
    const out = new Uint8ClampedArray(width * height * 4);
    for (let y = 0; y < height; y++) {
      out.set(mapped.subarray(y * bytesPerRow, y * bytesPerRow + width * 4), y * width * 4);
    }
    buffer.unmap();
    buffer.destroy();

    if (isBgraFormat(format)) {
      for (let i = 0; i < out.length; i += 4) {
        const b = out[i + 0];
        out[i + 0] = out[i + 2];
        out[i + 2] = b;
      }
    }

    return { width, height, data: out };
  }

  getCapabilities(): BackendCapabilities {
    return {
      kind: 'webgpu',
      supportsDirtyRects: true,
      supportsCapture: true,
    };
  }
}
