import { formatOneLineError } from "../text";
import { callMethodBestEffort, tryGetMethodBestEffort } from "../safeMethod";

// WebGPU presenter responsible for the *final* step: showing the emulator framebuffer
// on an HTML canvas with correct sRGB/linear handling and correct alpha mode.
//
// This mirrors `crates/aero-gpu/src/present.rs` and uses the same bitflags as
// `crates/aero-gpu/shaders/blit.wgsl`.

const BLIT_WGSL = `
struct VsOut {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
}

struct BlitParams {
  flags: u32,
}

const FLAG_APPLY_SRGB_ENCODE: u32 = 1u;
const FLAG_PREMULTIPLY_ALPHA: u32 = 2u;
const FLAG_FORCE_OPAQUE_ALPHA: u32 = 4u;
const FLAG_FLIP_Y: u32 = 8u;

@group(0) @binding(0) var input_tex: texture_2d<f32>;
@group(0) @binding(1) var input_sampler: sampler;
@group(0) @binding(2) var<uniform> params: BlitParams;

fn srgb_encode_channel(x: f32) -> f32 {
  let v = clamp(x, 0.0, 1.0);
  if (v <= 0.0031308) { return v * 12.92; }
  return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

fn srgb_encode(rgb: vec3<f32>) -> vec3<f32> {
  return vec3<f32>(
    srgb_encode_channel(rgb.r),
    srgb_encode_channel(rgb.g),
    srgb_encode_channel(rgb.b),
  );
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
  var positions = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0),
    vec2<f32>(3.0, -1.0),
    vec2<f32>(-1.0, 3.0),
  );
  let xy = positions[vid];

  var out: VsOut;
  out.position = vec4<f32>(xy, 0.0, 1.0);
  out.uv = vec2<f32>((xy.x + 1.0) * 0.5, (1.0 - xy.y) * 0.5);
  return out;
}

@fragment
fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
  var uv = input.uv;
  if ((params.flags & FLAG_FLIP_Y) != 0u) { uv.y = 1.0 - uv.y; }

  var color = textureSample(input_tex, input_sampler, uv);

  if ((params.flags & FLAG_PREMULTIPLY_ALPHA) != 0u) {
    color = vec4<f32>(color.rgb * color.a, color.a);
  }
  if ((params.flags & FLAG_FORCE_OPAQUE_ALPHA) != 0u) {
    color.a = 1.0;
  }
  if ((params.flags & FLAG_APPLY_SRGB_ENCODE) != 0u) {
    color = vec4<f32>(srgb_encode(color.rgb), color.a);
  }
  return color;
}
`;

/**
 * @typedef {"linear" | "srgb"} ColorSpace
 * @typedef {"opaque" | "premultiplied"} AlphaMode
 */

/**
 * @typedef {object} WebGpuPresenterOptions
 * @property {ColorSpace=} framebufferColorSpace
 * @property {ColorSpace=} outputColorSpace
 * @property {AlphaMode=} alphaMode
 * @property {boolean=} flipY
 * @property {((err: unknown) => void)=} onError
 */

function toSrgbFormat(format: GPUTextureFormat): GPUTextureFormat | null {
  if (format === "bgra8unorm") return "bgra8unorm-srgb";
  if (format === "rgba8unorm") return "rgba8unorm-srgb";
  return null;
}

function isBgraFormat(format: GPUTextureFormat): boolean {
  return format === "bgra8unorm" || format === "bgra8unorm-srgb";
}

function bgraToRgbaInPlace(bytes: Uint8Array) {
  for (let i = 0; i < bytes.length; i += 4) {
    const b = bytes[i + 0];
    const r = bytes[i + 2];
    bytes[i + 0] = r;
    bytes[i + 2] = b;
  }
}

type CanvasConfig = GPUCanvasConfiguration & { viewFormats?: GPUTextureFormat[] };

export class WebGpuPresenter {
  /** @type {GPUDevice} */
  device;
  /** @type {GPUQueue} */
  queue;
  /** @type {HTMLCanvasElement | OffscreenCanvas} */
  canvas;
  /** @type {GPUCanvasContext} */
  context;
  /** @type {GPUTextureFormat} */
  canvasFormat;
  /** @type {GPUTextureFormat} */
  viewFormat;
  /** @type {boolean} */
  srgbEncodeInShader;

  /** @type {GPURenderPipeline} */
  pipeline;
  /** @type {GPUSampler} */
  sampler;
  /** @type {GPUBuffer} */
  paramsBuffer;
  /** @type {GPUBindGroup} */
  bindGroup;

  srcTex: GPUTexture | null = null;
  srcView: GPUTextureView | null = null;
  /** @type {number} */
  srcWidth = 0;
  /** @type {number} */
  srcHeight = 0;

