import { startFrameScheduler } from "./src/main/frameScheduler";
import { GPU_PROTOCOL_NAME, GPU_PROTOCOL_VERSION, isGpuWorkerMessageBase } from "./src/ipc/gpu-protocol";
import type { AeroConfig } from "./src/config/aero_config";
import { WorkerCoordinator } from "./src/runtime/coordinator";
import { SHARED_FRAMEBUFFER_HEADER_U32_LEN, SharedFramebufferHeaderIndex } from "./src/ipc/shared-layout";
import { formatOneLineError } from "./src/text";

const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
      sample?: number[];
      expected?: number[];
    };
  }
}

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function renderError(message: string) {
  const status = $("status");
  if (status) status.textContent = message;
  window.__aeroTest = { ready: true, error: message };
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function samplePixel(rgba: Uint8Array, width: number, x: number, y: number): number[] {
  const i = (y * width + x) * 4;
  return [rgba[i + 0] ?? 0, rgba[i + 1] ?? 0, rgba[i + 2] ?? 0, rgba[i + 3] ?? 0];
}

async function main() {
  const canvas = $("frame");
  if (!(canvas instanceof HTMLCanvasElement)) {
    renderError("Canvas element not found");
    return;
  }

  const status = $("status");
  const log = (line: string) => {
    if (status) status.textContent += `${line}\n`;
  };

  if (!("transferControlToOffscreen" in canvas)) {
    renderError("OffscreenCanvas is not supported in this browser.");
    return;
  }

  const coordinator = new WorkerCoordinator();
  const config = {
    // Keep allocations small: this harness only needs enough guest RAM for a few tiny demo buffers.
    // Using 1MiB also prevents embedding the legacy shared framebuffer in guest RAM, keeping the
    // CPU worker on the deterministic JS fallback render path (vs the optional moving WASM demo).
    guestMemoryMiB: 1,
    // This harness uses legacy shared-framebuffer presentation; it does not need the BAR1/VRAM aperture.
    vramMiB: 0,
    enableWorkers: true,
    enableWebGPU: false,
    proxyUrl: null,
    activeDiskImage: null,
    logLevel: "info",
  } satisfies AeroConfig;

  const support = coordinator.checkSupport();
  if (!support.ok) {
    renderError(support.reason ?? "Shared memory unsupported");
    return;
  }

  try {
    coordinator.start(config);
    coordinator.setBootDisks({}, null, null);

    const gpuWorker = coordinator.getWorker("gpu");
    const cpuWorker = coordinator.getWorker("cpu");
    const frameStateSab = coordinator.getFrameStateSab();
    const sharedFramebuffer = coordinator.getSharedFramebuffer();

    if (!gpuWorker || !cpuWorker || !frameStateSab || !sharedFramebuffer) {
      throw new Error("Runtime workers did not expose expected shared resources.");
    }

    const header = new Int32Array(sharedFramebuffer.sab, sharedFramebuffer.offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    const width = Atomics.load(header, SharedFramebufferHeaderIndex.WIDTH);
    const height = Atomics.load(header, SharedFramebufferHeaderIndex.HEIGHT);

    const dpr = 1;
    canvas.width = Math.max(1, Math.round(width * dpr));
    canvas.height = Math.max(1, Math.round(height * dpr));
    canvas.style.width = `${Math.min(width, 320)}px`;
    canvas.style.height = `${Math.min(height, 240)}px`;

    const offscreen = canvas.transferControlToOffscreen();

    const pendingScreenshots = new Map<
      number,
      {
        resolve: (msg: { width: number; height: number; pixels: ArrayBuffer }) => void;
        reject: (err: unknown) => void;
      }
    >();
    let nextRequestId = 1;

    let presenterReadyResolved = false;
    let presenterReadyResolve: (() => void) | null = null;
    let presenterReadyReject: ((err: unknown) => void) | null = null;
    const presenterReady = new Promise<void>((resolve, reject) => {
      presenterReadyResolve = resolve;
      presenterReadyReject = reject;
    });

    gpuWorker.addEventListener("message", (event: MessageEvent) => {
      const msg = event.data as unknown;
      if (!isGpuWorkerMessageBase(msg) || typeof (msg as { type?: unknown }).type !== "string") return;
      const typed = msg as { type: string; requestId?: number; width?: number; height?: number; rgba8?: ArrayBuffer; message?: string };
      if (typed.type === "ready") {
        presenterReadyResolved = true;
        presenterReadyResolve?.();
        presenterReadyResolve = null;
        presenterReadyReject = null;
        return;
      }
      if (typed.type === "screenshot") {
        const pending = pendingScreenshots.get(typed.requestId ?? -1);
        if (!pending) return;
        pendingScreenshots.delete(typed.requestId ?? -1);
        pending.resolve({
          width: Number(typed.width) | 0,
          height: Number(typed.height) | 0,
          pixels: typed.rgba8 ?? new ArrayBuffer(0),
        });
        return;
      }
      if (typed.type === "error") {
        log(`gpu-worker error: ${formatOneLineError(typed.message, 512, "unknown")}`);
        if (!presenterReadyResolved && presenterReadyReject) {
        presenterReadyReject(new Error(formatOneLineError(typed.message, 512, "gpu-worker init error")));
          presenterReadyResolve = null;
          presenterReadyReject = null;
        }
      }
    });

    const frameScheduler = startFrameScheduler({
      gpuWorker,
      sharedFrameState: frameStateSab,
      sharedFramebuffer: sharedFramebuffer.sab,
      sharedFramebufferOffsetBytes: sharedFramebuffer.offsetBytes,
      canvas: offscreen,
      initOptions: {
        forceBackend: "webgl2_raw",
        disableWebGpu: true,
        outputWidth: width,
        outputHeight: height,
        dpr,
      },
      showDebugOverlay: false,
    });

    await presenterReady;

    // Ensure at least one frame has been published by the CPU worker and presented by the GPU worker.
    while (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) < 2) {
      await sleep(5);
    }

    const waitForPresentedAtLeast = async (count: number) => {
      while (frameScheduler.getMetrics().framesPresented < count) {
        await sleep(10);
      }
    };
    await waitForPresentedAtLeast(1);

    const requestScreenshot = (includeCursor: boolean): Promise<{ width: number; height: number; pixels: ArrayBuffer }> => {
      const requestId = nextRequestId++;
      gpuWorker.postMessage({ ...GPU_MESSAGE_BASE, type: "screenshot", requestId, includeCursor });
      return new Promise((resolve, reject) => {
        pendingScreenshots.set(requestId, { resolve, reject });
        setTimeout(() => {
          const pending = pendingScreenshots.get(requestId);
          if (!pending) return;
          pendingScreenshots.delete(requestId);
          reject(new Error("screenshot request timed out"));
        }, 5000);
      });
    };

    cpuWorker.postMessage({ type: "cursorDemo.start" });

    const expected = [0, 0, 255, 255];
    let sample: number[] = [0, 0, 0, 0];

    const deadlineMs = performance.now() + 3000;
    while (performance.now() < deadlineMs) {
      const shot = await requestScreenshot(true);
      const rgba = new Uint8Array(shot.pixels);
      sample = samplePixel(rgba, shot.width, 0, 0);
      if (sample.join(",") === expected.join(",")) break;
      await sleep(25);
    }

    const pass = sample.join(",") === expected.join(",");
    log(`sample=${sample.join(",")}`);
    log(`expected=${expected.join(",")}`);
    log(pass ? "PASS" : "FAIL");

    frameScheduler.stop();
    coordinator.stop();

    window.__aeroTest = { ready: true, pass, sample, expected };
  } catch (err) {
    coordinator.stop();
    renderError(formatOneLineError(err, 512));
  }
}

void main();
