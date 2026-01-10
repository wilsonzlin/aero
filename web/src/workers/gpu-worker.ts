/// <reference lib="webworker" />

import {
  FRAME_DIRTY,
  FRAME_METRICS_DROPPED_INDEX,
  FRAME_METRICS_PRESENTED_INDEX,
  FRAME_METRICS_RECEIVED_INDEX,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
  type DirtyRect,
  type FrameTimingsReport,
  type GpuWorkerMessageFromMain,
  type GpuWorkerMessageToMain,
} from '../shared/frameProtocol';

import {
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from '../ipc/shared-layout';

import { GpuTelemetry } from '../../gpu/telemetry.ts';
type PresentFn = (dirtyRects?: DirtyRect[] | null) => void | boolean | Promise<void | boolean>;
type GetTimingsFn = () => FrameTimingsReport | null | Promise<FrameTimingsReport | null>;

const postToMain = (msg: GpuWorkerMessageToMain) => {
  self.postMessage(msg);
};

let frameState: Int32Array | null = null;

let presentFn: PresentFn | null = null;
let getTimingsFn: GetTimingsFn | null = null;
let presenting = false;

let pendingDirtyRects: DirtyRect[] | null = null;
let pendingFrames = 0;

let framesReceived = 0;
let framesPresented = 0;
let framesDropped = 0;

let lastSeenSeq = 0;
let lastPresentedSeq = 0;

let lastMetricsPostAtMs = 0;
const METRICS_POST_INTERVAL_MS = 250;

let latestTimings: FrameTimingsReport | null = null;
type SharedFramebufferViews = {
  header: Int32Array;
  layout: SharedFramebufferLayout;
  slot0: Uint8Array;
  slot1: Uint8Array;
  dirty0: Uint32Array | null;
  dirty1: Uint32Array | null;
};

let framebufferViews: SharedFramebufferViews | null = null;

const tryInitSharedFramebufferViews = () => {
  if (framebufferViews) return;

  const initMsg = lastInitMessage;
  if (!initMsg?.sharedFramebuffer) return;

  const offsetBytes = initMsg.sharedFramebufferOffsetBytes ?? 0;
  const header = new Int32Array(initMsg.sharedFramebuffer, offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);

  const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC);
  const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION);
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) {
    return;
  }

  try {
    const layout = layoutFromHeader(header);
    const slot0 = new Uint8Array(
      initMsg.sharedFramebuffer,
      offsetBytes + layout.framebufferOffsets[0],
      layout.strideBytes * layout.height,
    );
    const slot1 = new Uint8Array(
      initMsg.sharedFramebuffer,
      offsetBytes + layout.framebufferOffsets[1],
      layout.strideBytes * layout.height,
    );

    const dirty0 =
      layout.dirtyWordsPerBuffer === 0
        ? null
        : new Uint32Array(initMsg.sharedFramebuffer, offsetBytes + layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
    const dirty1 =
      layout.dirtyWordsPerBuffer === 0
        ? null
        : new Uint32Array(initMsg.sharedFramebuffer, offsetBytes + layout.dirtyOffsets[1], layout.dirtyWordsPerBuffer);

    framebufferViews = { header, layout, slot0, slot1, dirty0, dirty1 };

    // Expose on the worker global so the dynamically-imported presenter module
    // can read the framebuffer without plumbing arguments through `present()`.
    // This keeps the hot path (per-frame) allocation-free and avoids `postMessage`
    // copies.
    (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer = framebufferViews;
  } catch {
    // Header likely not initialized yet; caller should retry later.
  }
};

let lastInitMessage: Extract<GpuWorkerMessageFromMain, { type: 'init' }> | null = null;

const telemetry = new GpuTelemetry({ frameBudgetMs: Number.POSITIVE_INFINITY });
let lastFrameStartMs: number | null = null;

const syncSharedMetrics = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_METRICS_DROPPED_INDEX) return;

  Atomics.store(frameState, FRAME_METRICS_RECEIVED_INDEX, framesReceived);
  Atomics.store(frameState, FRAME_METRICS_PRESENTED_INDEX, framesPresented);
  Atomics.store(frameState, FRAME_METRICS_DROPPED_INDEX, framesDropped);
};

const maybePostMetrics = () => {
  const nowMs = performance.now();
  if (nowMs - lastMetricsPostAtMs < METRICS_POST_INTERVAL_MS) return;

  lastMetricsPostAtMs = nowMs;
  syncSharedMetrics();
  telemetry.droppedFrames = framesDropped;
  postToMain({
    type: 'metrics',
    framesReceived,
    framesPresented,
    framesDropped,
    telemetry: telemetry.snapshot(),
  });
};

