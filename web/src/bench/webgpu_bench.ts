import { LogHistogram, RunningStats, msToUs, usToMs } from '../perf/stats';
import { formatOneLineError } from '../text';

export type WebGpuBenchOptions = {
  frames?: number;
  warmupFrames?: number;
  warmup_frames?: number;
  width?: number;
  height?: number;
  drawCallsPerFrame?: number;
  draw_calls_per_frame?: number;
  pipelineSwitchesPerFrame?: number;
  pipeline_switches_per_frame?: number;
  compute?: boolean;
  computeWorkgroups?: number;
  compute_workgroups?: number;
};

export type WebGpuBenchAdapterInfo = {
  vendor: string | null;
  architecture: string | null;
  device: string | null;
  description: string | null;
};

export type WebGpuBenchResult =
  | {
      supported: false;
      reason: string;
    }
  | {
      supported: true;
      adapter: WebGpuBenchAdapterInfo | null;
      capabilities: {
        timestamp_query: boolean;
      };
      frames: number;
      fps: number;
      draw_calls_per_frame: number;
      pipeline_switches_per_frame: number;
      cpu_encode_time_ms: {
        avg: number;
        p95: number;
      };
      gpu_time_ms: number | null;
      compute: {
        enabled: boolean;
        workgroups: number;
      };
    };

const WGSL_DRAW = /* wgsl */ `
struct VsOut {
  @builtin(position) pos: vec4f,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
  var positions = array<vec2f, 6>(
    vec2f(-1.0, -1.0),
    vec2f( 1.0, -1.0),
    vec2f(-1.0,  1.0),
    vec2f(-1.0,  1.0),
    vec2f( 1.0, -1.0),
    vec2f( 1.0,  1.0),
  );

  var out: VsOut;
  out.pos = vec4f(positions[vid], 0.0, 1.0);
  return out;
}

override COLOR_R: f32 = 1.0;
override COLOR_G: f32 = 0.0;
override COLOR_B: f32 = 0.0;

@fragment
fn fs_main() -> @location(0) vec4f {
  return vec4f(COLOR_R, COLOR_G, COLOR_B, 1.0);
}
`;

const WGSL_COMPUTE = /* wgsl */ `
struct Buf {
  data: array<u32>,
}

@group(0) @binding(0) var<storage, read> src: Buf;
@group(0) @binding(1) var<storage, read_write> dst: Buf;

@compute @workgroup_size(64)
fn cs_main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let idx = gid.x;
  if (idx >= arrayLength(&dst.data)) {
    return;
  }
  var v = src.data[idx];
  // Small ALU loop (texture-decompression-like bit twiddling).
  for (var i: u32 = 0u; i < 64u; i = i + 1u) {
    v = v * 1664525u + 1013904223u;
    v = (v ^ (v >> 16u)) * 2246822519u;
  }
  dst.data[idx] = v;
}
`;

function getNavigatorGpu(): GPU | undefined {
  return (navigator as Navigator & { gpu?: GPU }).gpu;
}

function round3(value: number): number {
  return Math.round(value * 1000) / 1000;
}

function clampInt(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) return min;
  const n = Math.floor(value);
  return Math.min(max, Math.max(min, n));
}

function errToReason(err: unknown): string {
  return formatOneLineError(err, 512);
}

async function getAdapterInfo(adapter: GPUAdapter): Promise<WebGpuBenchAdapterInfo | null> {
  try {
    const adapterAny = adapter as unknown as { info?: GPUAdapterInfo; requestAdapterInfo?: () => Promise<GPUAdapterInfo> };
    const info = adapterAny.info ?? (await adapterAny.requestAdapterInfo?.());
    if (!info) return null;
    return {
      vendor: info.vendor ?? null,
      architecture: (info as unknown as { architecture?: string }).architecture ?? null,
      device: info.device ?? null,
      description: info.description ?? null,
    };
  } catch {
    return null;
  }
}

