/**
 * VBE LFB full-screen blit.
 *
 * Approximates a "linear framebuffer" present path: upload a full-screen RGBA
 * buffer every frame and draw it.
 *
 * Implementation prefers WebGPU (queue.writeTexture) and falls back to WebGL2.
 */

function registerScenario(scenario) {
  const g = /** @type {any} */ (globalThis);
  g.__aeroGpuBenchScenarios = g.__aeroGpuBenchScenarios ?? {};
  g.__aeroGpuBenchScenarios[scenario.id] = scenario;
}

/**
 * @param {number} frames
 * @param {(ts:number, frameIndex:number) => void} onFrame
 */
function runRafFrames(frames, onFrame) {
  return new Promise((resolve) => {
    let i = 0;
    const step = (ts) => {
      onFrame(ts, i);
      i += 1;
      if (i < frames) {
        requestAnimationFrame(step);
      } else {
        resolve();
      }
    };
    requestAnimationFrame(step);
  });
}

async function tryRunWebGpu(ctx, params) {
  if (!("gpu" in navigator) || !navigator.gpu) {
    return { status: "skipped", reason: "WebGPU not available", api: "webgpu", params };
  }
  const canvasContext = ctx.canvas.getContext("webgpu");
  if (!canvasContext) {
    return { status: "skipped", reason: "canvas.getContext('webgpu') returned null", api: "webgpu", params };
  }

  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) {
    return { status: "skipped", reason: "navigator.gpu.requestAdapter() returned null", api: "webgpu", params };
  }

  const device = await adapter.requestDevice();
  const format = navigator.gpu.getPreferredCanvasFormat();
  canvasContext.configure({ device, format, alphaMode: "opaque" });

  const texture = device.createTexture({
    size: { width: params.width, height: params.height },
    format: "rgba8unorm",
    usage: GPUTextureUsage.TEXTURE_BINDING | GPUTextureUsage.COPY_DST,
  });

  const sampler = device.createSampler({ magFilter: "nearest", minFilter: "nearest" });

  const shaderCode = `
    struct VSOut {
      @builtin(position) pos: vec4f,
      @location(0) uv: vec2f,
    }

    @vertex
    fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
      var positions = array<vec2f, 3>(
        vec2f(-1.0, -1.0),
        vec2f( 3.0, -1.0),
        vec2f(-1.0,  3.0),
      );
      var uvs = array<vec2f, 3>(
        vec2f(0.0, 1.0),
        vec2f(2.0, 1.0),
        vec2f(0.0, -1.0),
      );

      var out: VSOut;
      out.pos = vec4f(positions[vid], 0.0, 1.0);
      out.uv = uvs[vid];
      return out;
    }

    @group(0) @binding(0) var s: sampler;
    @group(0) @binding(1) var t: texture_2d<f32>;

    @fragment
    fn fs_main(in: VSOut) -> @location(0) vec4f {
      return textureSample(t, s, in.uv);
    }
  `;

  const compileStart = performance.now();
  const shaderModule = device.createShaderModule({ code: shaderCode });
  if (shaderModule.getCompilationInfo) {
    await shaderModule.getCompilationInfo();
  }
  ctx.telemetry.recordShaderCompilationMs(performance.now() - compileStart);

  const pipeline = device.createRenderPipeline({
    layout: "auto",
    vertex: { module: shaderModule, entryPoint: "vs_main" },
    fragment: {
      module: shaderModule,
      entryPoint: "fs_main",
      targets: [{ format }],
    },
    primitive: { topology: "triangle-list" },
  });

  const bindGroup = device.createBindGroup({
    layout: pipeline.getBindGroupLayout(0),
    entries: [
      { binding: 0, resource: sampler },
      { binding: 1, resource: texture.createView() },
    ],
  });

  const pixelBytesPerRow = params.width * 4;
  const pixels = new Uint8Array(pixelBytesPerRow * params.height);

  /** @type {Promise<void>[]} */
  const latencyPromises = [];

  await runRafFrames(params.frames, (ts, frameIndex) => {
    ctx.telemetry.beginFrame(ts);

    pixels.fill(frameIndex & 0xff);
    ctx.telemetry.recordTextureUploadBytes(pixels.byteLength);
    device.queue.writeTexture(
      { texture },
      pixels,
      { bytesPerRow: pixelBytesPerRow },
      { width: params.width, height: params.height },
    );

    const encoder = device.createCommandEncoder();
    const pass = encoder.beginRenderPass({
      colorAttachments: [
        {
          view: canvasContext.getCurrentTexture().createView(),
          loadOp: "clear",
          storeOp: "store",
          clearValue: { r: 0, g: 0, b: 0, a: 1 },
        },
      ],
    });
    pass.setPipeline(pipeline);
    pass.setBindGroup(0, bindGroup);
    pass.draw(3);
    pass.end();
    device.queue.submit([encoder.finish()]);

    const submitAt = performance.now();
    latencyPromises.push(
      device.queue.onSubmittedWorkDone().then(() => {
        ctx.telemetry.recordPresentLatencyMs(performance.now() - submitAt);
      }),
    );

    ctx.telemetry.endFrame(performance.now());
  });

  await Promise.allSettled(latencyPromises);

  return { status: "ok", api: "webgpu", params };
}

