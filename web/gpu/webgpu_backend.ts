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

  private pipeline: GPURenderPipeline | null = null;
  private sampler: GPUSampler | null = null;

  private frameTexture: GPUTexture | null = null;
  private frameTextureView: GPUTextureView | null = null;
  private bindGroup: GPUBindGroup | null = null;

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
    context.configure({ device, format, alphaMode: 'opaque' });

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
    this.pipeline = pipeline;
    this.sampler = sampler;
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

    if (canvas instanceof HTMLCanvasElement) {
      const dpr = window.devicePixelRatio || 1;
      const displayWidth = Math.max(1, Math.round(canvas.clientWidth * dpr));
      const displayHeight = Math.max(1, Math.round(canvas.clientHeight * dpr));
      if (canvas.width !== displayWidth || canvas.height !== displayHeight) {
        await device.queue.onSubmittedWorkDone();
        canvas.width = displayWidth;
        canvas.height = displayHeight;
        if (!this.format) throw new Error('Backend not initialized');
        context.configure({ device, format: this.format, alphaMode: 'opaque' });
      }
    }

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
    if (!device || !texture) throw new Error('Frame not available');

    const width = this.frameWidth;
    const height = this.frameHeight;

    const bytesPerRow = align(width * 4, 256);
    const buffer = device.createBuffer({
      size: bytesPerRow * height,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });

    const encoder = device.createCommandEncoder();
    encoder.copyTextureToBuffer({ texture }, { buffer, bytesPerRow }, { width, height });
    device.queue.submit([encoder.finish()]);

    await buffer.mapAsync(GPUMapMode.READ);
    const mapped = new Uint8Array(buffer.getMappedRange());
    const out = new Uint8ClampedArray(width * height * 4);
    for (let y = 0; y < height; y++) {
      out.set(mapped.subarray(y * bytesPerRow, y * bytesPerRow + width * 4), y * width * 4);
    }
    buffer.unmap();
    buffer.destroy();

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
