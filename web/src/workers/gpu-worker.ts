/// <reference lib="webworker" />

// This worker serves two roles:
// 1) A frame-protocol driven scheduler that calls an externally-provided `present()` function
//    (typically from a wasm module) and reports presentation metrics.
// 2) A lightweight "presenter worker" harness used by Playwright to validate
//    WebGPU/WebGL2 presenter backends (including the raw WebGL2 fallback).
//
// Both protocols use `type: "init"`; we disambiguate by checking for an OffscreenCanvas
// `canvas` field.

import { perf } from '../perf/perf';
import { installWorkerPerfHandlers } from '../perf/worker';
import { PerfWriter } from '../perf/writer.js';
import { PERF_FRAME_HEADER_FRAME_ID_INDEX } from '../perf/shared.js';

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
import type { AeroConfig } from '../config/aero_config';
import type { WorkerRole } from '../runtime/shared_layout';
import { createSharedMemoryViews, setReadyFlag } from '../runtime/shared_layout';
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from '../runtime/protocol';

import type { Presenter, PresenterBackendKind, PresenterInitOptions } from '../gpu/presenter';
import { PresenterError } from '../gpu/presenter';
import { RawWebGl2Presenter } from '../gpu/raw-webgl2-presenter-backend';
import type {
  GpuWorkerInMessage as PresenterWorkerInMessage,
  GpuWorkerOutMessage as PresenterWorkerOutMessage,
} from './gpu-worker-protocol';

type PresentFn = (dirtyRects?: DirtyRect[] | null) => void | boolean | Promise<void | boolean>;
type GetTimingsFn = () => FrameTimingsReport | null | Promise<FrameTimingsReport | null>;

type AnyInboundMessage = GpuWorkerMessageFromMain | PresenterWorkerInMessage;

const ctx = self as unknown as DedicatedWorkerGlobalScope;
void installWorkerPerfHandlers();

const postToMain = (msg: GpuWorkerMessageToMain) => {
  ctx.postMessage(msg);
};

const postRuntimeError = (message: string) => {
  if (!status) return;
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
};

let role: WorkerRole = 'gpu';
let status: Int32Array | null = null;

let frameState: Int32Array | null = null;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfCurrentFrameId = 0;
let perfGpuMs = 0;
let perfUploadBytes = 0;

// NOTE: `present()` is expected to be provided by the GPU wasm module once the rendering stack
// is fully wired up. Until then, we keep a tiny no-op implementation so the frame pacing demo
// can run end-to-end without keeping the main thread stuck in DIRTY→tick spam.
let presentFn: PresentFn | null = () => true;
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
let lastInitMessage: Extract<GpuWorkerMessageFromMain, { type: 'init' }> | null = null;

const telemetry = new GpuTelemetry({ frameBudgetMs: Number.POSITIVE_INFINITY });
let lastFrameStartMs: number | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

const flushPerfFrameSample = () => {
  if (!perfWriter) return;
  if (perfCurrentFrameId === 0) return;

  perfWriter.frameSample(perfCurrentFrameId, {
    durations: { gpu_ms: perfGpuMs > 0 ? perfGpuMs : 0.01 },
  });
  if (perfUploadBytes > 0) {
    perfWriter.graphicsSample(perfCurrentFrameId, {
      counters: { upload_bytes: perfUploadBytes },
    });
  }

  perfGpuMs = 0;
  perfUploadBytes = 0;
};

const syncPerfFrame = () => {
  if (!perfWriter || !perfFrameHeader) return;
  const frameId = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (frameId === 0) return;

  if (perfCurrentFrameId === 0) {
    perfCurrentFrameId = frameId;
    return;
  }

  if (frameId !== perfCurrentFrameId) {
    flushPerfFrameSample();
    perfCurrentFrameId = frameId;
  }
};

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

    // Expose on the worker global so the dynamically-imported presenter module can read the
    // framebuffer without plumbing arguments through `present()`.
    (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer =
      framebufferViews;
  } catch {
    // Header likely not initialized yet; caller should retry later.
  }
};

const BYTES_PER_PIXEL_RGBA8 = 4;
const COPY_BYTES_PER_ROW_ALIGNMENT = 256;

const alignUp = (value: number, align: number): number => {
  if (align <= 0) return value;
  return Math.ceil(value / align) * align;
};

const bytesPerRowForUpload = (rowBytes: number, copyHeight: number): number => {
  if (copyHeight <= 1) return rowBytes;
  return alignUp(rowBytes, COPY_BYTES_PER_ROW_ALIGNMENT);
};

const requiredDataLen = (bytesPerRow: number, rowBytes: number, copyHeight: number): number => {
  if (copyHeight <= 0) return 0;
  return bytesPerRow * (copyHeight - 1) + rowBytes;
};

const clampInt = (value: number, min: number, max: number): number =>
  Math.max(min, Math.min(max, Math.trunc(value)));