  /** @type {WebGpuPresenterOptions} */
  opts;
  /** @type {"opaque" | "premultiplied"} */
  _alphaMode;

  _uncapturedErrorDevice: GPUDevice | null = null;
  _onUncapturedError: ((ev: any) => void) | null = null;
  /** @type {Set<string>} */
  _seenUncapturedErrorKeys = new Set();

  /**
   * @param {GPUDevice} device
   * @param {GPUCanvasContext} context
   * @param {GPUTextureFormat} canvasFormat
   * @param {GPUTextureFormat} viewFormat
   * @param {boolean} srgbEncodeInShader
   * @param {WebGpuPresenterOptions} opts
   */
  constructor(
    device: GPUDevice,
    canvas: HTMLCanvasElement | OffscreenCanvas,
    context: GPUCanvasContext,
    canvasFormat: GPUTextureFormat,
    viewFormat: GPUTextureFormat,
    srgbEncodeInShader: boolean,
    opts: any,
    alphaMode: "opaque" | "premultiplied",
  ) {
    this.device = device;
    this.queue = device.queue;
    this.canvas = canvas;
    this.context = context;
    this.canvasFormat = canvasFormat;
    this.viewFormat = viewFormat;
    this.srgbEncodeInShader = srgbEncodeInShader;
    this.opts = opts;
    this._alphaMode = alphaMode;

    this._installUncapturedErrorHandler();

    const module = device.createShaderModule({ code: BLIT_WGSL });
    this.pipeline = device.createRenderPipeline({
      layout: "auto",
      vertex: { module, entryPoint: "vs_main" },
      fragment: {
        module,
        entryPoint: "fs_main",
        targets: [{ format: viewFormat }],
      },
      primitive: { topology: "triangle-list" },
    });

    this.sampler = device.createSampler({
      magFilter: "nearest",
      minFilter: "nearest",
      addressModeU: "clamp-to-edge",
      addressModeV: "clamp-to-edge",
    });

    // Uniform buffers require 16-byte minimum alignment for bindings in practice.
    this.paramsBuffer = device.createBuffer({
      size: 16,
      usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST,
    });

    // bindGroup created once the source texture exists.
    // (the swapchain texture view changes each frame and is passed as render attachment).
    this.bindGroup = device.createBindGroup({
      layout: this.pipeline.getBindGroupLayout(0),
      entries: [
        // Placeholder view, replaced in `setSourceRgba8`.
        { binding: 0, resource: device.createTexture({ size: [1, 1], format: "rgba8unorm", usage: GPUTextureUsage.TEXTURE_BINDING }).createView() },
        { binding: 1, resource: this.sampler },
        { binding: 2, resource: { buffer: this.paramsBuffer } },
      ],
    });
  }

  _installUncapturedErrorHandler() {
    this._uninstallUncapturedErrorHandler();
    this._seenUncapturedErrorKeys.clear();

    const device = this.device;
    const handler = (ev: unknown) => {
      try {
        callMethodBestEffort(ev, "preventDefault");

        const err = (ev as { error?: unknown } | null | undefined)?.error;
        const ctor = err && typeof err === "object" ? (err as { constructor?: unknown }).constructor : undefined;
        const ctorName = typeof ctor === "function" ? ctor.name : "";
        const errorName =
          (err && typeof err === "object" && typeof (err as { name?: unknown }).name === "string" ? (err as { name: string }).name : "") ||
          ctorName;
        const errorMessage =
          err && typeof err === "object" && typeof (err as { message?: unknown }).message === "string" ? (err as { message: string }).message : "";
        let msg = errorMessage || formatOneLineError(err ?? "WebGPU uncaptured error", 512);
        if (errorName && msg && !msg.toLowerCase().startsWith(errorName.toLowerCase())) {
          msg = `${errorName}: ${msg}`;
        }

        const key = `${errorName}:${msg}`;
        if (this._seenUncapturedErrorKeys.has(key)) return;
        this._seenUncapturedErrorKeys.add(key);
        if (this._seenUncapturedErrorKeys.size > 128) {
          this._seenUncapturedErrorKeys.clear();
          this._seenUncapturedErrorKeys.add(key);
        }

        const onError = (this.opts as { onError?: unknown } | null | undefined)?.onError;
        if (typeof onError === "function") {
          onError(err ?? ev);
        } else {
          console.error("[WebGpuPresenter] uncapturederror", err ?? ev);
        }
      } catch {
        // Best-effort diagnostics only.
      }
    };

    this._uncapturedErrorDevice = device;
    this._onUncapturedError = handler;

    const addEventListener = tryGetMethodBestEffort(device, "addEventListener");
    if (addEventListener) {
      try {
        (addEventListener as (type: string, listener: (ev: unknown) => void) => void).call(
          device,
          "uncapturederror",
          handler,
        );
        return;
      } catch {
        // Fall through.
      }
    }

    try {
      (device as unknown as { onuncapturederror?: unknown }).onuncapturederror = handler;
    } catch {
      // Ignore.
    }
  }