export async function runWebGpuBench(options: WebGpuBenchOptions = {}): Promise<WebGpuBenchResult> {
  const gpu = getNavigatorGpu();
  if (!gpu) return { supported: false, reason: "navigator.gpu is not available" };

  const frames = clampInt(options.frames ?? 120, 1, 10_000);
  const warmupFrames = clampInt(options.warmupFrames ?? options.warmup_frames ?? 10, 0, 10_000);
  const width = clampInt(options.width ?? 256, 1, 4096);
  const height = clampInt(options.height ?? 256, 1, 4096);
  const drawCallsPerFrame = clampInt(options.drawCallsPerFrame ?? options.draw_calls_per_frame ?? 200, 1, 100_000);
  const pipelineSwitchesPerFrameRequested = clampInt(
    options.pipelineSwitchesPerFrame ?? options.pipeline_switches_per_frame ?? 50,
    0,
    100_000,
  );
  const includeCompute = Boolean(options.compute ?? false);
  const computeWorkgroups = clampInt(options.computeWorkgroups ?? options.compute_workgroups ?? 256, 1, 65_535);

  let adapter: GPUAdapter | null = null;
  try {
    adapter = await gpu.requestAdapter({ powerPreference: "high-performance" });
  } catch (err) {
    return { supported: false, reason: `requestAdapter failed: ${errToReason(err)}` };
  }
  if (!adapter) return { supported: false, reason: "requestAdapter returned null" };

  const wantsTimestampQuery = adapter.features.has("timestamp-query");

  let device: GPUDevice;
  let timestampQueryEnabled = false;
  try {
    device = await adapter.requestDevice({
      requiredFeatures: wantsTimestampQuery ? ["timestamp-query"] : [],
    });
    timestampQueryEnabled = wantsTimestampQuery;
  } catch (err) {
    // Timestamp query is optional; fall back to a basic device.
    try {
      device = await adapter.requestDevice();
    } catch (err2) {
      return { supported: false, reason: `requestDevice failed: ${errToReason(err2)}` };
    }
  }

  timestampQueryEnabled = timestampQueryEnabled && device.features.has("timestamp-query");

  // WebGPU validation/pipeline errors can surface asynchronously as uncaptured errors rather than thrown exceptions.
  // Surface them for bench debugging, but dedupe so repeated validation errors don't flood the console.
  const seenUncapturedErrorKeys = new Set<string>();
  const uncapturedErrorHandler = (ev: unknown) => {
    try {
      (ev as { preventDefault?: () => void } | null | undefined)?.preventDefault?.();
    } catch {
      // Ignore.
    }

    const err = (ev as { error?: unknown } | null | undefined)?.error;
    const ctor = err && typeof err === "object" ? (err as { constructor?: unknown }).constructor : undefined;
    const ctorName = typeof ctor === "function" ? ctor.name : "";
    const errorName =
      (err && typeof err === "object" && typeof (err as { name?: unknown }).name === "string" ? (err as { name: string }).name : "") ||
      ctorName;
    const errorMessage =
      err && typeof err === "object" && typeof (err as { message?: unknown }).message === "string" ? (err as { message: string }).message : "";
    let msg = formatOneLineError(errorMessage || (err ?? "WebGPU uncaptured error"), 512);
    if (errorName && msg && !msg.toLowerCase().startsWith(errorName.toLowerCase())) {
      msg = `${errorName}: ${msg}`;
    }
    if (seenUncapturedErrorKeys.has(msg)) return;
    seenUncapturedErrorKeys.add(msg);
    if (seenUncapturedErrorKeys.size > 128) {
      seenUncapturedErrorKeys.clear();
      seenUncapturedErrorKeys.add(msg);
    }
    console.error("[webgpu-bench] uncapturederror", err ?? ev);
  };
  try {
    const addEventListener = (device as unknown as { addEventListener?: unknown }).addEventListener;
    if (typeof addEventListener === "function") {
      (addEventListener as (type: string, listener: (ev: unknown) => void) => void).call(
        device,
        "uncapturederror",
        uncapturedErrorHandler,
      );
    } else {
      (device as unknown as { onuncapturederror?: unknown }).onuncapturederror = uncapturedErrorHandler;
    }
  } catch {
    // Best-effort.
  }

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  canvas.style.position = "fixed";
  canvas.style.left = "-10000px";
  canvas.style.top = "0";
  document.body.append(canvas);

  let computeSrcBuf: GPUBuffer | null = null;
  let computeDstBuf: GPUBuffer | null = null;
  let queryResolveBuffer: GPUBuffer | null = null;
  let queryReadBuffer: GPUBuffer | null = null;
  let querySet: GPUQuerySet | null = null;

  try {
    const context = (canvas as unknown as { getContext(type: "webgpu"): GPUCanvasContext | null }).getContext("webgpu");
    if (!context) return { supported: false, reason: "canvas.getContext('webgpu') returned null" };
    const configuredContext: GPUCanvasContext = context;

    const format = gpu.getPreferredCanvasFormat();
    configuredContext.configure({
      device,
      format,
      alphaMode: "opaque",
      usage: GPUTextureUsage.RENDER_ATTACHMENT,
    });

    const shader = device.createShaderModule({ code: WGSL_DRAW });
    const pipelineA = device.createRenderPipeline({
      layout: "auto",
      vertex: { module: shader, entryPoint: "vs_main" },
      fragment: {
        module: shader,
        entryPoint: "fs_main",
        constants: { COLOR_R: 1.0, COLOR_G: 0.0, COLOR_B: 0.0 },
        targets: [{ format }],
      },
      primitive: { topology: "triangle-list" },
    });
    const pipelineB = device.createRenderPipeline({
      layout: "auto",
      vertex: { module: shader, entryPoint: "vs_main" },
      fragment: {
        module: shader,
        entryPoint: "fs_main",
        constants: { COLOR_R: 0.0, COLOR_G: 0.8, COLOR_B: 1.0 },
        targets: [{ format }],
      },
      primitive: { topology: "triangle-list" },
    });

    let computePipeline: GPUComputePipeline | null = null;
    let computeBindGroup: GPUBindGroup | null = null;
    if (includeCompute) {
      const computeShader = device.createShaderModule({ code: WGSL_COMPUTE });
      computePipeline = device.createComputePipeline({
        layout: "auto",
        compute: { module: computeShader, entryPoint: "cs_main" },
      });

      const elementCount = computeWorkgroups * 64;
      const byteSize = elementCount * 4;
      const srcInit = new Uint32Array(elementCount);
      for (let i = 0; i < srcInit.length; i += 1) srcInit[i] = (i * 2654435761) >>> 0;

      computeSrcBuf = device.createBuffer({
        size: byteSize,
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST,
      });
      computeDstBuf = device.createBuffer({
        size: byteSize,
        usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC,
      });
      device.queue.writeBuffer(computeSrcBuf, 0, srcInit);

      computeBindGroup = device.createBindGroup({
        layout: computePipeline.getBindGroupLayout(0),
        entries: [
          { binding: 0, resource: { buffer: computeSrcBuf } },
          { binding: 1, resource: { buffer: computeDstBuf } },
        ],
      });
    }

    const queryCount = frames * 2;
    let timestampPeriodNs = 1;

    if (timestampQueryEnabled) {
      try {
        querySet = device.createQuerySet({ type: "timestamp", count: queryCount });
        queryResolveBuffer = device.createBuffer({
          size: queryCount * 8,
          usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC,
        });
        queryReadBuffer = device.createBuffer({
          size: queryCount * 8,
          usage: GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ,
        });

        const queueAny = device.queue as unknown as {
          getTimestampPeriod?: () => number;
        };
        if (typeof queueAny.getTimestampPeriod === "function") {
          const period = queueAny.getTimestampPeriod();
          if (Number.isFinite(period) && period > 0) timestampPeriodNs = period;
        } else {
          const limitsAny = device.limits as unknown as { timestampPeriod?: number };
          if (typeof limitsAny.timestampPeriod === "number" && Number.isFinite(limitsAny.timestampPeriod) && limitsAny.timestampPeriod > 0) {
            timestampPeriodNs = limitsAny.timestampPeriod;
          }
        }
      } catch {
        querySet = null;
        queryResolveBuffer = null;
        queryReadBuffer = null;
      }
    }

    const segmentCount = Math.min(drawCallsPerFrame, pipelineSwitchesPerFrameRequested + 1);
    const pipelineSwitchesPerFrame = Math.max(0, segmentCount - 1);
    const drawsPerSegmentBase = Math.floor(drawCallsPerFrame / segmentCount);
    const drawsPerSegmentRemainder = drawCallsPerFrame % segmentCount;

    const encodeStats = new RunningStats();
    const encodeHistogram = new LogHistogram();

    async function submitFrame(frameIndex: number | null, recordMetrics: boolean): Promise<void> {
      const t0 = performance.now();
      const encoder = device.createCommandEncoder();
      const encoderAny = encoder as unknown as { writeTimestamp?: (qs: GPUQuerySet, index: number) => void };

      const qStart = frameIndex === null ? null : frameIndex * 2;
      const qEnd = qStart === null ? null : qStart + 1;

      if (querySet && qStart !== null && typeof encoderAny.writeTimestamp === "function") {
        encoderAny.writeTimestamp(querySet, qStart);
      }

      if (computePipeline && computeBindGroup) {
        const pass = encoder.beginComputePass();
        pass.setPipeline(computePipeline);
        pass.setBindGroup(0, computeBindGroup);
        pass.dispatchWorkgroups(computeWorkgroups);
        pass.end();
      }

      const view = configuredContext.getCurrentTexture().createView();
      const rpass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view,
            loadOp: "clear",
            storeOp: "store",
            clearValue: { r: 0.02, g: 0.02, b: 0.02, a: 1.0 },
          },
        ],
      });

      const rpassAny = rpass as unknown as { writeTimestamp?: (qs: GPUQuerySet, index: number) => void };
      if (querySet && qStart !== null && typeof encoderAny.writeTimestamp !== "function" && typeof rpassAny.writeTimestamp === "function") {
        rpassAny.writeTimestamp(querySet, qStart);
      }

      for (let seg = 0; seg < segmentCount; seg += 1) {
        const drawsThisSegment = drawsPerSegmentBase + (seg < drawsPerSegmentRemainder ? 1 : 0);
        const pipeline = seg % 2 === 0 ? pipelineA : pipelineB;
        rpass.setPipeline(pipeline);
        for (let d = 0; d < drawsThisSegment; d += 1) rpass.draw(6);
      }

      if (querySet && qEnd !== null && typeof encoderAny.writeTimestamp !== "function" && typeof rpassAny.writeTimestamp === "function") {
        rpassAny.writeTimestamp(querySet, qEnd);
      }

      rpass.end();

      if (querySet && qEnd !== null && typeof encoderAny.writeTimestamp === "function") {
        encoderAny.writeTimestamp(querySet, qEnd);
      }

      if (querySet && queryResolveBuffer && qStart !== null) {
        encoder.resolveQuerySet(querySet, qStart, 2, queryResolveBuffer, qStart * 8);
      }

      device.queue.submit([encoder.finish()]);
      const t1 = performance.now();
      if (recordMetrics) {
        const dt = t1 - t0;
        encodeStats.push(dt);
        encodeHistogram.record(msToUs(dt));
      }

      // Keep swapchain texture allocation deterministic to avoid OOM in tight loops.
      await device.queue.onSubmittedWorkDone();
    }

    for (let i = 0; i < warmupFrames; i += 1) {
      await submitFrame(null, false);
    }

    const startTotal = performance.now();
    for (let i = 0; i < frames; i += 1) {
      await submitFrame(i, true);
    }
    const endTotal = performance.now();

    let gpuTimeMs: number | null = null;
    if (querySet && queryResolveBuffer && queryReadBuffer) {
      const gpuStats = new RunningStats();
      try {
        const copyEncoder = device.createCommandEncoder();
        copyEncoder.copyBufferToBuffer(queryResolveBuffer, 0, queryReadBuffer, 0, queryCount * 8);
        device.queue.submit([copyEncoder.finish()]);

        await queryReadBuffer.mapAsync(GPUMapMode.READ);
        const data = new BigUint64Array(queryReadBuffer.getMappedRange());
        for (let i = 0; i < frames; i += 1) {
          const start = data[i * 2];
          const end = data[i * 2 + 1];
          if (end > start) {
            const deltaTicks = end - start;
            const deltaNs = Number(deltaTicks) * timestampPeriodNs;
            gpuStats.push(deltaNs / 1e6);
          }
        }
        queryReadBuffer.unmap();

        if (gpuStats.count > 0) gpuTimeMs = round3(gpuStats.mean);
      } catch {
        gpuTimeMs = null;
      }
    }

    const totalMs = endTotal - startTotal;
    const fps = totalMs > 0 ? (frames / totalMs) * 1000 : 0;

    const adapterInfo = await getAdapterInfo(adapter);
    const encodeAvg = encodeStats.count > 0 ? encodeStats.mean : 0;
    const encodeP95 = encodeStats.count > 0 ? usToMs(encodeHistogram.quantile(0.95)) : 0;

    return {
      supported: true,
      adapter: adapterInfo,
      capabilities: {
        timestamp_query: querySet !== null,
      },
      frames,
      fps: round3(fps),
      draw_calls_per_frame: drawCallsPerFrame,
      pipeline_switches_per_frame: pipelineSwitchesPerFrame,
      cpu_encode_time_ms: {
        avg: round3(encodeAvg),
        p95: round3(encodeP95),
      },
      gpu_time_ms: gpuTimeMs,
      compute: {
        enabled: includeCompute,
        workgroups: includeCompute ? computeWorkgroups : 0,
      },
    };
  } catch (err) {
    return { supported: false, reason: `bench failed: ${errToReason(err)}` };
  } finally {
    canvas.remove();
    computeSrcBuf?.destroy();
    computeDstBuf?.destroy();
    queryReadBuffer?.destroy();
    queryResolveBuffer?.destroy();
    (querySet as unknown as { destroy?: () => void } | null)?.destroy?.();
    try {
      const removeEventListener = (device as unknown as { removeEventListener?: unknown }).removeEventListener;
      if (typeof removeEventListener === "function") {
        (removeEventListener as (type: string, listener: (ev: unknown) => void) => void).call(
          device,
          "uncapturederror",
          uncapturedErrorHandler,
        );
      }
    } catch {
      // Ignore.
    }
    try {
      const anyDevice = device as unknown as { onuncapturederror?: unknown };
      if (anyDevice.onuncapturederror === uncapturedErrorHandler) {
        anyDevice.onuncapturederror = null;
      }
    } catch {
      // Ignore.
    }
  }
}
