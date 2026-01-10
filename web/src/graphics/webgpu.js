import { bgra8ToRgba8, indexed8ToRgba8, rgb565ToRgba8 } from './convert.js';

/**
 * @param {ArrayBufferView} view
 * @returns {Uint8Array}
 */
function asU8(view) {
  return new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
}

/**
 * @param {ArrayBufferView} view
 * @returns {Uint16Array}
 */
function asU16(view) {
  if (view instanceof Uint16Array) return view;
  if (view.byteOffset % 2 !== 0 || view.byteLength % 2 !== 0) {
    throw new Error('rgb565 buffers must be 2-byte aligned');
  }
  return new Uint16Array(view.buffer, view.byteOffset, view.byteLength / 2);
}

/**
 * @param {import('./index.js').Framebuffer} framebuffer
 * @returns {{ width: number, height: number, rgba8: Uint8Array }}
 */
function toRgba8(framebuffer) {
  if (framebuffer.format === 'rgba8') {
    return {
      width: framebuffer.width,
      height: framebuffer.height,
      rgba8: asU8(framebuffer.data),
    };
  }

  if (framebuffer.format === 'bgra8') {
    return {
      width: framebuffer.width,
      height: framebuffer.height,
      rgba8: bgra8ToRgba8(asU8(framebuffer.data)),
    };
  }

  if (framebuffer.format === 'rgb565') {
    return {
      width: framebuffer.width,
      height: framebuffer.height,
      rgba8: rgb565ToRgba8(asU16(framebuffer.data)),
    };
  }

  if (framebuffer.format === 'indexed8') {
    if (!framebuffer.paletteRgba8) throw new Error('indexed8 framebuffer missing paletteRgba8');
    return {
      width: framebuffer.width,
      height: framebuffer.height,
      rgba8: indexed8ToRgba8(asU8(framebuffer.data), framebuffer.paletteRgba8),
    };
  }

  throw new Error(`Unsupported framebuffer format: ${framebuffer.format}`);
}

/**
 * @param {number} value
 * @param {number} alignment
 * @returns {number}
 */
function alignUp(value, alignment) {
  return Math.ceil(value / alignment) * alignment;
}

/**
 * Build quad vertices for a destination rect in pixels (top-left origin).
 *
 * Vertex format: vec2(position_clip) + vec2(uv)
 *
 * @param {number} x
 * @param {number} y
 * @param {number} w
 * @param {number} h
 * @param {number} canvasWidth
 * @param {number} canvasHeight
 * @returns {Float32Array}
 */
function quadVertsForRect(x, y, w, h, canvasWidth, canvasHeight) {
  const l = (x / canvasWidth) * 2 - 1;
  const r = ((x + w) / canvasWidth) * 2 - 1;
  const t = 1 - (y / canvasHeight) * 2;
  const b = 1 - ((y + h) / canvasHeight) * 2;

  return new Float32Array([
    l,
    b,
    0,
    1,
    r,
    b,
    1,
    1,
    l,
    t,
    0,
    0,
    l,
    t,
    0,
    0,
    r,
    b,
    1,
    1,
    r,
    t,
    1,
    0,
  ]);
}