function tryRunWebGl2(ctx, params) {
  const gl = ctx.canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: false });
  if (!gl) {
    return { status: "skipped", reason: "WebGL2 context unavailable", api: "webgl2", params };
  }

  const vsSrc = `#version 300 es
    precision highp float;
    out vec2 vUv;
    const vec2 pos[3] = vec2[3](
      vec2(-1.0, -1.0),
      vec2( 3.0, -1.0),
      vec2(-1.0,  3.0)
    );
    const vec2 uv[3] = vec2[3](
      vec2(0.0, 0.0),
      vec2(2.0, 0.0),
      vec2(0.0, 2.0)
    );
    void main() {
      gl_Position = vec4(pos[gl_VertexID], 0.0, 1.0);
      vUv = uv[gl_VertexID];
    }`;

  const fsSrc = `#version 300 es
    precision highp float;
    uniform sampler2D uTex;
    in vec2 vUv;
    out vec4 outColor;
    void main() {
      outColor = texture(uTex, vUv);
    }`;

  const compileStart = performance.now();
  const vs = gl.createShader(gl.VERTEX_SHADER);
  const fs = gl.createShader(gl.FRAGMENT_SHADER);
  if (!vs || !fs) {
    return { status: "skipped", reason: "WebGL shader allocation failed", api: "webgl2", params };
  }
  gl.shaderSource(vs, vsSrc);
  gl.shaderSource(fs, fsSrc);
  gl.compileShader(vs);
  gl.compileShader(fs);
  const prog = gl.createProgram();
  if (!prog) {
    return { status: "skipped", reason: "WebGL program allocation failed", api: "webgl2", params };
  }
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  ctx.telemetry.recordShaderCompilationMs(performance.now() - compileStart);

  gl.useProgram(prog);

  const tex = gl.createTexture();
  if (!tex) {
    return { status: "skipped", reason: "WebGL texture allocation failed", api: "webgl2", params };
  }
  gl.bindTexture(gl.TEXTURE_2D, tex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.RGBA,
    params.width,
    params.height,
    0,
    gl.RGBA,
    gl.UNSIGNED_BYTE,
    null,
  );

  const pixels = new Uint8Array(params.width * params.height * 4);

  return runRafFrames(params.frames, (ts, frameIndex) => {
    ctx.telemetry.beginFrame(ts);

    pixels.fill(frameIndex & 0xff);
    ctx.telemetry.recordTextureUploadBytes(pixels.byteLength);

    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, params.width, params.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);

    gl.viewport(0, 0, ctx.canvas.width, ctx.canvas.height);
    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.drawArrays(gl.TRIANGLES, 0, 3);

    ctx.telemetry.endFrame(performance.now());
  }).then(() => ({ status: "ok", api: "webgl2", params }));
}

export const scenario = {
  id: "vbe_lfb_blit",
  name: "VBE LFB full-screen blit",
  defaultParams: {
    frames: 120,
    width: 512,
    height: 512,
  },

  /**
   * @param {{canvas: HTMLCanvasElement, telemetry: any, params?: any}} ctx
   */
  async run(ctx) {
    const params = { ...scenario.defaultParams, ...(ctx.params ?? {}) };

    ctx.canvas.width = params.width;
    ctx.canvas.height = params.height;

    const webgpuResult = await tryRunWebGpu(ctx, params);
    if (webgpuResult.status === "ok") return webgpuResult;

    // WebGPU may be unavailable in headless/CI; fall back to WebGL2 so the suite
    // still produces useful numbers.
    return await tryRunWebGl2(ctx, params);
  },
};

registerScenario(scenario);