const estimateTextureUploadBytes = (
  layout: SharedFramebufferLayout | null,
  dirtyRects: DirtyRect[] | null,
): number => {
  if (!layout) return 0;

  const fullRect: DirtyRect = { x: 0, y: 0, w: layout.width, h: layout.height };
  const rects =
    dirtyRects == null ? [fullRect] : dirtyRects.length === 0 ? ([] as DirtyRect[]) : dirtyRects;

  let total = 0;
  for (const rect of rects) {
    const x = clampInt(rect.x, 0, layout.width);
    const y = clampInt(rect.y, 0, layout.height);
    const w = clampInt(rect.w, 0, layout.width - x);
    const h = clampInt(rect.h, 0, layout.height - y);
    if (w === 0 || h === 0) continue;

    const rowBytes = w * BYTES_PER_PIXEL_RGBA8;
    const bytesPerRow = bytesPerRowForUpload(rowBytes, h);
    total += requiredDataLen(bytesPerRow, rowBytes, h);
  }

  return total;
};

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
  perf.counter("framesReceived", framesReceived);
  perf.counter("framesPresented", framesPresented);
  perf.counter("framesDropped", framesDropped);
  postToMain({
    type: 'metrics',
    framesReceived,
    framesPresented,
    framesDropped,
    telemetry: telemetry.snapshot(),
  });
};

const sendError = (err: unknown) => {
  const message = err instanceof Error ? err.message : String(err);
  postToMain({ type: 'error', message });
  postRuntimeError(message);
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
  const st = Atomics.load(frameState, FRAME_STATUS_INDEX);
  return st === FRAME_DIRTY;
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

const presentOnce = async (dirtyRects: DirtyRect[] | null) => {
  if (!presentFn) return false;
  const t0 = performance.now();
  const result = await presentFn(dirtyRects);
  telemetry.recordPresentLatencyMs(performance.now() - t0);
  return typeof result === 'boolean' ? result : true;
};

const handleTick = async () => {
  syncPerfFrame();
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

    if (pendingFrames > 1) framesDropped += pendingFrames - 1;
    pendingFrames = 0;
  }

  const dirtyRects = pendingDirtyRects;
  pendingDirtyRects = null;

  presenting = true;
  try {
    const presentStartMs = performance.now();
    const didPresent = await presentOnce(dirtyRects);
    perfGpuMs += performance.now() - presentStartMs;
    if (didPresent) {
      framesPresented += 1;

      const now = performance.now();
      if (lastFrameStartMs !== null) {
        telemetry.beginFrame(lastFrameStartMs);
        const textureUploadBytes = estimateTextureUploadBytes(framebufferViews?.layout ?? null, dirtyRects);
        telemetry.recordTextureUploadBytes(textureUploadBytes);
        perf.counter("textureUploadBytes", textureUploadBytes);
        perfUploadBytes += textureUploadBytes;
        telemetry.endFrame(now);
      }
      lastFrameStartMs = now;
    }
  } catch (err) {
    sendError(err);
  } finally {
    presenting = false;
    finishPresentWithSharedState();
    maybePostMetrics();
  }
};

// -----------------------------------------------------------------------------
// Presenter-worker protocol (init/resize/present/screenshot)
// -----------------------------------------------------------------------------

let presenter: Presenter | null = null;

function postPresenterMessage(msg: PresenterWorkerOutMessage, transfer?: Transferable[]) {
  ctx.postMessage(msg, transfer ?? []);
}

function postPresenterError(err: unknown, backend?: PresenterBackendKind) {
  if (err instanceof PresenterError) {
    postPresenterMessage({ type: 'error', message: err.message, code: err.code, backend });
    return;
  }
  const msg = err instanceof Error ? err.message : String(err);
  postPresenterMessage({ type: 'error', message: msg, backend });
}

async function tryInitBackend(
  backend: PresenterBackendKind,
  canvas: OffscreenCanvas,
  width: number,
  height: number,
  dpr: number,
  opts?: PresenterInitOptions,
): Promise<Presenter> {
  const mergedOpts: PresenterInitOptions = {
    ...opts,
    onError: (e) => {
      postPresenterError(e, backend);
      opts?.onError?.(e);
    },
  };

  switch (backend) {
    case 'webgpu': {
      const mod = await import('../gpu/webgpu-presenter-backend');
      const p = new mod.WebGpuPresenterBackend();
      await p.init(canvas, width, height, dpr, mergedOpts);
      return p;
    }
    case 'webgl2_wgpu': {
      const mod = await import('../gpu/wgpu-webgl2-presenter');
      const p = new mod.WgpuWebGl2Presenter();
      await p.init(canvas, width, height, dpr, mergedOpts);
      return p;
    }
    case 'webgl2_raw': {
      const p = new RawWebGl2Presenter();
      p.init(canvas, width, height, dpr, mergedOpts);
      return p;
    }
    default: {
      const unreachable: never = backend;
      throw new PresenterError('unknown_backend', `Unknown backend ${unreachable}`);
    }
  }
}

