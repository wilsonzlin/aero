/**
 * WebGPU triangle batch (N draws).
 *
 * Measures draw-call overhead and basic pipeline execution cost. Falls back to
 * WebGL2 when WebGPU is unavailable (useful for headless CI environments).
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
  if (!navigator.gpu) {
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

  const shaderCode = `
    struct VSOut {
      @builtin(position) pos: vec4f,
    }

    @vertex
    fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
      var positions = array<vec2f, 3>(
        vec2f(0.0,  0.5),
        vec2f(-0.5, -0.5),
        vec2f(0.5, -0.5),
      );
      var out: VSOut;
      out.pos = vec4f(positions[vid], 0.0, 1.0);
      return out;
    }

    @fragment
    fn fs_main() -> @location(0) vec4f {
      return vec4f(1.0, 0.0, 0.0, 1.0);
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
    fragment: { module: shaderModule, entryPoint: "fs_main", targets: [{ format }] },
    primitive: { topology: "triangle-list" },
  });

  /** @type {Promise<void>[]} */
  const latencyPromises = [];

  await runRafFrames(params.frames, (ts) => {
    ctx.telemetry.beginFrame(ts);

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
    for (let i = 0; i < params.draws; i += 1) {
      pass.draw(3);
    }
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
    void main() {
      vec2 pos[3] = vec2[3](
        vec2(0.0,  0.5),
        vec2(-0.5, -0.5),
        vec2(0.5, -0.5)
      );
      gl_Position = vec4(pos[gl_VertexID], 0.0, 1.0);
    }`;
  const fsSrc = `#version 300 es
    precision highp float;
    out vec4 outColor;
    void main() {
      outColor = vec4(1.0, 0.0, 0.0, 1.0);
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
  gl.viewport(0, 0, ctx.canvas.width, ctx.canvas.height);

  return runRafFrames(params.frames, (ts) => {
    ctx.telemetry.beginFrame(ts);

    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);
    for (let i = 0; i < params.draws; i += 1) {
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }

    ctx.telemetry.endFrame(performance.now());
  }).then(() => ({ status: "ok", api: "webgl2", params }));
}

export const scenario = {
  id: "webgpu_triangle_batch",
  name: "WebGPU triangle batch (N draws)",
  defaultParams: {
    frames: 120,
    draws: 250,
    width: 800,
    height: 600,
  },

  /**
   * @param {{canvas: HTMLCanvasElement, telemetry: any, params?: any}} ctx
   */
  async run(ctx) {
    const params = { ...scenario.defaultParams, ...(ctx.params ?? {}) };
    ctx.canvas.width = params.width;
    ctx.canvas.height = params.height;

    const webgpu = await tryRunWebGpu(ctx, params);
    if (webgpu.status === "ok") return webgpu;

    return await tryRunWebGl2(ctx, params);
  },
};

registerScenario(scenario);

