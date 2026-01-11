import type { PresenterBackendKind, PresenterScaleMode } from "./gpu/presenter";
import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
} from "./shared/frameProtocol";
import {
  HEADER_INDEX_FRAME_COUNTER,
  initFramebufferHeader,
  requiredFramebufferBytes,
  wrapSharedFramebuffer,
} from "./display/framebuffer_protocol";
import type {
  GpuRuntimeInMessage,
  GpuRuntimeOutMessage,
  GpuRuntimeReadyMessage,
  GpuRuntimeScreenshotResponseMessage,
} from "./workers/gpu_runtime_protocol";

declare global {
  interface Window {
    __aeroTest?: {
      init: (opts: {
        width: number;
        height: number;
        dpr: number;
        forceBackend?: PresenterBackendKind;
        scaleMode?: PresenterScaleMode;
      }) => Promise<GpuRuntimeReadyMessage>;
      present: (pixels: Uint8Array, strideBytes: number) => void;
      screenshot: () => Promise<{ width: number; height: number; pixels: Uint8Array }>;
    };
  }
}

const canvasEl = document.getElementById("canvas");
if (!(canvasEl instanceof HTMLCanvasElement)) {
  throw new Error("Canvas element not found");
}
const canvas: HTMLCanvasElement = canvasEl;

let worker: Worker | null = null;
let sharedFramebuffer: SharedArrayBuffer | null = null;
let sharedFrameState: SharedArrayBuffer | null = null;
let frameState: Int32Array | null = null;
let fbHeader: Int32Array | null = null;
let fbPixels: Uint8Array | null = null;

let fbWidth = 0;
let fbHeight = 0;
let fbStrideBytes = 0;

let readyPromise: Promise<GpuRuntimeReadyMessage> | null = null;
let readyResolve: ((msg: GpuRuntimeReadyMessage) => void) | null = null;
let readyReject: ((err: unknown) => void) | null = null;

let nextRequestId = 1;
const pendingScreenshots = new Map<
  number,
  { resolve: (msg: GpuRuntimeScreenshotResponseMessage) => void; reject: (err: unknown) => void }
>();

function rejectPending(err: unknown): void {
  for (const pending of pendingScreenshots.values()) pending.reject(err);
  pendingScreenshots.clear();
}

function destroyWorker(): void {
  if (worker) {
    try {
      worker.postMessage({ type: "shutdown" } satisfies GpuRuntimeInMessage);
    } catch {
      // Ignore and just terminate below.
    }
    worker.terminate();
  }
  worker = null;
  readyPromise = null;
  readyResolve = null;
  readyReject = null;
  rejectPending(new Error("GPU worker destroyed"));
}

async function waitForPresented(timeoutMs = 2_000): Promise<void> {
  if (!frameState || !worker) throw new Error("GPU worker not initialized");
  const start = performance.now();

  while (performance.now() - start < timeoutMs) {
    const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
    if (status === FRAME_PRESENTED) return;
    if (status === FRAME_DIRTY) {
      worker.postMessage({ type: "tick", frameTimeMs: performance.now() } satisfies GpuRuntimeInMessage);
    }
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
  }

  throw new Error("Timed out waiting for GPU worker to present frame");
}