  _uninstallUncapturedErrorHandler() {
    const device = this._uncapturedErrorDevice;
    const handler = this._onUncapturedError;
    if (device && handler) {
      const removeEventListener = tryGetMethodBestEffort(device, "removeEventListener");
      if (removeEventListener) {
        try {
          (removeEventListener as (type: string, listener: (ev: unknown) => void) => void).call(
            device,
            "uncapturederror",
            handler,
          );
        } catch {
          // Ignore.
        }
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
    this._uncapturedErrorDevice = null;
    this._onUncapturedError = null;
    this._seenUncapturedErrorKeys.clear();
  }

  destroy() {
    this._uninstallUncapturedErrorHandler();
    callMethodBestEffort(this.context, "unconfigure");
    callMethodBestEffort(this.device, "destroy");
  }

  /**
   * Create and configure a WebGPU presenter.
   *
   * @param {HTMLCanvasElement | OffscreenCanvas} canvas
   * @param {WebGpuPresenterOptions=} opts
   */
  static async create(canvas: HTMLCanvasElement | OffscreenCanvas, opts: any = {}) {
    if (!navigator.gpu) throw new Error("WebGPU not supported");

    let device: GPUDevice | null = null;
    let context: GPUCanvasContext | null = null;
    try {
      const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
      if (!adapter) throw new Error("No WebGPU adapter");

      const requiredFeatures = (opts.requiredFeatures ?? []) as GPUFeatureName[];
      device =
        requiredFeatures.length > 0 ? await adapter.requestDevice({ requiredFeatures }) : await adapter.requestDevice();
      context = (canvas as unknown as { getContext(type: "webgpu"): GPUCanvasContext | null }).getContext("webgpu");
      if (!context) throw new Error("Canvas WebGPU context not available");

      const resolvedOpts = {
        framebufferColorSpace: opts.framebufferColorSpace ?? "linear",
        outputColorSpace: opts.outputColorSpace ?? "srgb",
        alphaMode: opts.alphaMode ?? "opaque",
        flipY: opts.flipY ?? false,
        // Optional diagnostics hook: when provided, uncaptured WebGPU errors will be routed here.
        ...(typeof opts.onError === "function" ? { onError: opts.onError } : {}),
      };

      const canvasFormat = navigator.gpu.getPreferredCanvasFormat();
      const srgbFormat = toSrgbFormat(canvasFormat);

      const alphaMode = resolvedOpts.alphaMode === "premultiplied" ? "premultiplied" : "opaque";

      let viewFormat = canvasFormat;
      let srgbEncodeInShader = resolvedOpts.outputColorSpace === "srgb";

      // Prefer an sRGB view format when requesting sRGB output.
      // Chrome currently reports `bgra8unorm` as preferred and requires using `viewFormats`
      // to render with an sRGB view.
      if (resolvedOpts.outputColorSpace === "srgb" && srgbFormat) {
        try {
          // TS libdefs lag behind WebGPU; `viewFormats` is standard but may not be in types.
          const config: CanvasConfig = {
            device,
            format: canvasFormat,
            usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
            alphaMode,
            viewFormats: [srgbFormat],
          };
          context.configure(config);
          viewFormat = srgbFormat;
          srgbEncodeInShader = false; // GPU will encode when writing to the sRGB view.
        } catch {
          // Fall back to a linear view and do encoding in shader.
          context.configure({
            device,
            format: canvasFormat,
            usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
            alphaMode,
          });
          viewFormat = canvasFormat;
          srgbEncodeInShader = true;
        }
      } else {
        context.configure({
          device,
          format: canvasFormat,
          usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
          alphaMode,
        });
        viewFormat = canvasFormat;
        srgbEncodeInShader = resolvedOpts.outputColorSpace === "srgb";
      }

      return new WebGpuPresenter(
        device,
        canvas,
        context,
        canvasFormat,
        viewFormat,
        srgbEncodeInShader,
        resolvedOpts,
        alphaMode,
      );
    } catch (err) {
      // Best-effort cleanup so tests / validation pages can retry without leaking GPU devices.
      callMethodBestEffort(context, "unconfigure");
      callMethodBestEffort(device, "destroy");
      throw err;
    }
  }

  /**
   * Reconfigure the canvas context after the backing canvas size changes.
   *
   * WebGPU canvases generally require calling `configure()` again on resize.
   */
  reconfigureCanvas() {
    const config: CanvasConfig = {
      device: this.device,
      format: this.canvasFormat,
      usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_SRC,
      alphaMode: this._alphaMode,
    };
    if (this.viewFormat !== this.canvasFormat) {
      config.viewFormats = [this.viewFormat];
    }
    this.context.configure(config);
  }

  /**
   * @param {Uint8Array} rgba
   * @param {number} width
   * @param {number} height
   */
  setSourceRgba8(rgba: Uint8Array, width: number, height: number) {
    if (!this.srcTex || width !== this.srcWidth || height !== this.srcHeight) {
      this.srcWidth = width;
      this.srcHeight = height;

      const format =
        this.opts.framebufferColorSpace === "srgb" ? ("rgba8unorm-srgb" as const) : ("rgba8unorm" as const);

      const srcTex = this.device.createTexture({
        size: { width, height },
        format,
        usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
      });
      const srcView = srcTex.createView();
      this.srcTex = srcTex;
      this.srcView = srcView;

      this.bindGroup = this.device.createBindGroup({
        layout: this.pipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: srcView },
          { binding: 1, resource: this.sampler },
          { binding: 2, resource: { buffer: this.paramsBuffer } },
        ],
      });
    }

    this.queue.writeTexture(
      { texture: this.srcTex! },
      rgba as unknown as GPUAllowSharedBufferSource,
      { bytesPerRow: width * 4 },
      { width, height },
    );
  }

