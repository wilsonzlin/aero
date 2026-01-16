/** @typedef {'webgpu' | 'webgl2'} BackendKind */

import { formatOneLineError } from '../text';

/**
 * @param {unknown} err
 */
function stringifyError(err) {
  return formatOneLineError(err, 512);
}

/**
 * @param {Uint8Array} bytes
 */
async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, '0')).join('');
}

/**
 * @param {number[]} values
 */
function stats(values) {
  if (values.length === 0) {
    return { min: 0, median: 0, p95: 0 };
  }
  const sorted = [...values].sort((a, b) => a - b);
  const min = sorted[0];
  const median =
    sorted.length % 2 === 1
      ? sorted[(sorted.length - 1) / 2]
      : (sorted[sorted.length / 2 - 1] + sorted[sorted.length / 2]) / 2;
  const p95 = sorted[Math.floor(0.95 * (sorted.length - 1))];
  return { min, median, p95 };
}

class WebGl2Backend {
  /**
   * @param {WebGL2RenderingContext} gl
   * @param {number} width
   * @param {number} height
   */
  constructor(gl, width, height) {
    this.gl = gl;
    this.width = width;
    this.height = height;
    this.benchUploadScratch = new Uint8Array(width * height * 4);
    const benchBuffer = gl.createBuffer();
    if (!benchBuffer) throw new Error('WebGL2 createBuffer failed');
    this.benchBuffer = benchBuffer;
    gl.bindBuffer(gl.ARRAY_BUFFER, benchBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, this.benchUploadScratch.byteLength, gl.DYNAMIC_DRAW);
  }