async function init(opts: {
  width: number;
  height: number;
  dpr: number;
  forceBackend?: PresenterBackendKind;
  scaleMode?: PresenterScaleMode;
}): Promise<GpuRuntimeReadyMessage> {
  destroyWorker();

  fbWidth = Math.max(1, Math.floor(opts.width));
  fbHeight = Math.max(1, Math.floor(opts.height));
  fbStrideBytes = fbWidth * 4;

  canvas.style.width = `${fbWidth}px`;
  canvas.style.height = `${fbHeight}px`;

  const requiredBytes = requiredFramebufferBytes(fbWidth, fbHeight, fbStrideBytes);
  sharedFramebuffer = new SharedArrayBuffer(requiredBytes);
  const fbView = wrapSharedFramebuffer(sharedFramebuffer, 0);
  fbHeader = fbView.header;
  fbPixels = fbView.pixelsU8;
  initFramebufferHeader(fbHeader, { width: fbWidth, height: fbHeight, strideBytes: fbStrideBytes });

  sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
  frameState = new Int32Array(sharedFrameState);
  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
  Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

  type TransferCanvas = HTMLCanvasElement & { transferControlToOffscreen: () => OffscreenCanvas };
  if (typeof (canvas as Partial<TransferCanvas>).transferControlToOffscreen !== "function") {
    throw new Error("OffscreenCanvas is not supported in this browser.");
  }

  const offscreen = (canvas as TransferCanvas).transferControlToOffscreen();
  worker = new Worker(new URL("./workers/gpu.worker.ts", import.meta.url), { type: "module" });

  readyPromise = new Promise<GpuRuntimeReadyMessage>((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });

  worker.addEventListener("message", (event) => {
    const msg = event.data as GpuRuntimeOutMessage;
    if (!msg || typeof msg !== "object" || typeof (msg as { type?: unknown }).type !== "string") return;

    switch (msg.type) {
      case "ready":
        readyResolve?.(msg);
        break;
      case "screenshot": {
        const pending = pendingScreenshots.get(msg.requestId);
        if (!pending) return;
        pendingScreenshots.delete(msg.requestId);
        pending.resolve(msg);
        break;
      }
      case "error": {
        const err = new Error(msg.message);
        if (readyReject) readyReject(err);
        rejectPending(err);
        break;
      }
      default:
        break;
    }
  });

  worker.addEventListener("error", (event) => {
    const err = (event as ErrorEvent).error ?? event;
    if (readyReject) readyReject(err);
    rejectPending(err);
  });

  worker.postMessage(
    {
      type: "init",
      canvas: offscreen,
      sharedFrameState,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      options: {
        forceBackend: opts.forceBackend,
        outputWidth: fbWidth,
        outputHeight: fbHeight,
        dpr: opts.dpr,
        presenter: {
          scaleMode: opts.scaleMode,
        },
      },
    } satisfies GpuRuntimeInMessage,
    [offscreen],
  );

  return await readyPromise;
}

function present(pixels: Uint8Array, strideBytes: number): void {
  if (!worker || !fbPixels || !fbHeader || !frameState) throw new Error("GPU worker not initialized");
  if (strideBytes !== fbStrideBytes) {
    throw new Error(`Unexpected strideBytes: got=${strideBytes} expected=${fbStrideBytes}`);
  }

  const expectedBytes = fbStrideBytes * fbHeight;
  if (pixels.byteLength < expectedBytes) {
    throw new Error(`Frame buffer too small: got=${pixels.byteLength} expected at least ${expectedBytes}`);
  }

  fbPixels.set(pixels.subarray(0, expectedBytes));

  const newSeq = (Atomics.add(fbHeader, HEADER_INDEX_FRAME_COUNTER, 1) + 1) | 0;
  Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);

  worker.postMessage({ type: "tick", frameTimeMs: performance.now() } satisfies GpuRuntimeInMessage);
}

async function screenshot(): Promise<{ width: number; height: number; pixels: Uint8Array }> {
  if (!worker || !readyPromise) throw new Error("GPU worker not initialized");
  await readyPromise;

  await waitForPresented();

  const requestId = nextRequestId++;
  worker.postMessage({ type: "screenshot", requestId } satisfies GpuRuntimeInMessage);
  const msg = await new Promise<GpuRuntimeScreenshotResponseMessage>((resolve, reject) => {
    pendingScreenshots.set(requestId, { resolve, reject });
    setTimeout(() => {
      const pending = pendingScreenshots.get(requestId);
      if (!pending) return;
      pendingScreenshots.delete(requestId);
      reject(new Error("Screenshot request timed out"));
    }, 2_000);
  });

  return { width: msg.width, height: msg.height, pixels: new Uint8Array(msg.rgba8) };
}

window.__aeroTest = { init, present, screenshot };
window.addEventListener("beforeunload", () => destroyWorker());