export class WebGpuBackend {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {GPUDevice} device
   * @param {GPUCanvasContext} context
   * @param {GPUTextureFormat} canvasFormat
   */
  constructor(canvas, device, context, canvasFormat) {
    this.kind = 'webgpu';
    this.canvas = canvas;
    this.device = device;
    this.context = context;
    this.canvasFormat = canvasFormat;

    this._sampler = device.createSampler({
      magFilter: 'nearest',
      minFilter: 'nearest',
    });

    const shader = device.createShaderModule({
      code: `
        struct VertexIn {
          @location(0) pos : vec2<f32>,
          @location(1) uv : vec2<f32>,
        };

        struct VSOut {
          @builtin(position) pos : vec4<f32>,
          @location(0) uv : vec2<f32>,
        };

        @vertex
        fn vs_main(in: VertexIn) -> VSOut {
          var out : VSOut;
          out.pos = vec4<f32>(in.pos, 0.0, 1.0);
          out.uv = in.uv;
          return out;
        }

        @group(0) @binding(0) var frameSampler: sampler;
        @group(0) @binding(1) var frameTex: texture_2d<f32>;

        @fragment
        fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
          return textureSample(frameTex, frameSampler, in.uv);
        }
      `,
    });

    this._pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: {
        module: shader,
        entryPoint: 'vs_main',
        buffers: [
          {
            arrayStride: 16,
            attributes: [
              { shaderLocation: 0, offset: 0, format: 'float32x2' },
              { shaderLocation: 1, offset: 8, format: 'float32x2' },
            ],
          },
        ],
      },
      fragment: {
        module: shader,
        entryPoint: 'fs_main',
        targets: [
          {
            format: canvasFormat,
            blend: {
              color: {
                srcFactor: 'src-alpha',
                dstFactor: 'one-minus-src-alpha',
                operation: 'add',
              },
              alpha: {
                srcFactor: 'one',
                dstFactor: 'one-minus-src-alpha',
                operation: 'add',
              },
            },
          },
        ],
      },
      primitive: { topology: 'triangle-list' },
    });

    this._frame = null;
    this._overlaySlots = [];

    this._vertexBuffer = null;
    this._vertexBufferCapacityBytes = 0;
  }

  /**
   * @param {HTMLCanvasElement} canvas
   * @returns {Promise<WebGpuBackend>}
   */
  static async create(canvas) {
    if (!navigator.gpu) throw new Error('WebGPU not available');

    const adapter = await navigator.gpu.requestAdapter({ powerPreference: 'high-performance' });
    if (!adapter) throw new Error('WebGPU adapter not available');

    const device = await adapter.requestDevice();

    const context = canvas.getContext('webgpu');
    if (!context) throw new Error('Failed to create WebGPU canvas context');

    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({ device, format, alphaMode: 'opaque' });

    return new WebGpuBackend(canvas, device, context, format);
  }

  /**
   * @param {number} width
   * @param {number} height
   */
  _ensureTexture(width, height) {
    if (this._frame && this._frame.width === width && this._frame.height === height) return;

    const texture = this.device.createTexture({
      size: { width, height },
      format: 'rgba8unorm',
      usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
    });

    const bindGroup = this.device.createBindGroup({
      layout: this._pipeline.getBindGroupLayout(0),
      entries: [
        { binding: 0, resource: this._sampler },
        { binding: 1, resource: texture.createView() },
      ],
    });

    this._frame = { width, height, texture, bindGroup };
  }

  /**
   * @param {GPUTexture} texture
   * @param {Uint8Array} rgba8
   * @param {number} width
   * @param {number} height
   */
  _uploadRgba8(texture, rgba8, width, height) {
    const bytesPerRow = width * 4;
    const paddedBytesPerRow = alignUp(bytesPerRow, 256);

    if (paddedBytesPerRow === bytesPerRow) {
      this.device.queue.writeTexture(
        { texture },
        rgba8,
        { bytesPerRow, rowsPerImage: height },
        { width, height },
      );
      return;
    }

    const padded = new Uint8Array(paddedBytesPerRow * height);
    for (let y = 0; y < height; y++) {
      padded.set(rgba8.subarray(y * bytesPerRow, y * bytesPerRow + bytesPerRow), y * paddedBytesPerRow);
    }

    this.device.queue.writeTexture(
      { texture },
      padded,
      { bytesPerRow: paddedBytesPerRow, rowsPerImage: height },
      { width, height },
    );
  }

  /**
   * @param {import('./index.js').Framebuffer} framebuffer
   * @param {import('./index.js').Blit[]} [blits]
   */
  present(framebuffer, blits = []) {
    const { width, height, rgba8 } = toRgba8(framebuffer);

    this.canvas.width = width;
    this.canvas.height = height;

    this._ensureTexture(width, height);
    this._uploadRgba8(this._frame.texture, rgba8, width, height);

    const quadCount = 1 + blits.length;
    const bytesNeeded = quadCount * 6 * 16;
    if (!this._vertexBuffer || this._vertexBufferCapacityBytes < bytesNeeded) {
      if (this._vertexBuffer) this._vertexBuffer.destroy();
      this._vertexBuffer = this.device.createBuffer({
        size: bytesNeeded,
        usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST,
      });
      this._vertexBufferCapacityBytes = bytesNeeded;
    }

    const vertices = new Float32Array(quadCount * 6 * 4);
    vertices.set(quadVertsForRect(0, 0, width, height, width, height), 0);

    for (let i = 0; i < blits.length; i++) {
      const blit = blits[i];
      const { rgba8: blitRgba } = toRgba8({
        width: blit.width,
        height: blit.height,
        format: blit.format,
        data: blit.data,
        paletteRgba8: blit.paletteRgba8,
      });

      let slot = this._overlaySlots[i];
      if (!slot || slot.width !== blit.width || slot.height !== blit.height) {
        if (slot) slot.texture.destroy();
        const texture = this.device.createTexture({
          size: { width: blit.width, height: blit.height },
          format: 'rgba8unorm',
          usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
        });
        const bindGroup = this.device.createBindGroup({
          layout: this._pipeline.getBindGroupLayout(0),
          entries: [
            { binding: 0, resource: this._sampler },
            { binding: 1, resource: texture.createView() },
          ],
        });
        slot = { width: blit.width, height: blit.height, texture, bindGroup };
        this._overlaySlots[i] = slot;
      }

      this._uploadRgba8(slot.texture, blitRgba, blit.width, blit.height);

      vertices.set(
        quadVertsForRect(blit.x, blit.y, blit.width, blit.height, width, height),
        (i + 1) * 6 * 4,
      );
    }

    this.device.queue.writeBuffer(this._vertexBuffer, 0, vertices);

    const encoder = this.device.createCommandEncoder();
    const view = this.context.getCurrentTexture().createView();

    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          loadOp: 'clear',
          storeOp: 'store',
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });

    pass.setPipeline(this._pipeline);
    pass.setVertexBuffer(0, this._vertexBuffer);
    pass.setBindGroup(0, this._frame.bindGroup);
    pass.draw(6, 1, 0, 0);

    for (let i = 0; i < blits.length; i++) {
      const slot = this._overlaySlots[i];
      pass.setBindGroup(0, slot.bindGroup);
      pass.draw(6, 1, (i + 1) * 6, 0);
    }

    pass.end();

    this.device.queue.submit([encoder.finish()]);
  }

  drawTestTriangle() {
    // A minimal triangle pass (no buffers) to validate pipeline functionality.
    const device = this.device;
    const format = this.canvasFormat;

    if (!this._trianglePipeline) {
      const shader = device.createShaderModule({
        code: `
          struct VSOut {
            @builtin(position) pos: vec4<f32>,
            @location(0) color: vec3<f32>,
          };

          @vertex
          fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
            var pos = array<vec2<f32>, 3>(
              vec2<f32>(0.0, 0.8),
              vec2<f32>(-0.8, -0.8),
              vec2<f32>(0.8, -0.8),
            );
            var col = array<vec3<f32>, 3>(
              vec3<f32>(1.0, 0.2, 0.2),
              vec3<f32>(0.2, 1.0, 0.2),
              vec3<f32>(0.2, 0.2, 1.0),
            );
            var out: VSOut;
            out.pos = vec4<f32>(pos[vid], 0.0, 1.0);
            out.color = col[vid];
            return out;
          }

          @fragment
          fn fs_main(in: VSOut) -> @location(0) vec4<f32> {
            return vec4<f32>(in.color, 1.0);
          }
        `,
      });
      this._trianglePipeline = device.createRenderPipeline({
        layout: 'auto',
        vertex: { module: shader, entryPoint: 'vs_main' },
        fragment: { module: shader, entryPoint: 'fs_main', targets: [{ format }] },
        primitive: { topology: 'triangle-list' },
      });
    }

    const encoder = device.createCommandEncoder();
    const view = this.context.getCurrentTexture().createView();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          loadOp: 'clear',
          storeOp: 'store',
          clearValue: { r: 0.05, g: 0.05, b: 0.08, a: 1 },
        },
      ],
    });
    pass.setPipeline(this._trianglePipeline);
    pass.draw(3);
    pass.end();
    device.queue.submit([encoder.finish()]);
  }

  destroy() {
    // WebGPU resources are GC'd, but explicit cleanup is still helpful.
    if (this._frame) this._frame.texture.destroy();
    for (const slot of this._overlaySlots) {
      if (slot) slot.texture.destroy();
    }
    if (this._vertexBuffer) this._vertexBuffer.destroy();
  }
}