  /**
   * @param {OffscreenCanvas} canvas
   * @param {number} width
   * @param {number} height
   */
  static async create(canvas, width, height) {
    canvas.width = width;
    canvas.height = height;
    const gl = canvas.getContext('webgl2', {
      alpha: false,
      antialias: false,
      depth: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer: true,
      stencil: false,
    });
    if (!gl) throw new Error('WebGL2 context unavailable');
    // Deterministic presentation: disable sources of driver variance and ensure a clean state.
    gl.disable(gl.DITHER);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);
    gl.disable(gl.BLEND);
    gl.disable(gl.SCISSOR_TEST);
    gl.disable(gl.STENCIL_TEST);
    gl.disable(gl.SAMPLE_ALPHA_TO_COVERAGE);
    gl.disable(gl.SAMPLE_COVERAGE);
    gl.colorMask(true, true, true, true);
    gl.viewport(0, 0, width, height);
    return new WebGl2Backend(gl, width, height);
  }

  getCapabilities() {
    const gl = this.gl;
    return {
      vendor: gl.getParameter(gl.VENDOR),
      renderer: gl.getParameter(gl.RENDERER),
      version: gl.getParameter(gl.VERSION),
      shadingLanguageVersion: gl.getParameter(gl.SHADING_LANGUAGE_VERSION),
    };
  }

  async presentTestPattern() {
    const gl = this.gl;
    const halfW = this.width / 2;
    const halfH = this.height / 2;

    gl.disable(gl.BLEND);
    gl.enable(gl.SCISSOR_TEST);

    // Coordinate space is bottom-left; we build the pattern in *display* space
    // (top-left origin) by mapping top/bottom quadrants accordingly.
    gl.scissor(0, halfH, halfW, halfH);
    gl.clearColor(1, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(halfW, halfH, halfW, halfH);
    gl.clearColor(0, 1, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(0, 0, halfW, halfH);
    gl.clearColor(0, 0, 1, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.scissor(halfW, 0, halfW, halfH);
    gl.clearColor(1, 1, 1, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    gl.disable(gl.SCISSOR_TEST);
  }

  async screenshotRgba() {
    // Smoke-test screenshot: read back the *rendered output*.
    //
    // This worker is a standalone GPU backend smoke test (WebGPU vs WebGL2) and
    // intentionally captures "what was rendered" (default framebuffer) for hashing.
    // It is not the same contract as the runtime `Presenter.screenshot()` API, which
    // is defined as a deterministic readback of the source framebuffer bytes.
    const gl = this.gl;
    const rowBytes = this.width * 4;
    const raw = new Uint8Array(this.width * this.height * 4);
    gl.readPixels(0, 0, this.width, this.height, gl.RGBA, gl.UNSIGNED_BYTE, raw);

    // WebGL readPixels returns rows bottom-to-top. Normalize to top-to-bottom.
    const flipped = new Uint8Array(raw.length);
    for (let y = 0; y < this.height; y++) {
      const src = (this.height - 1 - y) * rowBytes;
      const dst = y * rowBytes;
      flipped.set(raw.subarray(src, src + rowBytes), dst);
    }
    return flipped;
  }

  /**
   * @param {number} frames
   */
  async benchPresent(frames) {
    const times = [];
    for (let i = 0; i < frames; i++) {
      this.benchUploadScratch[0] = i & 0xff;
      const t0 = performance.now();
      this.gl.bindBuffer(this.gl.ARRAY_BUFFER, this.benchBuffer);
      this.gl.bufferSubData(this.gl.ARRAY_BUFFER, 0, this.benchUploadScratch);
      await this.presentTestPattern();
      this.gl.flush();
      const t1 = performance.now();
      times.push(t1 - t0);
    }
    return {
      backend: /** @type {BackendKind} */ ('webgl2'),
      frames,
      timingsMs: stats(times),
      capabilities: this.getCapabilities(),
    };
  }
}

class WebGpuBackend {
  /**
   * @param {GPUDevice} device
   * @param {OffscreenCanvas} canvas
   * @param {GPUCanvasContext} context
   * @param {GPUTextureFormat} format
   * @param {GPURenderPipeline} pipeline
   * @param {GPUBindGroup} bindGroup
   * @param {GPUBuffer} uniformBuffer
   * @param {GPUTexture} readbackTexture
   * @param {GPUTextureView} readbackTextureView
   * @param {number} width
   * @param {number} height
   * @param {unknown} capabilities
   */
  constructor(
    device,
    canvas,
    context,
    format,
    pipeline,
    bindGroup,
    uniformBuffer,
    readbackTexture,
    readbackTextureView,
    benchUploadBuffer,
    benchUploadScratch,
    width,
    height,
    capabilities,
  ) {
    this.device = device;
    this.canvas = canvas;
    this.context = context;
    this.format = format;
    this.pipeline = pipeline;
    this.bindGroup = bindGroup;
    this.uniformBuffer = uniformBuffer;
    this.readbackTexture = readbackTexture;
    this.readbackTextureView = readbackTextureView;
    this.benchUploadBuffer = benchUploadBuffer;
    this.benchUploadScratch = benchUploadScratch;
    this.width = width;
    this.height = height;
    this.capabilities = capabilities;
    this.hasPresentedFrame = false;
  }

  /**
   * @param {OffscreenCanvas} canvas
   * @param {number} width
   * @param {number} height
   */
  static async create(canvas, width, height) {
    if (!navigator.gpu) throw new Error('WebGPU unavailable (navigator.gpu is missing)');
    const adapter = await navigator.gpu.requestAdapter();
    if (!adapter) throw new Error('WebGPU adapter unavailable');
    const device = await adapter.requestDevice();

    canvas.width = width;
    canvas.height = height;

    const context = canvas.getContext('webgpu');
    if (!context) throw new Error('Failed to acquire WebGPU canvas context');

    const format = navigator.gpu.getPreferredCanvasFormat();
    context.configure({
      device,
      format,
      alphaMode: 'opaque',
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });

    const readbackTexture = device.createTexture({
      size: { width, height, depthOrArrayLayers: 1 },
      format,
      usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
    });
    const readbackTextureView = readbackTexture.createView();

    const shader = device.createShaderModule({
      code: `
        struct Uniforms {
          size: vec2<f32>,
          _pad: vec2<f32>,
        }

        @group(0) @binding(0) var<uniform> u: Uniforms;

        struct VsOut {
          @builtin(position) position: vec4<f32>,
        }

        @vertex
        fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
          var positions = array<vec2<f32>, 3>(
            vec2<f32>(-1.0, -1.0),
            vec2<f32>(3.0, -1.0),
            vec2<f32>(-1.0, 3.0),
          );
          var out: VsOut;
          out.position = vec4<f32>(positions[i], 0.0, 1.0);
          return out;
        }

        @fragment
        fn fs_main(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
          let half = u.size * 0.5;
          let isLeft = p.x < half.x;
          let isTop = p.y < half.y;
          if (isLeft && isTop) {
            return vec4<f32>(1.0, 0.0, 0.0, 1.0);
          }
          if (!isLeft && isTop) {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
          }
          if (isLeft && !isTop) {
            return vec4<f32>(0.0, 0.0, 1.0, 1.0);
          }
          return vec4<f32>(1.0, 1.0, 1.0, 1.0);
        }
      `,
    });

    const pipeline = device.createRenderPipeline({
      layout: 'auto',
      vertex: {
        module: shader,
        entryPoint: 'vs_main',
      },
      fragment: {
        module: shader,
        entryPoint: 'fs_main',
        targets: [{ format }],
      },
      primitive: { topology: 'triangle-list' },
    });

    const uniformBuffer = device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });
    device.queue.writeBuffer(uniformBuffer, 0, new Float32Array([width, height, 0, 0]));

    const bindGroup = device.createBindGroup({
      layout: pipeline.getBindGroupLayout(0),
      entries: [{ binding: 0, resource: { buffer: uniformBuffer } }],
    });

    const benchUploadBuffer = device.createBuffer({
      size: width * height * 4,
      usage: GPUBufferUsage.COPY_DST,
    });
    const benchUploadScratch = new Uint8Array(width * height * 4);

    const capabilities = {
      format,
      adapterFeatures: Array.from(adapter.features.values()),
      deviceLimits: {
        maxTextureDimension2D: device.limits.maxTextureDimension2D,
        maxBindGroups: device.limits.maxBindGroups,
      },
    };

    return new WebGpuBackend(
      device,
      canvas,
      context,
      format,
      pipeline,
      bindGroup,
      uniformBuffer,
      readbackTexture,
      readbackTextureView,
      benchUploadBuffer,
      benchUploadScratch,
      width,
      height,
      capabilities,
    );
  }

  getCapabilities() {
    return this.capabilities;
  }

  async presentTestPattern() {
    const canvasTexture = this.context.getCurrentTexture();
    const canvasView = canvasTexture.createView();

    const encoder = this.device.createCommandEncoder();
    for (const view of [canvasView, this.readbackTextureView]) {
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
      pass.setPipeline(this.pipeline);
      pass.setBindGroup(0, this.bindGroup);
      pass.draw(3);
      pass.end();
    }

    this.device.queue.submit([encoder.finish()]);
    this.hasPresentedFrame = true;
  }

  async screenshotRgba() {
    if (!this.hasPresentedFrame) {
      throw new Error('No rendered frame available (present_test_pattern not called yet)');
    }

    // Smoke-test screenshot: read back the *rendered output*.
    //
    // We render the test pattern into an internal `readbackTexture` so we can copy it
    // into a mappable buffer. This captures the same pixels as the canvas output.
    // It is not the same contract as the runtime `Presenter.screenshot()` API, which
    // is defined as a deterministic readback of the source framebuffer bytes.
    const bytesPerPixel = 4;
    const unpaddedBytesPerRow = this.width * bytesPerPixel;
    const paddedBytesPerRow = Math.ceil(unpaddedBytesPerRow / 256) * 256;

    const readback = this.device.createBuffer({
      size: paddedBytesPerRow * this.height,
      usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
    });

    const encoder = this.device.createCommandEncoder();
    encoder.copyTextureToBuffer(
      { texture: this.readbackTexture },
      { buffer: readback, bytesPerRow: paddedBytesPerRow },
      { width: this.width, height: this.height, depthOrArrayLayers: 1 },
    );
    this.device.queue.submit([encoder.finish()]);

    await readback.mapAsync(GPUMapMode.READ);
    const mapped = new Uint8Array(readback.getMappedRange());

    // Strip row padding (if any) and normalize to RGBA byte order.
    const rgba = new Uint8Array(this.width * this.height * bytesPerPixel);
    for (let y = 0; y < this.height; y++) {
      const srcRow = mapped.subarray(y * paddedBytesPerRow, y * paddedBytesPerRow + unpaddedBytesPerRow);
      rgba.set(srcRow, y * unpaddedBytesPerRow);
    }

    readback.unmap();

    if (this.format.startsWith('bgra')) {
      for (let i = 0; i < rgba.length; i += 4) {
        const b = rgba[i];
        rgba[i] = rgba[i + 2];
        rgba[i + 2] = b;
      }
    }

    return rgba;
  }

  /**
   * @param {number} frames
   */
  async benchPresent(frames) {
    const times = [];
    for (let i = 0; i < frames; i++) {
      this.benchUploadScratch[0] = i & 0xff;
      const t0 = performance.now();
      this.device.queue.writeBuffer(this.benchUploadBuffer, 0, this.benchUploadScratch);
      await this.presentTestPattern();
      const t1 = performance.now();
      times.push(t1 - t0);
    }

    return {
      backend: /** @type {BackendKind} */ ('webgpu'),
      frames,
      timingsMs: stats(times),
      capabilities: this.capabilities,
    };
  }
}

/** @type {{ kind: BackendKind, backend: WebGpuBackend | WebGl2Backend } | null} */
let gpu = null;

/**
 * @param {OffscreenCanvas} canvas
 * @param {number} width
 * @param {number} height
 * @param {{ preferWebGpu?: boolean, forceBackend?: BackendKind } | undefined} options
 */
async function initBackend(canvas, width, height, options) {
  const preferWebGpu = options?.preferWebGpu ?? true;
  const forceBackend = options?.forceBackend;

  if (forceBackend === 'webgpu') {
    return { kind: /** @type {BackendKind} */ ('webgpu'), backend: await WebGpuBackend.create(canvas, width, height) };
  }
  if (forceBackend === 'webgl2') {
    return { kind: /** @type {BackendKind} */ ('webgl2'), backend: await WebGl2Backend.create(canvas, width, height) };
  }

  if (preferWebGpu && navigator.gpu) {
    try {
      return { kind: /** @type {BackendKind} */ ('webgpu'), backend: await WebGpuBackend.create(canvas, width, height) };
    } catch {
      // Fallback to WebGL2.
    }
  }

  return { kind: /** @type {BackendKind} */ ('webgl2'), backend: await WebGl2Backend.create(canvas, width, height) };
}

self.onmessage = async (event) => {
  const msg = event.data;
  if (!msg || typeof msg !== 'object' || typeof msg.type !== 'string' || typeof msg.id !== 'number') return;
  const { id, type } = msg;

  try {
    switch (type) {
      case 'init': {
        // @ts-ignore - structured clone carries OffscreenCanvas fine.
        const { canvas, width, height, options } = msg;
        if (!(canvas instanceof OffscreenCanvas)) throw new Error('init.canvas must be an OffscreenCanvas');
        gpu = await initBackend(canvas, width, height, options);
        self.postMessage({
          id,
          type: 'ready',
          backend: gpu.kind,
          capabilities: gpu.backend.getCapabilities(),
        });
        break;
      }
      case 'present_test_pattern': {
        if (!gpu) throw new Error('GPU backend not initialized');
        await gpu.backend.presentTestPattern();
        self.postMessage({ id, type: 'presented' });
        break;
      }
      case 'request_screenshot': {
        if (!gpu) throw new Error('GPU backend not initialized');
        const rgba = await gpu.backend.screenshotRgba();
        const hash = await sha256Hex(rgba);
        self.postMessage(
          {
            id,
            type: 'screenshot',
            backend: gpu.kind,
            width: gpu.backend.width,
            height: gpu.backend.height,
            rgba,
            hash,
          },
          [rgba.buffer],
        );
        break;
      }
      case 'bench_present': {
        if (!gpu) throw new Error('GPU backend not initialized');
        const frames = Number(msg.frames);
        if (!Number.isFinite(frames) || frames <= 0) throw new Error('bench_present.frames must be a positive number');
        const report = await gpu.backend.benchPresent(frames);
        self.postMessage({ id, type: 'bench_result', report });
        break;
      }
      default:
        throw new Error(`Unknown message type: ${type}`);
    }
  } catch (err) {
    self.postMessage({ id, type: 'error', error: stringifyError(err) });
  }
};