const loadPresentFnFromModuleUrl = async (wasmModuleUrl: string) => {
  const mod: unknown = await import(/* @vite-ignore */ wasmModuleUrl);

  const maybePresent = (mod as { present?: unknown }).present;
  if (typeof maybePresent !== 'function') {
    throw new Error(`Module ${wasmModuleUrl} did not export a present() function`);
  }
  presentFn = maybePresent as PresentFn;

  const maybeGetTimings =
    (mod as { get_frame_timings?: unknown }).get_frame_timings ??
    (mod as { getFrameTimings?: unknown }).getFrameTimings;
  getTimingsFn = typeof maybeGetTimings === 'function' ? (maybeGetTimings as GetTimingsFn) : null;
};

const maybeUpdateFramesReceivedFromSeq = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_SEQ_INDEX) return;

  const seq = Atomics.load(frameState, FRAME_SEQ_INDEX);
  if (seq === lastSeenSeq) return;

  const delta = seq - lastSeenSeq;
  if (delta > 0) framesReceived += delta;
  lastSeenSeq = seq;
};

const shouldPresentWithSharedState = () => {
  if (!frameState) return false;
  const status = Atomics.load(frameState, FRAME_STATUS_INDEX);
  return status === FRAME_DIRTY;
};

const claimPresentWithSharedState = () => {
  if (!frameState) return false;
  const prev = Atomics.compareExchange(frameState, FRAME_STATUS_INDEX, FRAME_DIRTY, FRAME_PRESENTING);
  return prev === FRAME_DIRTY;
};

const finishPresentWithSharedState = () => {
  if (!frameState) return;
  Atomics.compareExchange(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTING, FRAME_PRESENTED);
  Atomics.notify(frameState, FRAME_STATUS_INDEX);
};

const computeDroppedFromSeqForPresent = () => {
  if (!frameState) return;
  if (frameState.length <= FRAME_SEQ_INDEX) return;

  const seq = Atomics.load(frameState, FRAME_SEQ_INDEX);
  const dropped = Math.max(0, seq - lastPresentedSeq - 1);
  framesDropped += dropped;
  lastPresentedSeq = seq;
};

const presentOnce = async () => {
  if (!presentFn) return false;
  const dirtyRects = pendingDirtyRects;
  pendingDirtyRects = null;
  const t0 = performance.now();
  const result = await presentFn(dirtyRects);
  telemetry.recordPresentLatencyMs(performance.now() - t0);
  return typeof result === 'boolean' ? result : true;
};

const handleTick = async () => {
  tryInitSharedFramebufferViews();
  maybeUpdateFramesReceivedFromSeq();

  if (!presentFn) {
    maybePostMetrics();
    return;
  }

  if (presenting) {
    maybePostMetrics();
    return;
  }

  if (frameState) {
    if (!shouldPresentWithSharedState()) {
      maybePostMetrics();
      return;
    }

    if (!claimPresentWithSharedState()) {
      maybePostMetrics();
      return;
    }

    computeDroppedFromSeqForPresent();
  } else {
    if (pendingFrames === 0) {
      maybePostMetrics();
      return;
    }

    if (pendingFrames > 1) {
      framesDropped += pendingFrames - 1;
    }

    pendingFrames = 0;
  }

  presenting = true;
  try {
    const didPresent = await presentOnce();
    if (didPresent) {
      framesPresented += 1;

      const now = performance.now();
      if (lastFrameStartMs !== null) {
        telemetry.beginFrame(lastFrameStartMs);
        telemetry.endFrame(now);
      }
      lastFrameStartMs = now;
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    postToMain({ type: 'error', message });
  } finally {
    presenting = false;
    finishPresentWithSharedState();
    maybePostMetrics();
  }
};

self.onmessage = (event: MessageEvent<GpuWorkerMessageFromMain>) => {
  const msg = event.data;
  if (!msg || typeof msg !== 'object' || !('type' in msg)) return;

  switch (msg.type) {
    case 'init': {
      lastInitMessage = msg;
      if (msg.sharedFrameState) {
        frameState = new Int32Array(msg.sharedFrameState);
      }

      // Optional: zero-copy framebuffer region for presenter modules.
      // When present, the worker exposes `globalThis.__aeroSharedFramebuffer`
      // (see `tryInitSharedFramebufferViews`).
      tryInitSharedFramebufferViews();

      if (msg.wasmModuleUrl) {
        loadPresentFnFromModuleUrl(msg.wasmModuleUrl).catch((err) => {
          const message = err instanceof Error ? err.message : String(err);
          postToMain({ type: 'error', message });
        });
      }

      telemetry.reset();
      lastFrameStartMs = null;
      break;
    }

    case 'frame_dirty': {
      pendingDirtyRects = msg.dirtyRects ?? null;
      if (!frameState) {
        pendingFrames += 1;
        framesReceived += 1;
      }
      break;
    }

    case 'request_timings': {
      void (async () => {
        try {
          const timings = getTimingsFn ? await getTimingsFn() : latestTimings;
          latestTimings = timings;
          postToMain({ type: 'timings', timings });
        } catch (err) {
          const message = err instanceof Error ? err.message : String(err);
          postToMain({ type: 'error', message });
        }
      })();
      break;
    }

    case 'tick': {
      void msg.frameTimeMs;
      void handleTick();
      break;
    }
  }
};