async function initPresenter(
  canvas: OffscreenCanvas,
  width: number,
  height: number,
  dpr: number,
  opts?: PresenterInitOptions,
  forceBackend?: PresenterBackendKind,
): Promise<void> {
  presenter?.destroy?.();
  presenter = null;

  const backends: PresenterBackendKind[] = forceBackend ? [forceBackend] : ['webgpu', 'webgl2_wgpu', 'webgl2_raw'];

  let lastError: unknown = null;

  for (const backend of backends) {
    try {
      presenter = await tryInitBackend(backend, canvas, width, height, dpr, opts);
      postPresenterMessage({ type: 'inited', backend: presenter.backend });
      return;
    } catch (err) {
      lastError = err;
    }
  }

  postPresenterError(lastError ?? new PresenterError('no_backend', 'No GPU presenter backend could be initialized'));
}

const handleRuntimeInit = (init: WorkerInitMessage) => {
  role = init.role ?? 'gpu';
  const segments = { control: init.controlSab, guestMemory: init.guestMemory, vgaFramebuffer: init.vgaFramebuffer };
  status = createSharedMemoryViews(segments).status;
  setReadyFlag(status, role, true);

  if (init.frameStateSab) {
    frameState = new Int32Array(init.frameStateSab);
  }

  if (init.perfChannel) {
    perfWriter = new PerfWriter(init.perfChannel.buffer, {
      workerKind: init.perfChannel.workerKind,
      runStartEpochMs: init.perfChannel.runStartEpochMs,
    });
    perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
    perfCurrentFrameId = 0;
    perfGpuMs = 0;
    perfUploadBytes = 0;
  }

  ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
};

ctx.onmessage = (event: MessageEvent<unknown>) => {
  const data = event.data;

  if (data && typeof data === "object" && "kind" in data && (data as { kind?: unknown }).kind === "config.update") {
    const update = data as ConfigUpdateMessage;
    currentConfig = update.config;
    currentConfigVersion = update.version;
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  // Runtime/harness init (SharedArrayBuffers + worker role).
  if (data && typeof data === 'object' && 'kind' in data && (data as { kind?: unknown }).kind === 'init') {
    handleRuntimeInit(data as WorkerInitMessage);
    return;
  }

  const msg = data as Partial<AnyInboundMessage>;
  if (!msg || typeof msg !== 'object' || typeof msg.type !== 'string') return;

  switch (msg.type) {
    case 'init': {
      // Presenter-worker init includes an OffscreenCanvas.
      if (typeof (msg as { canvas?: unknown }).canvas !== 'undefined') {
        const presenterMsg = msg as PresenterWorkerInMessage;
        void initPresenter(
          presenterMsg.canvas,
          presenterMsg.width,
          presenterMsg.height,
          presenterMsg.dpr,
          presenterMsg.opts,
          presenterMsg.forceBackend,
        ).catch((err) => {
          postPresenterError(err, presenter?.backend);
        });
        return;
      }

      perf.spanBegin('worker:init');
      try {
        const frameMsg = msg as Extract<GpuWorkerMessageFromMain, { type: 'init' }>;
        lastInitMessage = frameMsg;
        if (frameMsg.sharedFrameState) {
          frameState = new Int32Array(frameMsg.sharedFrameState);
        }

        tryInitSharedFramebufferViews();

        if (frameMsg.wasmModuleUrl) {
          void perf.spanAsync('wasm:init', () => loadPresentFnFromModuleUrl(frameMsg.wasmModuleUrl)).catch(sendError);
        }

        telemetry.reset();
        lastFrameStartMs = null;
      } finally {
        perf.spanEnd('worker:init');
      }
      break;
    }

    case 'resize': {
      const presenterMsg = msg as PresenterWorkerInMessage;
      try {
        if (!presenter) throw new PresenterError('not_initialized', 'resize before init');
        presenter.resize(presenterMsg.width, presenterMsg.height, presenterMsg.dpr);
      } catch (err) {
        postPresenterError(err, presenter?.backend);
      }
      break;
    }

    case 'present': {
      const presenterMsg = msg as PresenterWorkerInMessage;
      try {
        if (!presenter) throw new PresenterError('not_initialized', 'present before init');
        presenter.present(presenterMsg.frame, presenterMsg.stride);
      } catch (err) {
        postPresenterError(err, presenter?.backend);
      }
      break;
    }

    case 'screenshot': {
      const presenterMsg = msg as PresenterWorkerInMessage;
      void (async () => {
        try {
          if (!presenter) throw new PresenterError('not_initialized', 'screenshot before init');
          const shot = await presenter.screenshot();
          postPresenterMessage(
            {
              type: 'screenshot',
              requestId: presenterMsg.requestId,
              width: shot.width,
              height: shot.height,
              pixels: shot.pixels,
            },
            [shot.pixels],
          );
        } catch (err) {
          postPresenterError(err, presenter?.backend);
        }
      })();
      break;
    }

    case 'frame_dirty': {
      const frameMsg = msg as Extract<GpuWorkerMessageFromMain, { type: 'frame_dirty' }>;
      pendingDirtyRects = frameMsg.dirtyRects ?? null;
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
          sendError(err);
        }
      })();
      break;
    }

    case 'tick': {
      void (msg as Extract<GpuWorkerMessageFromMain, { type: 'tick' }>).frameTimeMs;
      void handleTick();
      break;
    }
  }
};

void currentConfig;
