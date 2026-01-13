import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import {
  FRAME_DIRTY,
  FRAME_PRESENTED,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type GpuRuntimeEventsMessage,
  type GpuRuntimeInitOptions,
  type GpuRuntimeOutMessage,
  type GpuRuntimeReadyMessage,
  type GpuRuntimeScreenshotResponseMessage,
  type GpuRuntimeScreenshotPresentedResponseMessage,
  type GpuRuntimeSubmitCompleteMessage,
  type GpuRuntimeStatsMessage,
} from "../ipc/gpu-protocol";
import { perf } from "../perf/perf";

export interface CreateGpuWorkerParams {
  canvas: HTMLCanvasElement;
  width: number;
  height: number;
  devicePixelRatio: number;
  gpuOptions?: GpuRuntimeInitOptions;
  onError?: (msg: Extract<GpuRuntimeOutMessage, { type: "error" }>) => void;
  onStats?: (msg: GpuRuntimeStatsMessage) => void;
  onEvents?: (msg: GpuRuntimeEventsMessage) => void;
}

export interface GpuWorkerHandle {
  worker: Worker;
  ready: Promise<GpuRuntimeReadyMessage>;
  resize(width: number, height: number, devicePixelRatio: number): void;
  presentTestPattern(): void;
  /**
   * Publish an RGBA8 frame (top-left origin) into the shared framebuffer and trigger a tick.
   */
  presentRgba8(rgba8: Uint8Array): void;
  submitAerogpu(
    cmdStream: ArrayBuffer,
    signalFence: bigint,
    allocTable?: ArrayBuffer,
    contextId?: number,
  ): Promise<GpuRuntimeSubmitCompleteMessage>;
  /**
   * Request a deterministic screenshot from the GPU worker.
   *
   * The returned pixels are a readback of the *source framebuffer* content
   * (pre-scaling / pre-color-management), not a capture of the presented canvas.
   */
  requestScreenshot(opts?: { includeCursor?: boolean }): Promise<GpuRuntimeScreenshotResponseMessage>;
  /**
   * Debug-only: read back the *presented* pixels from the worker's output canvas (RGBA8, top-left origin).
   *
   * This is intended for validating presentation policy (scaling/letterboxing, sRGB/alpha, etc).
   * It is intentionally separate from `requestScreenshot()`, which returns source framebuffer bytes
   * for deterministic hashing.
   *
   * Note: the underlying worker API is best-effort; if a backend cannot read back presented
   * output yet it may fall back to returning a source-framebuffer screenshot.
   */
  requestPresentedScreenshot(opts?: { includeCursor?: boolean }): Promise<GpuRuntimeScreenshotPresentedResponseMessage>;
  shutdown(): void;
}

function createTestPattern(width: number, height: number): Uint8Array {
  const halfW = Math.floor(width / 2);
  const halfH = Math.floor(height / 2);
  const out = new Uint8Array(width * height * 4);

  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const i = (y * width + x) * 4;
      const isLeft = x < halfW;
      const isTop = y < halfH;

      // Top-left origin:
      // - top-left: red
      // - top-right: green
      // - bottom-left: blue
      // - bottom-right: white
      let r = 0;
      let g = 0;
      let b = 0;
      if (isTop && isLeft) {
        r = 255;
      } else if (isTop && !isLeft) {
        g = 255;
      } else if (!isTop && isLeft) {
        b = 255;
      } else {
        r = 255;
        g = 255;
        b = 255;
      }

      out[i + 0] = r;
      out[i + 1] = g;
      out[i + 2] = b;
      out[i + 3] = 255;
    }
  }

  return out;
}

