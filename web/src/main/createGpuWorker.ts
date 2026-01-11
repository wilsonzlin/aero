import {
  computeSharedFramebufferLayout,
  FramebufferFormat,
  SharedFramebufferHeaderIndex,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
} from "../ipc/shared-layout";
import { FRAME_DIRTY, FRAME_PRESENTED, FRAME_SEQ_INDEX, FRAME_STATUS_INDEX } from "../shared/frameProtocol";
import { perf } from "../perf/perf";
import type {
  GpuRuntimeInitOptions,
  GpuRuntimeEventsMessage,
  GpuRuntimeOutMessage,
  GpuRuntimeReadyMessage,
  GpuRuntimeScreenshotResponseMessage,
  GpuRuntimeSubmitCompleteMessage,
  GpuRuntimeStatsMessage,
} from "../workers/gpu_runtime_protocol";

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
  submitAerogpu(cmdStream: ArrayBuffer, signalFence: bigint, allocTable?: ArrayBuffer): Promise<GpuRuntimeSubmitCompleteMessage>;
  requestScreenshot(): Promise<GpuRuntimeScreenshotResponseMessage>;
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

  const strideBytes = params.width * 4;
  const layout = computeSharedFramebufferLayout(params.width, params.height, strideBytes, FramebufferFormat.RGBA8, 0);

  const sharedFramebuffer = new SharedArrayBuffer(layout.totalBytes);
  const header = new Int32Array(sharedFramebuffer, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

  // Shared frame pacing state (mirrors the layout in `src/shared/frameProtocol.ts`).
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
  const submitRequests = new Map<
    number,
    { resolve: (msg: GpuRuntimeSubmitCompleteMessage) => void; reject: (err: unknown) => void }
  >();

  function rejectAllPending(err: unknown): void {
    for (const [, pending] of screenshotRequests) {
      pending.reject(err);
    }
    screenshotRequests.clear();
    for (const [, pending] of submitRequests) {
      pending.reject(err);
    }
    submitRequests.clear();
  }

  worker.addEventListener("message", (event) => {
    const msg = event.data as GpuRuntimeOutMessage;
    if (!msg || typeof msg !== "object" || typeof (msg as { type?: unknown }).type !== "string") return;

    switch (msg.type) {
      case "ready":
        readySettled = true;
        readyResolve(msg);
        break;
      case "screenshot": {
        const pending = screenshotRequests.get(msg.requestId);
        if (!pending) return;
        screenshotRequests.delete(msg.requestId);
        pending.resolve(msg);
        break;
      }
      case "submit_complete": {
        const pending = submitRequests.get(msg.requestId);
        if (!pending) return;
        submitRequests.delete(msg.requestId);
        pending.resolve(msg);
        break;
      }
      case "error": {
        params.onError?.(msg);
        const err = new Error(`gpu-worker error: ${msg.message}`);
        if (!readySettled) {
          readySettled = true;
          readyReject(err);
        }
        rejectAllPending(err);
        break;
      }
      case "stats":
        params.onStats?.(msg);
        break;
      case "events":
        params.onEvents?.(msg);
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
    worker.postMessage({ type: "resize", width, height, dpr: devicePixelRatio });
  }

  function presentTestPattern(): void {
    publishFrame(createTestPattern(params.width, params.height));
    worker.postMessage({ type: "tick", frameTimeMs: performance.now() });
  }

  function submitAerogpu(
    cmdStream: ArrayBuffer,
    signalFence: bigint,
    allocTable?: ArrayBuffer,
  ): Promise<GpuRuntimeSubmitCompleteMessage> {
    return ready.then(
      () => {
        const requestId = nextRequestId++;
        const transfer: Transferable[] = [cmdStream];
        if (allocTable) transfer.push(allocTable);
        worker.postMessage(
          {
            type: "submit_aerogpu",
            requestId,
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

  function requestScreenshot(): Promise<GpuRuntimeScreenshotResponseMessage> {
    return ready.then(
      () => {
        const requestId = nextRequestId++;
        worker.postMessage({ type: "screenshot", requestId });
        return new Promise<GpuRuntimeScreenshotResponseMessage>((resolve, reject) => {
          screenshotRequests.set(requestId, { resolve, reject });
        });
      },
      (err) => Promise.reject(err),
    );
  }

  function shutdown(): void {
    worker.postMessage({ type: "shutdown" });
    worker.terminate();
  }

  return {
    worker,
    ready,
    resize,
    presentTestPattern,
    submitAerogpu,
    requestScreenshot,
    shutdown,
  };
}
