import { FramePacer, type FramePresenter, type FrameSubmission } from "../../../web/src/gpu/frame_pacing";

type ClearFrame = { clearColor: [number, number, number, number] };

declare global {
  interface Window {
    __runFramePacingStressTest?: (options?: {
      durationMs?: number;
      producerIntervalMs?: number;
      maxFramesInFlight?: number;
      simulateWorkDoneDelayMs?: number;
    }) => Promise<unknown>;
    __runWebGpuFramePacingSmokeTest?: () => Promise<unknown>;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

class WebGlFencePresenter implements FramePresenter<ClearFrame> {
  private readonly canvas: HTMLCanvasElement;
  private readonly simulateWorkDoneDelayMs: number;
  private readonly gl: WebGL2RenderingContext;

  constructor(canvas: HTMLCanvasElement, simulateWorkDoneDelayMs: number) {
    this.canvas = canvas;
    this.simulateWorkDoneDelayMs = simulateWorkDoneDelayMs;

    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
    });
    if (!gl) {
      throw new Error("WebGL2 not supported in this environment");
    }
    this.gl = gl;
  }

  onAnimationFrame(): void {
    const dpr = window.devicePixelRatio || 1;
    const rect = this.canvas.getBoundingClientRect();
    const width = Math.max(1, Math.round(rect.width * dpr));
    const height = Math.max(1, Math.round(rect.height * dpr));
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }

  present(frame: ClearFrame): FrameSubmission {
    const gl = this.gl;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.disable(gl.DITHER);
    gl.disable(gl.BLEND);
    gl.disable(gl.DEPTH_TEST);
    gl.disable(gl.CULL_FACE);

    gl.clearColor(frame.clearColor[0], frame.clearColor[1], frame.clearColor[2], frame.clearColor[3]);
    gl.clear(gl.COLOR_BUFFER_BIT);

    const sync = gl.fenceSync(gl.SYNC_GPU_COMMANDS_COMPLETE, 0);
    gl.flush();

    const submittedAt = performance.now();
    const workDone = Promise.all([
      this.waitForSync(sync),
      this.simulateWorkDoneDelayMs > 0 ? sleep(this.simulateWorkDoneDelayMs) : Promise.resolve(),
    ]).then(() => undefined);

    return { submittedAt, workDone };
  }

  private waitForSync(sync: WebGLSync | null): Promise<void> {
    const gl = this.gl;
    if (!sync) return Promise.resolve();

    return new Promise((resolve) => {
      const poll = () => {
        let status: number;
        try {
          status = gl.clientWaitSync(sync, 0, 0);
        } catch {
          gl.deleteSync(sync);
          resolve();
          return;
        }

        if (status === gl.ALREADY_SIGNALED || status === gl.CONDITION_SATISFIED) {
          gl.deleteSync(sync);
          resolve();
          return;
        }

        if (status === gl.WAIT_FAILED) {
          gl.deleteSync(sync);
          resolve();
          return;
        }

        setTimeout(poll, 0);
      };

      poll();
    });
  }
}

window.__runFramePacingStressTest = async (options = {}) => {
  const durationMs = Math.max(100, options.durationMs ?? 2000);
  const producerIntervalMs = Math.max(0, options.producerIntervalMs ?? 0);
  const maxFramesInFlight = Math.max(1, options.maxFramesInFlight ?? 2);
  const simulateWorkDoneDelayMs = Math.max(0, options.simulateWorkDoneDelayMs ?? 20);

  const canvas = document.getElementById("c");
  if (!(canvas instanceof HTMLCanvasElement)) {
    throw new Error("Expected a <canvas id=\"c\"> element");
  }

  const pacer = new FramePacer<ClearFrame>({
    presenter: new WebGlFencePresenter(canvas, simulateWorkDoneDelayMs),
    maxFramesInFlight,
    maxPendingFrames: 1,
  });

  pacer.start();

  let produced = 0;
  const interval = window.setInterval(() => {
    const t = produced;
    produced += 1;
    pacer.enqueue({
      clearColor: [(t % 255) / 255, ((t * 3) % 255) / 255, ((t * 7) % 255) / 255, 1],
    });
  }, producerIntervalMs);

  await sleep(durationMs);
  window.clearInterval(interval);
  await sleep(Math.max(100, simulateWorkDoneDelayMs * maxFramesInFlight));
  pacer.stop();

  return {
    config: { durationMs, producerIntervalMs, maxFramesInFlight, simulateWorkDoneDelayMs },
    produced,
    telemetry: pacer.getTelemetry(),
  };
};

window.__runWebGpuFramePacingSmokeTest = async () => {
  if (!navigator.gpu) {
    return { supported: false };
  }

  const canvas = document.getElementById("c");
  if (!(canvas instanceof HTMLCanvasElement)) {
    throw new Error("Expected a <canvas id=\"c\"> element");
  }

  const context = canvas.getContext("webgpu");
  if (!context) {
    return { supported: false, error: "Canvas WebGPU context not available" };
  }

  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  if (!adapter) {
    return { supported: false, error: "No WebGPU adapter available" };
  }

  let device: GPUDevice;
  try {
    device = await adapter.requestDevice();
  } catch (error) {
    return { supported: false, error: String(error) };
  }

  const format = navigator.gpu.getPreferredCanvasFormat();
  context.configure({ device, format, alphaMode: "opaque" });

  const presenter: FramePresenter<ClearFrame> = {
    present(frame: ClearFrame): FrameSubmission {
      const view = context.getCurrentTexture().createView();
      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({
        colorAttachments: [
          {
            view,
            loadOp: "clear",
            storeOp: "store",
            clearValue: {
              r: frame.clearColor[0],
              g: frame.clearColor[1],
              b: frame.clearColor[2],
              a: frame.clearColor[3],
            },
          },
        ],
      });
      pass.end();
      device.queue.submit([encoder.finish()]);

      const submittedAt = performance.now();
      const workDone = device.queue.onSubmittedWorkDone();
      return { submittedAt, workDone };
    },
  };

  const pacer = new FramePacer<ClearFrame>({
    presenter,
    maxFramesInFlight: 2,
    maxPendingFrames: 1,
  });

  pacer.start();
  for (let i = 0; i < 120; i++) {
    pacer.enqueue({ clearColor: [0, 0, 0, 1] });
  }

  await sleep(500);
  pacer.stop();

  return { supported: true, telemetry: pacer.getTelemetry() };
};

export {};