export function createGpuWorker(params: CreateGpuWorkerParams): GpuWorkerHandle {
  if (!("transferControlToOffscreen" in params.canvas)) {
    throw new Error("OffscreenCanvas is not supported in this browser.");
  }

  const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

  const strideBytes = params.width * 4;
  const layout = computeSharedFramebufferLayout(params.width, params.height, strideBytes, FramebufferFormat.RGBA8, 0);

  const sharedFramebuffer = new SharedArrayBuffer(layout.totalBytes);
  const header = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

  // Shared frame pacing state (mirrors the layout in `src/ipc/gpu-protocol.ts` - FRAME_* constants).
  const sharedFrameState = new SharedArrayBuffer(8 * Int32Array.BYTES_PER_ELEMENT);
  const frameState = new Int32Array(sharedFrameState);

  // Initialize the shared framebuffer header.
  Atomics.store(header, SharedFramebufferHeaderIndex.MAGIC, SHARED_FRAMEBUFFER_MAGIC);
  Atomics.store(header, SharedFramebufferHeaderIndex.VERSION, SHARED_FRAMEBUFFER_VERSION);
  Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, params.width);
  Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, params.height);
  Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, strideBytes);
  Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, FramebufferFormat.RGBA8);
  Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_X, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.TILES_Y, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.DIRTY_WORDS_PER_BUFFER, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ, 0);
  Atomics.store(header, SharedFramebufferHeaderIndex.FLAGS, 0);

  Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED);
  Atomics.store(frameState, FRAME_SEQ_INDEX, 0);

  const slot0 = new Uint8Array(sharedFramebuffer, layout.framebufferOffsets[0], strideBytes * params.height);
  const slot1 = new Uint8Array(sharedFramebuffer, layout.framebufferOffsets[1], strideBytes * params.height);

  let activeIndex = 0;

  const publishFrame = (rgba8: Uint8Array) => {
    const back = activeIndex ^ 1;
    const dst = back === 0 ? slot0 : slot1;
    dst.set(rgba8);

    const newSeq = (Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ) + 1) | 0;
    Atomics.store(
      header,
      back === 0 ? SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ : SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ,
      newSeq,
    );
    Atomics.store(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX, back);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_SEQ, newSeq);
    Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 1);
    Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_SEQ, 1);
    activeIndex = back;

    Atomics.store(frameState, FRAME_SEQ_INDEX, newSeq);
    Atomics.store(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY);
  };

  // IMPORTANT: Keep the `new Worker(new URL(..., import.meta.url), ...)` shape so
  // Vite can statically detect and bundle workers.
  const worker = new Worker(new URL("../workers/gpu.worker.ts", import.meta.url), { type: "module" });
  perf.registerWorker(worker, { threadName: "gpu-presenter" });
  if (perf.traceEnabled) perf.instant("boot:worker:spawn", "p", { role: "gpu-presenter" });

  const offscreen = params.canvas.transferControlToOffscreen();

  let readyResolve: (msg: GpuRuntimeReadyMessage) => void;
  let readyReject: (err: unknown) => void;
  let readySettled = false;

  const ready = new Promise<GpuRuntimeReadyMessage>((resolve, reject) => {
    readyResolve = resolve;
    readyReject = reject;
  });

  let nextRequestId = 1;
  const screenshotRequests = new Map<
    number,
    { resolve: (msg: GpuRuntimeScreenshotResponseMessage) => void; reject: (err: unknown) => void }
  >();
  const presentedScreenshotRequests = new Map<
    number,
    { resolve: (msg: GpuRuntimeScreenshotPresentedResponseMessage) => void; reject: (err: unknown) => void }
  >();
  const submitRequests = new Map<
    number,
    { resolve: (msg: GpuRuntimeSubmitCompleteMessage) => void; reject: (err: unknown) => void }
  >();

  function rejectAllPending(err: unknown): void {
    for (const [, pending] of screenshotRequests) {
      pending.reject(err);
    }
    screenshotRequests.clear();
    for (const [, pending] of presentedScreenshotRequests) {
      pending.reject(err);
    }
    presentedScreenshotRequests.clear();
    for (const [, pending] of submitRequests) {
      pending.reject(err);
    }
    submitRequests.clear();
  }

  worker.addEventListener("message", (event) => {
    const msg = event.data as unknown;
    if (!isGpuWorkerMessageBase(msg) || typeof (msg as { type?: unknown }).type !== "string") return;

    const typed = msg as GpuRuntimeOutMessage;

    switch (typed.type) {
      case "ready":
        readySettled = true;
        readyResolve(typed);
        break;
      case "screenshot": {
        const pending = screenshotRequests.get(typed.requestId);
        if (!pending) return;
        screenshotRequests.delete(typed.requestId);
        pending.resolve(typed);
        break;
      }
      case "screenshot_presented": {
        const pending = presentedScreenshotRequests.get(typed.requestId);
        if (!pending) return;
        presentedScreenshotRequests.delete(typed.requestId);
        pending.resolve(typed as GpuRuntimeScreenshotPresentedResponseMessage);
        break;
      }
      case "submit_complete": {
        const pending = submitRequests.get(typed.requestId);
        if (!pending) return;
        submitRequests.delete(typed.requestId);
        pending.resolve(typed);
        break;
      }
      case "error": {
        params.onError?.(typed);
        const err = new Error(`gpu-worker error: ${typed.message}`);
        if (!readySettled) {
          readySettled = true;
          readyReject(err);
        }
        rejectAllPending(err);
        break;
      }
      case "stats":
        params.onStats?.(typed);
        break;
      case "events":
        params.onEvents?.(typed);
        break;
      default:
        break;
    }
  });

  worker.addEventListener("error", (event) => {
    const err = (event as ErrorEvent).error ?? event;
    if (!readySettled) {
      readySettled = true;
      readyReject(err);
    }
    rejectAllPending(err);
  });

  const mergedOptions: GpuRuntimeInitOptions = {
    ...(params.gpuOptions ?? {}),
    outputWidth: params.width,
    outputHeight: params.height,
    dpr: params.devicePixelRatio,
  };

  worker.postMessage(
    {
      ...GPU_MESSAGE_BASE,
      type: "init",
      canvas: offscreen,
      sharedFrameState,
      sharedFramebuffer,
      sharedFramebufferOffsetBytes: 0,
      options: mergedOptions,
    },
    [offscreen],
  );

  function resize(width: number, height: number, devicePixelRatio: number): void {
    worker.postMessage({ ...GPU_MESSAGE_BASE, type: "resize", width, height, dpr: devicePixelRatio });
  }

  function presentTestPattern(): void {
    publishFrame(createTestPattern(params.width, params.height));
    worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
  }

  function presentRgba8(rgba8: Uint8Array): void {
    publishFrame(rgba8);
    worker.postMessage({ ...GPU_MESSAGE_BASE, type: "tick", frameTimeMs: performance.now() });
  }

  function submitAerogpu(
    cmdStream: ArrayBuffer,
    signalFence: bigint,
    allocTable?: ArrayBuffer,
    contextId = 0,
  ): Promise<GpuRuntimeSubmitCompleteMessage> {
    return ready.then(
      () => {
        const requestId = nextRequestId++;
        const transfer: Transferable[] = [cmdStream];
        if (allocTable) transfer.push(allocTable);
        const normalizedContextId = Number.isFinite(contextId) ? contextId >>> 0 : 0;
        worker.postMessage(
          {
            ...GPU_MESSAGE_BASE,
            type: "submit_aerogpu",
            requestId,
            contextId: normalizedContextId,
            signalFence,
            cmdStream,
            ...(allocTable ? { allocTable } : {}),
          },
          transfer,
        );
        return new Promise<GpuRuntimeSubmitCompleteMessage>((resolve, reject) => {
          submitRequests.set(requestId, { resolve, reject });
        });
      },
      (err) => Promise.reject(err),
    );
  }

  function requestScreenshot(opts?: { includeCursor?: boolean }): Promise<GpuRuntimeScreenshotResponseMessage> {
    return ready.then(
      () => {
        const requestId = nextRequestId++;
        const includeCursor = opts?.includeCursor === true;
        worker.postMessage({
          ...GPU_MESSAGE_BASE,
          type: "screenshot",
          requestId,
          ...(includeCursor ? { includeCursor: true } : {}),
        });
        return new Promise<GpuRuntimeScreenshotResponseMessage>((resolve, reject) => {
          screenshotRequests.set(requestId, { resolve, reject });
        });
      },
      (err) => Promise.reject(err),
    );
  }

  function requestPresentedScreenshot(
    opts?: { includeCursor?: boolean },
  ): Promise<GpuRuntimeScreenshotPresentedResponseMessage> {
    return ready.then(
      () => {
        const requestId = nextRequestId++;
        const includeCursor = opts?.includeCursor === true;
        worker.postMessage({
          ...GPU_MESSAGE_BASE,
          type: "screenshot_presented",
          requestId,
          ...(includeCursor ? { includeCursor: true } : {}),
        });
        return new Promise<GpuRuntimeScreenshotPresentedResponseMessage>((resolve, reject) => {
          presentedScreenshotRequests.set(requestId, { resolve, reject });
        });
      },
      (err) => Promise.reject(err),
    );
  }

  function shutdown(): void {
    worker.postMessage({ ...GPU_MESSAGE_BASE, type: "shutdown" });
    worker.terminate();
  }

  return {
    worker,
    ready,
    resize,
    presentTestPattern,
    presentRgba8,
    submitAerogpu,
    requestScreenshot,
    requestPresentedScreenshot,
    shutdown,
  };
}
