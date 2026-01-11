import { fnv1a32Hex } from "./src/utils/fnv1a";
import { startFrameScheduler } from "./src/main/frameScheduler";
import { WorkerCoordinator } from "./src/runtime/coordinator";
import { SHARED_FRAMEBUFFER_HEADER_U32_LEN, SharedFramebufferHeaderIndex } from "./src/ipc/shared-layout";

declare global {
  interface Window {
    __aeroTest?: {
      ready?: boolean;
      error?: string;
      pass?: boolean;
      hashes?: string[];
      samples?: {
        topLeftSeen: number[][];
        topRight: number[];
      };
      metrics?: { framesReceived: number; framesPresented: number; framesDropped: number };
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
    guestMemoryMiB: 256,
    enableWorkers: true,
    enableWebGPU: false,
    proxyUrl: null,
    activeDiskImage: null,
    logLevel: "info",
  } as const;

  const support = coordinator.checkSupport();
  if (!support.ok) {
    renderError(support.reason ?? "Shared memory unsupported");
    return;
  }

  coordinator.start(config as any);

  const gpuWorker = coordinator.getWorker("gpu");
  const frameStateSab = coordinator.getFrameStateSab();
  const sharedFramebuffer = coordinator.getSharedFramebuffer();

  if (!gpuWorker || !frameStateSab || !sharedFramebuffer) {
    renderError("Runtime workers did not expose expected GPU worker + shared memory.");
    return;
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
    const msg = event.data as any;
    if (!msg || typeof msg !== "object") return;
    if (msg.type === "ready") {
      presenterReadyResolved = true;
      presenterReadyResolve?.();
      presenterReadyResolve = null;
      presenterReadyReject = null;
      return;
    }
    if (msg.type === "screenshot") {
      const pending = pendingScreenshots.get(msg.requestId);
      if (!pending) return;
      pendingScreenshots.delete(msg.requestId);
      pending.resolve({ width: msg.width, height: msg.height, pixels: msg.rgba8 });
      return;
    }
    if (msg.type === "error") {
      log(`gpu-worker error: ${msg.message ?? "unknown"}`);
      if (!presenterReadyResolved && presenterReadyReject) {
        presenterReadyReject(new Error(String(msg.message ?? "gpu-worker init error")));
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

  // Wait for at least one frame to be published and presented.
  while (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) < 2) {
    await sleep(5);
  }

  const waitForPresentedAtLeast = async (count: number) => {
    while (frameScheduler.getMetrics().framesPresented < count) {
      await sleep(10);
    }
  };
  await waitForPresentedAtLeast(1);

  const requestScreenshot = (): Promise<{ width: number; height: number; pixels: ArrayBuffer }> => {
    const requestId = nextRequestId++;
    gpuWorker.postMessage({ type: "screenshot", requestId });
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

  const hashes: string[] = [];
  const topLeftSeen: number[][] = [];

  // Capture multiple screenshots until we observe both tile colors (red/green) in the top-left.
  const seen = new Set<string>();
  const ok = new Set(["0,255,0,255", "255,0,0,255"]);
  let topRight: number[] = [0, 0, 0, 0];

  for (let i = 0; i < 10; i += 1) {
    await waitForPresentedAtLeast(i + 1);
    const shot = await requestScreenshot();
    const rgba = new Uint8Array(shot.pixels);
    const hash = fnv1a32Hex(rgba);
    hashes.push(hash);

    const tl = samplePixel(rgba, shot.width, 0, 0);
    topLeftSeen.push(tl);
    seen.add(tl.join(","));

    topRight = samplePixel(rgba, shot.width, shot.width - 1, 0);

    if (seen.size >= 2) break;
    await sleep(25);
  }

  const pass = seen.size >= 2 && Array.from(seen).every((k) => ok.has(k)) && topRight.join(",") === "0,255,0,255";

  const metrics = frameScheduler.getMetrics();
  log(`framesReceived=${metrics.framesReceived} framesPresented=${metrics.framesPresented} framesDropped=${metrics.framesDropped}`);
  log(`topLeftSeen=${JSON.stringify(topLeftSeen)}`);
  log(`topRight=${JSON.stringify(topRight)}`);
  log(pass ? "PASS" : "FAIL");

  frameScheduler.stop();
  coordinator.stop();

  window.__aeroTest = {
    ready: true,
    pass,
    hashes,
    samples: { topLeftSeen, topRight },
    metrics,
  };
}

void main().catch((err) => {
  renderError(err instanceof Error ? err.message : String(err));
});