  private computeFlags(): number {
    let flags = 0;
    if (this.srgbEncodeInShader) flags |= 1;
    if (this.opts.alphaMode === "premultiplied") flags |= 2;
    if (this.opts.alphaMode === "opaque") flags |= 4;
    if (this.opts.flipY) flags |= 8;
    return flags;
  }

  present() {
    if (!this.srcTex) throw new Error("present() called before setSourceRgba8()");

    const flags = this.computeFlags();
    this.queue.writeBuffer(this.paramsBuffer, 0, new Uint32Array([flags]));

    const currentTexture = this.context.getCurrentTexture();
    const view =
      this.viewFormat === this.canvasFormat
        ? currentTexture.createView()
        : currentTexture.createView({ format: this.viewFormat });

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          loadOp: "clear",
          storeOp: "store",
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.draw(3, 1, 0, 0);
    pass.end();

    this.queue.submit([encoder.finish()]);
  }

  /**
   * Present and read back the final canvas pixels as RGBA8 (top-left origin).
   *
   * This is intended for validation tests; it is not a fast path.
   *
   * Semantics: this reads back the **presented output** (swapchain texture) and therefore
   * includes any color-space/alpha policy applied during presentation. Do not confuse this
   * with `Presenter.screenshot()` in `web/src/gpu/presenter.ts`, which is defined as a
   * readback of the *source framebuffer* bytes for deterministic hashing.
   */
  async presentAndReadbackRgba8(): Promise<Uint8Array> {
    if (!this.srcTex) throw new Error("presentAndReadbackRgba8() called before setSourceRgba8()");

    const width = this.canvas.width;
    const height = this.canvas.height;

    // WebGPU requires bytesPerRow to be a multiple of 256.
    const unpaddedBytesPerRow = width * 4;
    const bytesPerRow = Math.ceil(unpaddedBytesPerRow / 256) * 256;
    const bufferSize = bytesPerRow * height;

    const readback = this.device.createBuffer({
      size: bufferSize,
      usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST,
    });

    const flags = this.computeFlags();
    this.queue.writeBuffer(this.paramsBuffer, 0, new Uint32Array([flags]));

    const currentTexture = this.context.getCurrentTexture();
    const view =
      this.viewFormat === this.canvasFormat
        ? currentTexture.createView()
        : currentTexture.createView({ format: this.viewFormat });

    const encoder = this.device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view,
          loadOp: "clear",
          storeOp: "store",
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });
    pass.setPipeline(this.pipeline);
    pass.setBindGroup(0, this.bindGroup);
    pass.draw(3, 1, 0, 0);
    pass.end();

    encoder.copyTextureToBuffer(
      { texture: currentTexture },
      { buffer: readback, bytesPerRow, rowsPerImage: height },
      { width, height, depthOrArrayLayers: 1 },
    );

    this.queue.submit([encoder.finish()]);

    await readback.mapAsync(GPUMapMode.READ);
    const mapped = new Uint8Array(readback.getMappedRange());

    const out = new Uint8Array(width * height * 4);
    for (let y = 0; y < height; y++) {
      out.set(mapped.subarray(y * bytesPerRow, y * bytesPerRow + unpaddedBytesPerRow), y * unpaddedBytesPerRow);
    }

    readback.unmap();

    // Convert from swapchain storage order to RGBA for consistent hashing across backends.
    if (isBgraFormat(this.canvasFormat)) {
      bgraToRgbaInPlace(out);
    }

    return out;
  }
}
