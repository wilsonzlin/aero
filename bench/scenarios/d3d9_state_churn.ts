/**
 * D3D9 state churn test (many pipeline switches).
 *
 * This is not a full D3D9 implementation; instead, it approximates the cost of
 * the translation layer by:
 * - simulating "DXBC -> WGSL" translation work (CPU)
 * - compiling/linking the produced shaders (GPU backend)
 * - aggressively switching pipelines/programs to stress the cache
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

/**
 * Cheap-ish simulation of DXBC->WGSL translation. The returned WGSL shader is
 * deterministic for a given `stateKey` so the pipeline cache can hit.
 *
 * @param {number} stateKey
 */
function fakeDxbcToWgsl(stateKey) {
  // Do some bounded CPU work to emulate instruction decoding / control flow
  // reconstruction. Keep it deterministic to avoid JIT variance.
  const start = performance.now();
  let acc = stateKey >>> 0;
  for (let i = 0; i < 10_000; i += 1) {
    acc = (acc * 1664525 + 1013904223) >>> 0;
    acc ^= acc >>> 16;
  }
  const elapsed = performance.now() - start;

  const colorR = ((acc >>> 0) & 0xff) / 255;
  const colorG = ((acc >>> 8) & 0xff) / 255;
  const colorB = ((acc >>> 16) & 0xff) / 255;

  const wgsl = `
    @fragment
    fn fs_main() -> @location(0) vec4f {
      return vec4f(${colorR.toFixed(4)}, ${colorG.toFixed(4)}, ${colorB.toFixed(4)}, 1.0);
    }
  `;

  return { wgsl, translationMs: elapsed };
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

  const vsCode = `
    struct VSOut { @builtin(position) pos: vec4f };
    @vertex fn vs_main(@builtin(vertex_index) vid: u32) -> VSOut {
      var positions = array<vec2f, 3>(
        vec2f(0.0,  0.5),
        vec2f(-0.5, -0.5),
        vec2f(0.5, -0.5),
      );
      var out: VSOut;
      out.pos = vec4f(positions[vid], 0.0, 1.0);
      return out;
    }
  `;

  const vsCompileStart = performance.now();
  const vsModule = device.createShaderModule({ code: vsCode });
  if (vsModule.getCompilationInfo) await vsModule.getCompilationInfo();
  ctx.telemetry.recordShaderCompilationMs(performance.now() - vsCompileStart);

  /** @type {Map<number, GPURenderPipeline>} */
  const pipelineCache = new Map();

  const estimatedPipelineBytes = 4096;

  /** @type {Promise<void>[]} */
  const pendingCompilation = [];

  function getPipeline(stateKey) {
    const cached = pipelineCache.get(stateKey);
    if (cached) {
      ctx.telemetry.recordPipelineCacheHit();
      return cached;
    }
    ctx.telemetry.recordPipelineCacheMiss();

    const { wgsl, translationMs } = fakeDxbcToWgsl(stateKey);
    ctx.telemetry.recordShaderTranslationMs(translationMs);

    // Shader compilation is asynchronous in most implementations. Record the
    // time-to-compilation-info without blocking the render loop so we can still
    // measure frame pacing / dropped frames.
    const fsCompileStart = performance.now();
    const fsModule = device.createShaderModule({ code: wgsl });
    if (fsModule.getCompilationInfo) {
      pendingCompilation.push(
        fsModule.getCompilationInfo().then(() => {
          ctx.telemetry.recordShaderCompilationMs(performance.now() - fsCompileStart);
        }),
      );
    } else {
      ctx.telemetry.recordShaderCompilationMs(performance.now() - fsCompileStart);
    }

    const blendEnabled = (stateKey & 1) !== 0;

    const pipeline = device.createRenderPipeline({
      layout: "auto",
      vertex: { module: vsModule, entryPoint: "vs_main" },
      fragment: {
        module: fsModule,
        entryPoint: "fs_main",
        targets: [
          {
            format,
            blend: blendEnabled
              ? {
                  color: { srcFactor: "src-alpha", dstFactor: "one-minus-src-alpha", operation: "add" },
                  alpha: { srcFactor: "one", dstFactor: "one-minus-src-alpha", operation: "add" },
                }
              : undefined,
          },
        ],
      },
      primitive: { topology: "triangle-list" },
    });

    pipelineCache.set(stateKey, pipeline);
    ctx.telemetry.setPipelineCacheStats({
      entries: pipelineCache.size,
      sizeBytes: pipelineCache.size * estimatedPipelineBytes,
    });
    return pipeline;
  }

  /** @type {Promise<void>[]} */
  const latencyPromises = [];

  await runRafFrames(params.frames, (ts, frameIndex) => {
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

    const base = frameIndex * params.switchesPerFrame;
    for (let i = 0; i < params.switchesPerFrame; i += 1) {
      const stateKey = (base + i) % params.uniqueStates;
      const pipeline = getPipeline(stateKey);
      pass.setPipeline(pipeline);
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

  await Promise.allSettled(pendingCompilation);
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

  /** @type {Map<number, WebGLProgram>} */
  const programCache = new Map();
  const estimatedProgramBytes = 2048;

  function compileProgram(stateKey) {
    const cached = programCache.get(stateKey);
    if (cached) {
      ctx.telemetry.recordPipelineCacheHit();
      return cached;
    }
    ctx.telemetry.recordPipelineCacheMiss();

    const { wgsl, translationMs } = fakeDxbcToWgsl(stateKey);
    ctx.telemetry.recordShaderTranslationMs(translationMs);

    // Convert our tiny WGSL-like output into a simple WebGL fragment shader by
    // embedding the constant color.
    const colorMatch = wgsl.match(/vec4f\\(([^)]+)\\)/);
    const color = colorMatch ? colorMatch[1] : "1.0, 0.0, 1.0, 1.0";
    const fsSrc = `#version 300 es
      precision highp float;
      out vec4 outColor;
      void main() { outColor = vec4(${color}); }`;

    const compileStart = performance.now();
    const vs = gl.createShader(gl.VERTEX_SHADER);
    const fs = gl.createShader(gl.FRAGMENT_SHADER);
    if (!vs || !fs) return null;
    gl.shaderSource(vs, vsSrc);
    gl.shaderSource(fs, fsSrc);
    gl.compileShader(vs);
    gl.compileShader(fs);
    const prog = gl.createProgram();
    if (!prog) return null;
    gl.attachShader(prog, vs);
    gl.attachShader(prog, fs);
    gl.linkProgram(prog);
    ctx.telemetry.recordShaderCompilationMs(performance.now() - compileStart);

    programCache.set(stateKey, prog);
    ctx.telemetry.setPipelineCacheStats({
      entries: programCache.size,
      sizeBytes: programCache.size * estimatedProgramBytes,
    });
    return prog;
  }

  gl.viewport(0, 0, ctx.canvas.width, ctx.canvas.height);

  return runRafFrames(params.frames, (ts, frameIndex) => {
    ctx.telemetry.beginFrame(ts);

    gl.clearColor(0, 0, 0, 1);
    gl.clear(gl.COLOR_BUFFER_BIT);

    const base = frameIndex * params.switchesPerFrame;
    for (let i = 0; i < params.switchesPerFrame; i += 1) {
      const stateKey = (base + i) % params.uniqueStates;
      const prog = compileProgram(stateKey);
      if (!prog) continue;
      gl.useProgram(prog);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }

    ctx.telemetry.endFrame(performance.now());
  }).then(() => ({ status: "ok", api: "webgl2", params }));
}

export const scenario = {
  id: "d3d9_state_churn",
  name: "D3D9 state churn test (many pipeline switches)",
  defaultParams: {
    frames: 120,
    switchesPerFrame: 100,
    uniqueStates: 64,
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
