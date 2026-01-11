/// <reference lib="webworker" />

// Canonical GPU worker used by:
// - the runtime WorkerCoordinator (via `gpu.worker.ts`)
// - smoke tests (shared framebuffer presentation + screenshot readback)
//
// It consumes a SharedArrayBuffer-backed framebuffer and optionally presents it to an
// OffscreenCanvas using one of the presenter backends in `web/src/gpu/*`.
//
// NOTE: This worker also participates in the WorkerCoordinator control-plane protocol
// (`kind: "init"`, READY/ERROR messages) so it can be managed like other runtime workers.

import { perf } from '../perf/perf';
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from '../perf/shared.js';
import { installWorkerPerfHandlers } from '../perf/worker';
import { PerfWriter } from '../perf/writer.js';

import {
  FRAME_DIRTY,
  FRAME_METRICS_DROPPED_INDEX,
  FRAME_METRICS_PRESENTED_INDEX,
  FRAME_METRICS_RECEIVED_INDEX,
  FRAME_PRESENTED,
  FRAME_PRESENTING,
  FRAME_SEQ_INDEX,
  FRAME_STATUS_INDEX,
} from "../shared/frameProtocol";

import {
  dirtyTilesToRects,
  type DirtyRect,
  layoutFromHeader,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SHARED_FRAMEBUFFER_MAGIC,
  SHARED_FRAMEBUFFER_VERSION,
  SharedFramebufferHeaderIndex,
  type SharedFramebufferLayout,
} from "../ipc/shared-layout";

import {
  FRAMEBUFFER_FORMAT_RGBA8888,
  FRAMEBUFFER_MAGIC,
  FRAMEBUFFER_VERSION,
  HEADER_BYTE_LENGTH,
  HEADER_I32_COUNT,
  HEADER_INDEX_FRAME_COUNTER,
  HEADER_INDEX_FORMAT,
  HEADER_INDEX_HEIGHT,
  HEADER_INDEX_STRIDE_BYTES,
  HEADER_INDEX_WIDTH,
} from "../display/framebuffer_protocol";

import { GpuTelemetry } from '../gpu/telemetry.ts';
import type { AeroConfig } from '../config/aero_config';
import { createSharedMemoryViews, ringRegionsForWorker, setReadyFlag, StatusIndex, type WorkerRole } from '../runtime/shared_layout';
import { RingBuffer } from '../ipc/ring_buffer';
import { decodeCommand, encodeEvent, type Command, type Event } from '../ipc/protocol';
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";

import type { Presenter, PresenterBackendKind, PresenterInitOptions } from "../gpu/presenter";
import { PresenterError } from "../gpu/presenter";
import { RawWebGl2Presenter } from "../gpu/raw-webgl2-presenter-backend";
import type {
  GpuRuntimeErrorEvent,
  GpuRuntimeInMessage,
  GpuRuntimeFallbackInfo,
  GpuRuntimeCursorSetImageMessage,
  GpuRuntimeCursorSetStateMessage,
  GpuRuntimeEventsMessage,
  GpuRuntimeInitMessage,
  GpuRuntimeInitOptions,
  GpuRuntimeOutMessage,
  GpuRuntimeScreenshotRequestMessage,
  GpuRuntimeSubmitAerogpuMessage,
  GpuRuntimeStatsCountersV1,
  GpuRuntimeStatsMessage,
} from "./gpu_runtime_protocol";

type PresentFn = (dirtyRects?: DirtyRect[] | null) => void | boolean | Promise<void | boolean>;

const ctx = self as unknown as DedicatedWorkerGlobalScope;
void installWorkerPerfHandlers();

const postToMain = (msg: GpuRuntimeOutMessage, transfer?: Transferable[]) => {
  ctx.postMessage(msg, transfer ?? []);
};

const postRuntimeError = (message: string) => {
  if (!status) return;
  pushRuntimeEvent({ kind: 'log', level: 'error', message });
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
};

let role: WorkerRole = "gpu";
let status: Int32Array | null = null;

let frameState: Int32Array | null = null;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfCurrentFrameId = 0;
let perfGpuMs = 0;
let perfUploadBytes = 0;
let commandRing: RingBuffer | null = null;
let eventRing: RingBuffer | null = null;
let runtimePollTimer: number | null = null;

// Optional `present()` entrypoint supplied by a dynamically imported module.
// When unset, the worker uses the built-in presenter backends.
let presentFn: PresentFn | null = null;
let presentModule: Record<string, unknown> | null = null;
let wasmInitPromise: Promise<void> | null = null;
let presenting = false;

let runtimeInit: GpuRuntimeInitMessage | null = null;
let runtimeCanvas: OffscreenCanvas | null = null;
let runtimeOptions: GpuRuntimeInitOptions | null = null;
let runtimeReadySent = false;

let telemetryPollTimer: number | null = null;
const TELEMETRY_POLL_INTERVAL_MS = 1000 / 3;
let telemetryTickInFlight = false;

let isDeviceLost = false;
let recoveryPromise: Promise<void> | null = null;

let presentsAttempted = 0;
let presentsSucceeded = 0;
let recoveriesAttempted = 0;
let recoveriesSucceeded = 0;
let surfaceReconfigures = 0;

let canvasWithContextLossHandlers: OffscreenCanvas | null = null;
let onWebglContextLost: ((ev: Event) => void) | null = null;
let onWebglContextRestored: ((ev: Event) => void) | null = null;

let outputWidthCss: number | null = null;
let outputHeightCss: number | null = null;
let outputDpr = 1;

let presenter: Presenter | null = null;
let presenterInitOptions: PresenterInitOptions | null = null;
let presenterUserOnError: ((error: PresenterError) => void) | undefined = undefined;
let presenterFallback: GpuRuntimeFallbackInfo | undefined = undefined;
let presenterInitPromise: Promise<void> | null = null;
let presenterErrorGeneration = 0;
let presenterSrcWidth = 0;
let presenterSrcHeight = 0;

// -----------------------------------------------------------------------------
// AeroGPU command submission (ACMD)
// -----------------------------------------------------------------------------

type AeroGpuCpuTexture = {
  width: number;
  height: number;
  format: number;
  data: Uint8Array;
};

const aerogpuTextures = new Map<number, AeroGpuCpuTexture>();
let aerogpuCurrentRenderTarget: number | null = null;
let aerogpuPresentCount: bigint = 0n;
let aerogpuLastPresentedFrame: { width: number; height: number; rgba8: ArrayBuffer } | null = null;

// Ensure submissions execute serially even though message handlers are async.
let aerogpuSubmitChain: Promise<void> = Promise.resolve();

let framesReceived = 0;
let framesPresented = 0;
let framesDropped = 0;

let lastSeenSeq = 0;
let lastPresentedSeq = 0;
let lastUploadDirtyRects: DirtyRect[] | null = null;

let lastMetricsPostAtMs = 0;
const METRICS_POST_INTERVAL_MS = 250;

type SharedFramebufferViews = {
  header: Int32Array;
  layout: SharedFramebufferLayout;
  slot0: Uint8Array;
  slot1: Uint8Array;
  dirty0: Uint32Array | null;
  dirty1: Uint32Array | null;
};

let sharedFramebufferViews: SharedFramebufferViews | null = null;
let sharedFramebufferLayoutKey: string | null = null;

type FramebufferProtocolViews = {
  header: Int32Array;
  width: number;
  height: number;
  strideBytes: number;
  pixels: Uint8Array;
};

let framebufferProtocolViews: FramebufferProtocolViews | null = null;
let framebufferProtocolLayoutKey: string | null = null;

type CursorPresenter = Presenter & {
  setCursorImageRgba8?: (rgba: Uint8Array, width: number, height: number) => void;
  setCursorState?: (enabled: boolean, x: number, y: number, hotX: number, hotY: number) => void;
  setCursorRenderEnabled?: (enabled: boolean) => void;
  redraw?: () => void;
};

let cursorImage: Uint8Array | null = null;
let cursorWidth = 0;
let cursorHeight = 0;
let cursorEnabled = false;
let cursorX = 0;
let cursorY = 0;
let cursorHotX = 0;
let cursorHotY = 0;

// Normally true; temporarily disabled for cursor-less screenshots.
let cursorRenderEnabled = true;

const getCursorPresenter = (): CursorPresenter | null => presenter as unknown as CursorPresenter | null;

const syncCursorToPresenter = (): void => {
  const p = getCursorPresenter();
  if (!p) return;

  if (p.setCursorRenderEnabled) {
    p.setCursorRenderEnabled(cursorRenderEnabled);
  }

  if (cursorImage && cursorWidth > 0 && cursorHeight > 0 && p.setCursorImageRgba8) {
    p.setCursorImageRgba8(cursorImage, cursorWidth, cursorHeight);
  }

  if (p.setCursorState) {
    p.setCursorState(cursorEnabled, cursorX, cursorY, cursorHotX, cursorHotY);
  }
};

const redrawCursor = (): void => {
  const p = getCursorPresenter();
  if (!p) return;
  if (p.redraw) {
    p.redraw();
    return;
  }

  // Best-effort fallback: re-present the current framebuffer if no redraw primitive exists.
  const frame = getCurrentFrameInfo();
  if (!frame || !presenter) return;
  presenter.present(frame.pixels, frame.strideBytes);
};

const compositeCursorOverRgba8 = (
  dst: Uint8Array,
  dstWidth: number,
  dstHeight: number,
  enabled: boolean,
  cursorRgba: Uint8Array | null,
  cursorW: number,
  cursorH: number,
  cursorX: number,
  cursorY: number,
  hotX: number,
  hotY: number,
): void => {
  if (!enabled) return;
  if (!cursorRgba) return;
  if (cursorW <= 0 || cursorH <= 0) return;
  if (dstWidth <= 0 || dstHeight <= 0) return;

  const requiredCursorLen = cursorW * cursorH * 4;
  if (cursorRgba.byteLength < requiredCursorLen) return;

  const originX = cursorX - hotX;
  const originY = cursorY - hotY;

  for (let cy = 0; cy < cursorH; cy += 1) {
    const dy = originY + cy;
    if (dy < 0 || dy >= dstHeight) continue;
    for (let cx = 0; cx < cursorW; cx += 1) {
      const dx = originX + cx;
      if (dx < 0 || dx >= dstWidth) continue;

      const srcOff = (cy * cursorW + cx) * 4;
      const a = cursorRgba[srcOff + 3]!;
      if (a === 0) continue;

      const dstOff = (dy * dstWidth + dx) * 4;
      if (a === 255) {
        dst[dstOff + 0] = cursorRgba[srcOff + 0]!;
        dst[dstOff + 1] = cursorRgba[srcOff + 1]!;
        dst[dstOff + 2] = cursorRgba[srcOff + 2]!;
        dst[dstOff + 3] = 255;
        continue;
      }

      const invA = 255 - a;
      for (let ch = 0; ch < 3; ch += 1) {
        const src = cursorRgba[srcOff + ch]!;
        const dstCh = dst[dstOff + ch]!;
        dst[dstOff + ch] = Math.floor((src * a + dstCh * invA + 127) / 255);
      }
      dst[dstOff + 3] = 255;
    }
  }
};

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
  const enabled = Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
  if (!enabled) {
    perfCurrentFrameId = 0;
    perfGpuMs = 0;
    perfUploadBytes = 0;
    return;
  }
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

const refreshSharedFramebufferViews = (shared: SharedArrayBuffer, offsetBytes: number): void => {
  const header = new Int32Array(shared, offsetBytes, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
  const magic = Atomics.load(header, SharedFramebufferHeaderIndex.MAGIC);
  const version = Atomics.load(header, SharedFramebufferHeaderIndex.VERSION);
  if (magic !== SHARED_FRAMEBUFFER_MAGIC || version !== SHARED_FRAMEBUFFER_VERSION) return;

  try {
    const layout = layoutFromHeader(header);
    const layoutKey = `${layout.width},${layout.height},${layout.strideBytes},${layout.tileSize},${layout.dirtyWordsPerBuffer}`;
    if (sharedFramebufferViews && sharedFramebufferLayoutKey === layoutKey) return;

    const slot0 = new Uint8Array(shared, offsetBytes + layout.framebufferOffsets[0], layout.strideBytes * layout.height);
    const slot1 = new Uint8Array(shared, offsetBytes + layout.framebufferOffsets[1], layout.strideBytes * layout.height);

    const dirty0 =
      layout.dirtyWordsPerBuffer === 0
        ? null
        : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[0], layout.dirtyWordsPerBuffer);
    const dirty1 =
      layout.dirtyWordsPerBuffer === 0
        ? null
        : new Uint32Array(shared, offsetBytes + layout.dirtyOffsets[1], layout.dirtyWordsPerBuffer);

    sharedFramebufferViews = { header, layout, slot0, slot1, dirty0, dirty1 };
    sharedFramebufferLayoutKey = layoutKey;

    framebufferProtocolViews = null;
    framebufferProtocolLayoutKey = null;

    // Expose on the worker global so a dynamically imported present() module can
    // read the framebuffer without plumbing arguments through postMessage.
    (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer =
      sharedFramebufferViews;
  } catch {
    // Header likely not initialized yet; caller should retry later.
  }
};

const refreshFramebufferProtocolViews = (shared: SharedArrayBuffer, offsetBytes: number): void => {
  const header = new Int32Array(shared, offsetBytes, HEADER_I32_COUNT);
  const magic = Atomics.load(header, 0);
  const version = Atomics.load(header, 1);
  if (magic !== FRAMEBUFFER_MAGIC || version !== FRAMEBUFFER_VERSION) return;

  const width = Atomics.load(header, HEADER_INDEX_WIDTH);
  const height = Atomics.load(header, HEADER_INDEX_HEIGHT);
  const strideBytes = Atomics.load(header, HEADER_INDEX_STRIDE_BYTES);
  const format = Atomics.load(header, HEADER_INDEX_FORMAT);

  // Not yet initialized (or unsupported mode).
  if (width <= 0 || height <= 0 || strideBytes <= 0) return;
  if (format !== FRAMEBUFFER_FORMAT_RGBA8888) return;

  const requiredBytes = HEADER_BYTE_LENGTH + strideBytes * height;
  if (offsetBytes + requiredBytes > shared.byteLength) return;

  const layoutKey = `${width},${height},${strideBytes}`;
  if (framebufferProtocolViews && framebufferProtocolLayoutKey === layoutKey) return;

  framebufferProtocolViews = {
    header,
    width,
    height,
    strideBytes,
    pixels: new Uint8Array(shared, offsetBytes + HEADER_BYTE_LENGTH, strideBytes * height),
  };
  framebufferProtocolLayoutKey = layoutKey;

  sharedFramebufferViews = null;
  sharedFramebufferLayoutKey = null;
  (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer = undefined;
};

const refreshFramebufferViews = (): void => {
  const init = runtimeInit;
  if (!init) return;

  const shared = init.sharedFramebuffer;
  const offsetBytes = init.sharedFramebufferOffsetBytes ?? 0;
  if (offsetBytes < 0 || offsetBytes + 8 > shared.byteLength) return;

  // Detect the framebuffer protocol based on (magic, version).
  const header2 = new Int32Array(shared, offsetBytes, 2);
  const magic = Atomics.load(header2, 0);
  const version = Atomics.load(header2, 1);

  if (magic === SHARED_FRAMEBUFFER_MAGIC && version === SHARED_FRAMEBUFFER_VERSION) {
    refreshSharedFramebufferViews(shared, offsetBytes);
    return;
  }

  if (magic === FRAMEBUFFER_MAGIC && version === FRAMEBUFFER_VERSION) {
    refreshFramebufferProtocolViews(shared, offsetBytes);
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

function backendKindForEvent(): string {
  if (presenter) return presenter.backend;
  if (runtimeCanvas) return "unknown";
  return "headless";
}

function postGpuEvents(events: GpuRuntimeErrorEvent[]): void {
  if (events.length === 0) return;
  postToMain({ type: "events", version: 1, events } satisfies GpuRuntimeEventsMessage);
}

function emitGpuEvent(event: GpuRuntimeErrorEvent): void {
  postGpuEvents([event]);
}

function normalizeSeverity(value: unknown): GpuRuntimeErrorEvent["severity"] {
  switch (typeof value === "string" ? value.toLowerCase() : "") {
    case "info":
      return "info";
    case "warn":
    case "warning":
      return "warn";
    case "error":
      return "error";
    case "fatal":
      return "fatal";
    default:
      return "error";
  }
}

function normalizeGpuEvent(raw: unknown): GpuRuntimeErrorEvent | null {
  const now = performance.now();
  const defaultBackend = backendKindForEvent();

  const parsed = typeof raw === "string" ? (() => { try { return JSON.parse(raw); } catch { return raw; } })() : raw;
  if (parsed == null) return null;

  if (typeof parsed !== "object") {
    return {
      time_ms: now,
      backend_kind: defaultBackend,
      severity: "error",
      category: "Unknown",
      message: String(parsed),
    };
  }

  const obj = parsed as Record<string, unknown>;
  const timeVal = obj.time_ms ?? obj.timeMs ?? obj.time ?? obj.ts_ms ?? obj.ts;
  const time_ms = typeof timeVal === "number" ? timeVal : now;

  const backendVal = obj.backend_kind ?? obj.backendKind ?? obj.backend;
  const backend_kind = typeof backendVal === "string" ? backendVal : defaultBackend;

  const messageVal = obj.message ?? obj.msg ?? obj.error ?? obj.text;
  const message = typeof messageVal === "string" ? messageVal : String(messageVal ?? "gpu event");

  const categoryVal = obj.category ?? obj.cat;
  const category = typeof categoryVal === "string" ? categoryVal : "Unknown";

  const severityVal = obj.severity ?? obj.level ?? obj.sev;
  const severity = normalizeSeverity(severityVal);

  const details = "details" in obj ? obj.details : "data" in obj ? obj.data : undefined;
  return {
    time_ms,
    backend_kind,
    severity,
    category,
    message,
    ...(details === undefined ? {} : { details }),
  };
}

function normalizeGpuEventBatch(raw: unknown): GpuRuntimeErrorEvent[] {
  const parsed = typeof raw === "string" ? (() => { try { return JSON.parse(raw); } catch { return raw; } })() : raw;
  if (parsed == null) return [];

  let items: unknown[] = [];
  if (Array.isArray(parsed)) {
    items = parsed;
  } else if (typeof parsed === "object") {
    const obj = parsed as Record<string, unknown>;
    const events = obj.events ?? obj.error_events ?? obj.gpu_events;
    if (Array.isArray(events)) {
      items = events;
    } else {
      items = [parsed];
    }
  } else {
    items = [parsed];
  }

  const out: GpuRuntimeErrorEvent[] = [];
  for (const item of items) {
    const ev = normalizeGpuEvent(item);
    if (ev) out.push(ev);
  }
  return out;
}

function getStatsCounters(): GpuRuntimeStatsCountersV1 {
  return {
    presents_attempted: presentsAttempted,
    presents_succeeded: presentsSucceeded,
    recoveries_attempted: recoveriesAttempted,
    recoveries_succeeded: recoveriesSucceeded,
    surface_reconfigures: surfaceReconfigures,
  };
}

function postStatsMessage(wasmStats?: unknown): void {
  const backendKind = presenter?.backend ?? (runtimeCanvas ? undefined : "headless");
  postToMain({
    type: "stats",
    version: 1,
    timeMs: performance.now(),
    ...(backendKind ? { backendKind } : {}),
    counters: getStatsCounters(),
    ...(wasmStats === undefined ? {} : { wasm: wasmStats }),
  } satisfies GpuRuntimeStatsMessage);
}

function getModuleExportFn<T extends (...args: any[]) => any>(names: readonly string[]): T | null {
  const mod = presentModule as Record<string, unknown> | null;
  if (!mod) return null;
  for (const name of names) {
    const fn = mod[name];
    if (typeof fn === "function") return fn as T;
  }
  return null;
}

async function tryGetWasmStats(): Promise<unknown | undefined> {
  const fn = getModuleExportFn<() => unknown | Promise<unknown>>(["get_gpu_stats", "getGpuStats"]);
  if (!fn) return undefined;
  try {
    const value = await fn();
    if (typeof value === "string") {
      try {
        return JSON.parse(value);
      } catch {
        return value;
      }
    }
    return value;
  } catch {
    return undefined;
  }
}

async function tryDrainWasmEvents(): Promise<GpuRuntimeErrorEvent[]> {
  const fn = getModuleExportFn<() => unknown | Promise<unknown>>([
    "drain_gpu_events",
    "drain_gpu_error_events",
    "take_gpu_events",
    "take_gpu_error_events",
    "drainGpuEvents",
  ]);
  if (!fn) return [];
  try {
    const value = await fn();
    return normalizeGpuEventBatch(value);
  } catch {
    return [];
  }
}

async function telemetryTick(): Promise<void> {
  if (telemetryTickInFlight) return;
  if (!runtimeInit) return;
  if (isDeviceLost) return;

  telemetryTickInFlight = true;
  try {
    const events = await tryDrainWasmEvents();
    if (events.length > 0) {
      postGpuEvents(events);
      // Infer device loss from runtime-reported events.
      for (const ev of events) {
        if (isDeviceLost) break;
        if (ev.category.toLowerCase() === "devicelost" && (ev.severity === "error" || ev.severity === "fatal")) {
          handleDeviceLost(ev.message, { source: "wasm", event: ev }, true);
          break;
        }
      }
    }

    if (isDeviceLost) return;

    const wasmStats = await tryGetWasmStats();
    postStatsMessage(wasmStats);
  } finally {
    telemetryTickInFlight = false;
  }
}

function startTelemetryPolling(): void {
  if (telemetryPollTimer !== null) return;
  telemetryPollTimer = setInterval(() => void telemetryTick(), TELEMETRY_POLL_INTERVAL_MS) as unknown as number;
  void telemetryTick();
}

function stopTelemetryPolling(): void {
  if (telemetryPollTimer === null) return;
  clearInterval(telemetryPollTimer);
  telemetryPollTimer = null;
}

function installContextLossHandlers(canvas: OffscreenCanvas): void {
  if (canvasWithContextLossHandlers === canvas) return;
  uninstallContextLossHandlers();

  canvasWithContextLossHandlers = canvas;
  onWebglContextLost = (ev: Event) => {
    // Allow restoration when supported.
    (ev as any).preventDefault?.();
    handleDeviceLost("WebGL context lost", { source: "webglcontextlost" }, false);
  };
  onWebglContextRestored = () => {
    if (!isDeviceLost) return;
    void attemptRecovery("webglcontextrestored");
  };

  try {
    (canvas as any).addEventListener("webglcontextlost", onWebglContextLost, { passive: false } as any);
    (canvas as any).addEventListener("webglcontextrestored", onWebglContextRestored);
  } catch {
    // Best-effort: some OffscreenCanvas implementations do not expose these events.
  }
}

function uninstallContextLossHandlers(): void {
  const canvas = canvasWithContextLossHandlers;
  if (!canvas) return;
  try {
    if (onWebglContextLost) (canvas as any).removeEventListener("webglcontextlost", onWebglContextLost);
    if (onWebglContextRestored) (canvas as any).removeEventListener("webglcontextrestored", onWebglContextRestored);
  } catch {
    // Ignore.
  }
  canvasWithContextLossHandlers = null;
  onWebglContextLost = null;
  onWebglContextRestored = null;
}

function getDeviceLostCode(
  err: unknown,
): "webgl_context_lost" | "webgl_context_restore_failed" | "webgpu_device_lost" | null {
  if (!(err instanceof PresenterError)) return null;
  switch (err.code) {
    case "webgl_context_lost":
    case "webgl_context_restore_failed":
    case "webgpu_device_lost":
      return err.code;
    default:
      return null;
  }
}

function handleDeviceLost(message: string, details?: unknown, startRecovery?: boolean): void {
  if (isDeviceLost) return;
  if (!runtimeInit) return;

  isDeviceLost = true;
  runtimeReadySent = false;
  stopTelemetryPolling();

  const backend = backendKindForEvent();
  emitGpuEvent({
    time_ms: performance.now(),
    backend_kind: backend,
    severity: "error",
    category: "DeviceLost",
    message,
    ...(details === undefined ? {} : { details }),
  });

  presenter?.destroy?.();
  presenter = null;
  presenterFallback = undefined;
  presenterSrcWidth = 0;
  presenterSrcHeight = 0;

  if (startRecovery) {
    void attemptRecovery("device_lost");
  }
}

async function attemptRecovery(reason: string): Promise<void> {
  if (!runtimeInit) return;
  if (recoveryPromise) return recoveryPromise;

  recoveriesAttempted += 1;
  emitGpuEvent({
    time_ms: performance.now(),
    backend_kind: backendKindForEvent(),
    severity: "info",
    category: "DeviceLost",
    message: `Attempting GPU recovery (${reason})`,
  });

  recoveryPromise = (async () => {
    if (presenterInitPromise) {
      try {
        await presenterInitPromise;
      } catch {
        // Ignore; init failure is reported through the existing error channel.
      }
    }

    if (wasmInitPromise) {
      try {
        await wasmInitPromise;
      } catch {
        // Ignore; wasm init failure is reported through the existing error channel.
      }
    }

    // Re-init present() module if configured.
    if (runtimeOptions?.wasmModuleUrl) {
      presentFn = null;
      presentModule = null;
      await loadPresentFnFromModuleUrl(runtimeOptions.wasmModuleUrl);
    }

    // Re-init presenter backend (if we are using the built-in presenter path).
    if (runtimeCanvas && !presentFn) {
      const frame = getCurrentFrameInfo();
      if (!frame) {
        throw new PresenterError("not_initialized", "GPU recovery requested before framebuffer init");
      }
      await initPresenterForRuntime(runtimeCanvas, frame.width, frame.height);
    }

    isDeviceLost = false;
    recoveriesSucceeded += 1;
    startTelemetryPolling();

    // Re-emit READY for consumers that treat recovery like a re-init.
    await maybeSendReady();

    emitGpuEvent({
      time_ms: performance.now(),
      backend_kind: backendKindForEvent(),
      severity: "info",
      category: "DeviceLost",
      message: "GPU recovery succeeded",
    });
  })()
    .catch((err) => {
      emitGpuEvent({
        time_ms: performance.now(),
        backend_kind: backendKindForEvent(),
        severity: "fatal",
        category: "DeviceLost",
        message: "GPU recovery failed",
        details: err instanceof Error ? { message: err.message, stack: err.stack } : String(err),
      });
      postFatalError(err);
    })
    .finally(() => {
      recoveryPromise = null;
    });

  return recoveryPromise;
}

function postFatalError(err: unknown): void {
  if (err instanceof PresenterError) {
    postToMain({ type: "error", message: err.message, code: err.code, backend: presenter?.backend });
    postRuntimeError(err.message);
    return;
  }

  const message = err instanceof Error ? err.message : String(err);
  postToMain({ type: "error", message, backend: presenter?.backend });
  postRuntimeError(message);
}

const sendError = (err: unknown) => {
  const deviceLostCode = getDeviceLostCode(err);
  if (deviceLostCode) {
    const startRecovery = deviceLostCode !== "webgl_context_lost";
    handleDeviceLost(
      err instanceof Error ? err.message : String(err),
      { source: "exception", code: deviceLostCode, error: err },
      startRecovery,
    );
    return;
  }
  postFatalError(err);
};

async function loadPresentFnFromModuleUrl(wasmModuleUrl: string): Promise<void> {
  const mod: unknown = await import(/* @vite-ignore */ wasmModuleUrl);
  presentModule = mod as Record<string, unknown>;

  const maybePresent = (presentModule as { present?: unknown } | null)?.present;
  if (typeof maybePresent !== "function") {
    throw new Error(`Module ${wasmModuleUrl} did not export a present() function`);
  }
  presentFn = maybePresent as PresentFn;
}

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

type CurrentFrameInfo = {
  width: number;
  height: number;
  strideBytes: number;
  pixels: Uint8Array;
  frameSeq: number;
  sharedLayout?: SharedFramebufferLayout;
  dirtyRects?: DirtyRect[] | null;
};

const getCurrentFrameInfo = (): CurrentFrameInfo | null => {
  refreshFramebufferViews();

  if (sharedFramebufferViews) {
    const active = Atomics.load(sharedFramebufferViews.header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
    const pixels = active === 0 ? sharedFramebufferViews.slot0 : sharedFramebufferViews.slot1;
    const dirtyWords = active === 0 ? sharedFramebufferViews.dirty0 : sharedFramebufferViews.dirty1;
    let dirtyRects: DirtyRect[] | null = null;
    if (dirtyWords) {
      // Mirror the Rust `FrameSource` behavior: if dirty tracking is enabled but
      // the producer does not set any bits, treat the frame as full-frame dirty.
      // (This avoids interpreting `[]` as "nothing changed".)
      let anyDirty = false;
      for (let i = 0; i < dirtyWords.length; i += 1) {
        if (dirtyWords[i] !== 0) {
          anyDirty = true;
          break;
        }
      }
      dirtyRects = anyDirty ? dirtyTilesToRects(sharedFramebufferViews.layout, dirtyWords) : null;
    }
    const frameSeq = Atomics.load(sharedFramebufferViews.header, SharedFramebufferHeaderIndex.FRAME_SEQ);
    return {
      width: sharedFramebufferViews.layout.width,
      height: sharedFramebufferViews.layout.height,
      strideBytes: sharedFramebufferViews.layout.strideBytes,
      pixels,
      frameSeq,
      sharedLayout: sharedFramebufferViews.layout,
      dirtyRects,
    };
  }

  if (framebufferProtocolViews) {
    const frameSeq = Atomics.load(framebufferProtocolViews.header, HEADER_INDEX_FRAME_COUNTER);
    return {
      width: framebufferProtocolViews.width,
      height: framebufferProtocolViews.height,
      strideBytes: framebufferProtocolViews.strideBytes,
      pixels: framebufferProtocolViews.pixels,
      frameSeq,
    };
  }

  return null;
};

const estimateFullFrameUploadBytes = (width: number, height: number): number => {
  const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
  const bytesPerRow = bytesPerRowForUpload(rowBytes, height);
  return requiredDataLen(bytesPerRow, rowBytes, height);
};

const presentOnce = async (): Promise<boolean> => {
  const t0 = performance.now();
  lastUploadDirtyRects = null;

  try {
    const frame = getCurrentFrameInfo();
    const dirtyRects = frame?.dirtyRects ?? null;
    if (isDeviceLost) return false;

    const clearSharedFramebufferDirty = () => {
      if (!frame?.sharedLayout || !sharedFramebufferViews) return;
      // `frame_dirty` is a producer->consumer "new frame" flag. Clearing it is
      // optional, but doing so allows producers to detect consumer liveness (and
      // some implementations may wait for it).
      //
      // Avoid clearing a newer frame: only clear if we still observe the same
      // published sequence number after the upload/present work completes.
      const header = sharedFramebufferViews.header;
      const seqNow = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
      if (seqNow !== frame.frameSeq) return;
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
      Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_DIRTY);
    };

    if (presentFn) {
      lastUploadDirtyRects = dirtyRects;
      const result = await presentFn(dirtyRects);
      if (typeof result === "boolean" ? result : true) {
        clearSharedFramebufferDirty();
      }
      return typeof result === "boolean" ? result : true;
    }

    if (presenter) {
      if (!frame) return false;

      if (frame.width !== presenterSrcWidth || frame.height !== presenterSrcHeight) {
        presenterSrcWidth = frame.width;
        presenterSrcHeight = frame.height;
        if (presenter.backend === "webgpu") surfaceReconfigures += 1;
        presenter.resize(frame.width, frame.height, outputDpr);
      }

      const dirtyPresenter = presenter as Presenter & {
        presentDirtyRects?: (frame: number | ArrayBuffer | ArrayBufferView, stride: number, dirtyRects: DirtyRect[]) => void;
      };
      if (dirtyRects && dirtyRects.length > 0 && typeof dirtyPresenter.presentDirtyRects === "function") {
        dirtyPresenter.presentDirtyRects(frame.pixels, frame.strideBytes, dirtyRects);
        lastUploadDirtyRects = dirtyRects;
      } else {
        presenter.present(frame.pixels, frame.strideBytes);
      }
      clearSharedFramebufferDirty();
      return true;
    }

    // Headless: treat as successfully presented so the shared frame state can
    // transition back to PRESENTED and avoid DIRTY→tick spam.
    clearSharedFramebufferDirty();
    return true;
  } finally {
    telemetry.recordPresentLatencyMs(performance.now() - t0);
  }
};

// -----------------------------------------------------------------------------
// AeroGPU command submissions (ACMD)
// -----------------------------------------------------------------------------

const AEROGPU_CMD_STREAM_MAGIC = 0x444d_4341; // "ACMD" little-endian
const AEROGPU_STREAM_HEADER_BYTES = 24;
const AEROGPU_CMD_HDR_BYTES = 8;

const AEROGPU_CMD_CREATE_TEXTURE2D = 0x101;
const AEROGPU_CMD_DESTROY_RESOURCE = 0x102;
const AEROGPU_CMD_UPLOAD_RESOURCE = 0x104;

const AEROGPU_CMD_SET_RENDER_TARGETS = 0x400;

const AEROGPU_CMD_PRESENT = 0x700;
const AEROGPU_CMD_PRESENT_EX = 0x701;

const AEROGPU_FORMAT_R8G8B8A8_UNORM = 3; // See `drivers/aerogpu/protocol/aerogpu_pci.h`.

const readU32LeChecked = (dv: DataView, offset: number, limit: number, label: string): number => {
  if (offset < 0 || offset + 4 > limit) {
    throw new Error(`aerogpu: truncated u32 (${label}) at offset ${offset}`);
  }
  return dv.getUint32(offset, true);
};

const readU64LeChecked = (dv: DataView, offset: number, limit: number, label: string): bigint => {
  if (offset < 0 || offset + 8 > limit) {
    throw new Error(`aerogpu: truncated u64 (${label}) at offset ${offset}`);
  }
  return dv.getBigUint64(offset, true);
};

const checkedU64ToNumber = (value: bigint, label: string): number => {
  if (value < 0n) throw new Error(`aerogpu: negative u64 (${label})`);
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`aerogpu: ${label} too large for JS number (${value} > ${Number.MAX_SAFE_INTEGER})`);
  }
  return Number(value);
};

const presentAerogpuTexture = (tex: AeroGpuCpuTexture): void => {
  aerogpuLastPresentedFrame = { width: tex.width, height: tex.height, rgba8: tex.data.slice().buffer };

  if (!presenter) return;

  if (tex.width !== presenterSrcWidth || tex.height !== presenterSrcHeight) {
    presenterSrcWidth = tex.width;
    presenterSrcHeight = tex.height;
    presenter.resize(tex.width, tex.height, outputDpr);
  }

  presenter.present(tex.data, tex.width * 4);
};

const executeAerogpuCmdStream = (cmdStream: ArrayBuffer): bigint => {
  const dv = new DataView(cmdStream);
  const bufLen = dv.byteLength;

  if (bufLen < AEROGPU_STREAM_HEADER_BYTES) {
    throw new Error(`aerogpu: cmd stream too small (${bufLen} bytes)`);
  }

  const magic = dv.getUint32(0, true);
  if (magic !== AEROGPU_CMD_STREAM_MAGIC) {
    throw new Error(`aerogpu: bad cmd stream magic 0x${magic.toString(16)} (expected 0x${AEROGPU_CMD_STREAM_MAGIC.toString(16)})`);
  }

  const sizeBytes = dv.getUint32(8, true);
  if (sizeBytes < AEROGPU_STREAM_HEADER_BYTES || sizeBytes > bufLen) {
    throw new Error(`aerogpu: invalid cmd stream size_bytes=${sizeBytes} (buffer_len=${bufLen})`);
  }

  let offset = AEROGPU_STREAM_HEADER_BYTES;
  let presentDelta = 0n;

  while (offset < sizeBytes) {
    if (offset + AEROGPU_CMD_HDR_BYTES > sizeBytes) {
      throw new Error(`aerogpu: truncated command header at offset ${offset}`);
    }

    const opcode = readU32LeChecked(dv, offset + 0, sizeBytes, "opcode");
    const cmdSizeBytes = readU32LeChecked(dv, offset + 4, sizeBytes, "size_bytes");

    if (cmdSizeBytes < AEROGPU_CMD_HDR_BYTES) {
      throw new Error(`aerogpu: invalid command size_bytes=${cmdSizeBytes} at offset ${offset}`);
    }
    if (cmdSizeBytes % 4 !== 0) {
      throw new Error(`aerogpu: misaligned command size_bytes=${cmdSizeBytes} at offset ${offset}`);
    }

    const end = offset + cmdSizeBytes;
    if (end > sizeBytes) {
      throw new Error(`aerogpu: command at offset ${offset} overruns stream (end=${end}, size=${sizeBytes})`);
    }

    switch (opcode) {
      case AEROGPU_CMD_CREATE_TEXTURE2D: {
        // struct aerogpu_cmd_create_texture2d is 56 bytes (including header).
        if (cmdSizeBytes < 56) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "texture_handle");
        const format = readU32LeChecked(dv, offset + 16, end, "format");
        const width = readU32LeChecked(dv, offset + 20, end, "width");
        const height = readU32LeChecked(dv, offset + 24, end, "height");

        if (width === 0 || height === 0) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D invalid dimensions ${width}x${height}`);
        }
        if (format !== AEROGPU_FORMAT_R8G8B8A8_UNORM) {
          throw new Error(`aerogpu: CREATE_TEXTURE2D unsupported format ${format} (only RGBA8_UNORM=${AEROGPU_FORMAT_R8G8B8A8_UNORM} supported)`);
        }

        const byteLen = width * height * 4;
        aerogpuTextures.set(handle, { width, height, format, data: new Uint8Array(byteLen) });
        break;
      }

      case AEROGPU_CMD_DESTROY_RESOURCE: {
        if (cmdSizeBytes < 16) {
          throw new Error(`aerogpu: DESTROY_RESOURCE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        aerogpuTextures.delete(handle);
        if (aerogpuCurrentRenderTarget === handle) aerogpuCurrentRenderTarget = null;
        break;
      }

      case AEROGPU_CMD_UPLOAD_RESOURCE: {
        // struct aerogpu_cmd_upload_resource is 32 bytes (including header), followed by `size_bytes` payload bytes.
        if (cmdSizeBytes < 32) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const handle = readU32LeChecked(dv, offset + 8, end, "resource_handle");
        const offsetBytes = checkedU64ToNumber(readU64LeChecked(dv, offset + 16, end, "offset_bytes"), "offset_bytes");
        const sizeBytesU64 = readU64LeChecked(dv, offset + 24, end, "size_bytes");
        const uploadBytes = checkedU64ToNumber(sizeBytesU64, "size_bytes");

        const dataStart = offset + 32;
        const dataEnd = dataStart + uploadBytes;
        if (dataEnd > end) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE payload overruns packet (dataEnd=${dataEnd}, end=${end})`);
        }

        const tex = aerogpuTextures.get(handle);
        if (!tex) {
          throw new Error(`aerogpu: UPLOAD_RESOURCE references unknown texture handle ${handle}`);
        }
        if (offsetBytes + uploadBytes > tex.data.byteLength) {
          throw new Error(
            `aerogpu: UPLOAD_RESOURCE out of bounds (offset=${offsetBytes}, size=${uploadBytes}, texBytes=${tex.data.byteLength})`,
          );
        }

        tex.data.set(new Uint8Array(cmdStream, dataStart, uploadBytes), offsetBytes);
        break;
      }

      case AEROGPU_CMD_SET_RENDER_TARGETS: {
        // struct aerogpu_cmd_set_render_targets is 48 bytes (including header).
        if (cmdSizeBytes < 48) {
          throw new Error(`aerogpu: SET_RENDER_TARGETS packet too small (size_bytes=${cmdSizeBytes})`);
        }
        const colorCount = readU32LeChecked(dv, offset + 8, end, "color_count");
        const rt0 = readU32LeChecked(dv, offset + 16, end, "colors[0]");
        aerogpuCurrentRenderTarget = colorCount > 0 ? rt0 : null;
        break;
      }

      case AEROGPU_CMD_PRESENT:
      case AEROGPU_CMD_PRESENT_EX: {
        aerogpuPresentCount += 1n;
        presentDelta += 1n;

        const rt = aerogpuCurrentRenderTarget;
        if (rt != null && rt !== 0) {
          const tex = aerogpuTextures.get(rt);
          if (!tex) {
            throw new Error(`aerogpu: PRESENT references missing render target handle ${rt}`);
          }
          presentAerogpuTexture(tex);
        }
        break;
      }

      default:
        // Unknown opcodes are skipped (forward-compat).
        break;
    }

    offset = end;
  }

  return presentDelta;
};

const handleSubmitAerogpu = async (req: GpuRuntimeSubmitAerogpuMessage): Promise<void> => {
  const signalFence = typeof req.signalFence === "bigint" ? req.signalFence : BigInt(req.signalFence);

  let presentDelta = 0n;
  try {
    await maybeSendReady();
    presentDelta = executeAerogpuCmdStream(req.cmdStream);
  } catch (err) {
    sendError(err);
  } finally {
    postToMain({
      type: "submit_complete",
      requestId: req.requestId,
      completedFence: signalFence,
      ...(presentDelta > 0n ? { presentCount: aerogpuPresentCount } : {}),
    });
  }
};

const handleTick = async () => {
  syncPerfFrame();
  refreshFramebufferViews();
  maybeUpdateFramesReceivedFromSeq();
  await maybeSendReady();

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
  }

  presenting = true;
  try {
    presentsAttempted += 1;
    const presentStartMs = performance.now();
    const didPresent = await presentOnce();
    perfGpuMs += performance.now() - presentStartMs;
    if (didPresent) {
      presentsSucceeded += 1;
      framesPresented += 1;

      const now = performance.now();
      if (lastFrameStartMs !== null) {
        telemetry.beginFrame(lastFrameStartMs);

        const frame = getCurrentFrameInfo();
        const textureUploadBytes = frame?.sharedLayout
          ? estimateTextureUploadBytes(frame.sharedLayout, lastUploadDirtyRects)
          : frame
            ? estimateFullFrameUploadBytes(frame.width, frame.height)
            : 0;
        telemetry.recordTextureUploadBytes(textureUploadBytes);
        perf.counter("textureUploadBytes", textureUploadBytes);
        perfUploadBytes += textureUploadBytes;
        telemetry.endFrame(now);
      }
      lastFrameStartMs = now;
    } else {
      framesDropped += 1;
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
// Presenter backend init (OffscreenCanvas path)
// -----------------------------------------------------------------------------

function postPresenterError(err: unknown, backend?: PresenterBackendKind): void {
  const deviceLostCode = getDeviceLostCode(err);
  if (deviceLostCode) {
    const startRecovery = deviceLostCode !== "webgl_context_lost";
    handleDeviceLost(
      err instanceof Error ? err.message : String(err),
      { source: "presenter", backend, code: deviceLostCode, error: err },
      startRecovery,
    );
    return;
  }

  if (err instanceof PresenterError) {
    postToMain({ type: "error", message: err.message, code: err.code, backend: backend ?? presenter?.backend });
    postRuntimeError(err.message);
    return;
  }

  const message = err instanceof Error ? err.message : String(err);
  postToMain({ type: "error", message, backend: backend ?? presenter?.backend });
  postRuntimeError(message);
}

async function tryInitBackend(
  backend: PresenterBackendKind,
  canvas: OffscreenCanvas,
  width: number,
  height: number,
  dpr: number,
  opts: PresenterInitOptions,
  generation: number,
): Promise<Presenter> {
  if (backend === "webgpu" && runtimeOptions?.disableWebGpu === true) {
    throw new PresenterError("webgpu_disabled", "WebGPU backend was disabled by init options");
  }

  // Ensure backend errors are surfaced even if the caller didn't pass an onError.
  opts.onError = (e) => {
    if (generation !== presenterErrorGeneration) return;
    postPresenterError(e, backend);
    presenterUserOnError?.(e);
  };

  switch (backend) {
    case "webgpu": {
      const mod = await import("../gpu/webgpu-presenter-backend");
      const p = new mod.WebGpuPresenterBackend();
      await p.init(canvas, width, height, dpr, opts);
      return p;
    }
    case "webgl2_wgpu": {
      const mod = await import("../gpu/wgpu-webgl2-presenter");
      const p = new mod.WgpuWebGl2Presenter();
      await p.init(canvas, width, height, dpr, opts);
      return p;
    }
    case "webgl2_raw": {
      const p = new RawWebGl2Presenter();
      p.init(canvas, width, height, dpr, opts);
      return p;
    }
    default: {
      const unreachable: never = backend;
      throw new PresenterError("unknown_backend", `Unknown backend ${unreachable}`);
    }
  }
}

async function initPresenterForRuntime(canvas: OffscreenCanvas, width: number, height: number): Promise<void> {
  presenter?.destroy?.();
  presenter = null;
  presenterFallback = undefined;
  presenterErrorGeneration += 1;
  const generation = presenterErrorGeneration;

  const dpr = outputDpr || 1;

  const opts = presenterInitOptions ?? {};
  presenterInitOptions = opts;

  if (outputWidthCss != null) opts.outputWidth = outputWidthCss;
  if (outputHeightCss != null) opts.outputHeight = outputHeightCss;

  const forceBackend = runtimeOptions?.forceBackend;
  const disableWebGpu = runtimeOptions?.disableWebGpu === true;
  const preferWebGpu = runtimeOptions?.preferWebGpu !== false;

  let backends: PresenterBackendKind[];
  if (forceBackend) {
    backends = [forceBackend];
  } else {
    backends = preferWebGpu ? ["webgpu", "webgl2_raw"] : ["webgl2_raw", "webgpu"];
    if (disableWebGpu && !preferWebGpu) {
      // When WebGPU is disabled and WebGL2 is preferred, never attempt WebGPU.
      backends = ["webgl2_raw"];
    }
  }

  const firstBackend = backends[0];
  let firstError: unknown | null = null;
  let lastError: unknown | null = null;

  for (const backend of backends) {
    try {
      presenter = await tryInitBackend(backend, canvas, width, height, dpr, opts, generation);
      presenterSrcWidth = width;
      presenterSrcHeight = height;
      if (presenter.backend === "webgpu") surfaceReconfigures += 1;
      syncCursorToPresenter();

      if (backend !== firstBackend && firstError) {
        const reason = firstError instanceof Error ? firstError.message : String(firstError);
        presenterFallback = {
          from: firstBackend,
          to: backend,
          reason,
          originalErrorMessage: reason,
        };
      }

      return;
    } catch (err) {
      if (!firstError) firstError = err;
      lastError = err;
    }
  }

  throw lastError ?? new PresenterError("no_backend", "No GPU presenter backend could be initialized");
}

async function maybeSendReady(): Promise<void> {
  if (runtimeReadySent) return;
  if (!runtimeInit) return;
  if (isDeviceLost) return;

  // Headless mode: still run frame pacing/metrics.
  if (!runtimeCanvas) {
    runtimeReadySent = true;
    postToMain({ type: "ready", backendKind: "headless" });
    return;
  }

  if (presenter) {
    runtimeReadySent = true;
    postToMain({ type: "ready", backendKind: presenter.backend, fallback: presenterFallback });
    return;
  }

  const frame = getCurrentFrameInfo();
  if (!frame) return;

  if (!presenterInitPromise) {
    presenterInitPromise = initPresenterForRuntime(runtimeCanvas, frame.width, frame.height)
      .catch((err) => {
        postPresenterError(err);
      })
      .finally(() => {
        presenterInitPromise = null;
      });
  }

  await presenterInitPromise;
  if (!presenter) return;

  runtimeReadySent = true;
  postToMain({ type: "ready", backendKind: presenter.backend, fallback: presenterFallback });
}

const handleRuntimeInit = (init: WorkerInitMessage) => {
  role = init.role ?? 'gpu';
  const segments = {
    control: init.controlSab,
    guestMemory: init.guestMemory,
    vgaFramebuffer: init.vgaFramebuffer,
    ioIpc: init.ioIpcSab,
    sharedFramebuffer: init.sharedFramebuffer,
    sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
  };
  status = createSharedMemoryViews(segments).status;

  const regions = ringRegionsForWorker(role);
  commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
  eventRing = new RingBuffer(segments.control, regions.event.byteOffset);

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
  pushRuntimeEvent({ kind: 'log', level: 'info', message: 'worker ready' });
  startRuntimePolling();
  ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
};

function startRuntimePolling(): void {
  if (!status || runtimePollTimer !== null) return;
  // Keep the GPU worker responsive to `postMessage` frame scheduler traffic: avoid blocking
  // waits and instead poll the shutdown command ring at a low rate.
  runtimePollTimer = setInterval(() => {
    drainRuntimeCommands();
    if (status && Atomics.load(status, StatusIndex.StopRequested) === 1) {
      shutdownRuntime();
    }
  }, 8) as unknown as number;
}

function drainRuntimeCommands(): void {
  if (!status || !commandRing) return;
  while (true) {
    const bytes = commandRing.tryPop();
    if (!bytes) break;
    let cmd: Command;
    try {
      cmd = decodeCommand(bytes);
    } catch {
      continue;
    }
    if (cmd.kind === 'shutdown') {
      Atomics.store(status, StatusIndex.StopRequested, 1);
    }
  }
}

function shutdownRuntime(): void {
  if (!status) return;
  if (runtimePollTimer !== null) {
    clearInterval(runtimePollTimer);
    runtimePollTimer = null;
  }
  pushRuntimeEvent({ kind: 'log', level: 'info', message: 'worker shutdown' });
  setReadyFlag(status, role, false);
  ctx.close();
}

function pushRuntimeEvent(evt: Event): void {
  if (!eventRing) return;
  eventRing.tryPush(encodeEvent(evt));
}

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

  const msg = data as Partial<GpuRuntimeInMessage>;
  if (!msg || typeof msg !== "object" || typeof msg.type !== "string") return;

  switch (msg.type) {
    case "init": {
      const init = msg as GpuRuntimeInitMessage;

      perf.spanBegin("worker:init");
      try {
        stopTelemetryPolling();
        uninstallContextLossHandlers();
        isDeviceLost = false;
        recoveryPromise = null;

        runtimeInit = init;
        runtimeCanvas = init.canvas ?? null;
        runtimeOptions = init.options ?? null;
        runtimeReadySent = false;

        if (runtimeCanvas) installContextLossHandlers(runtimeCanvas);

        outputWidthCss = runtimeOptions?.outputWidth ?? null;
        outputHeightCss = runtimeOptions?.outputHeight ?? null;
        outputDpr = runtimeOptions?.dpr ?? 1;

        frameState = new Int32Array(init.sharedFrameState);

        framesReceived = 0;
        framesPresented = 0;
        framesDropped = 0;
        lastSeenSeq = Atomics.load(frameState, FRAME_SEQ_INDEX);
        lastPresentedSeq = lastSeenSeq;

        presentsAttempted = 0;
        presentsSucceeded = 0;
        recoveriesAttempted = 0;
        recoveriesSucceeded = 0;
        surfaceReconfigures = 0;

        telemetry.reset();
        lastFrameStartMs = null;

        sharedFramebufferViews = null;
        sharedFramebufferLayoutKey = null;
        framebufferProtocolViews = null;
        framebufferProtocolLayoutKey = null;
        (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer = undefined;

        presenter?.destroy?.();
        presenter = null;
        presenterFallback = undefined;
        presenterInitPromise = null;
        presenterSrcWidth = 0;
        presenterSrcHeight = 0;

        aerogpuTextures.clear();
        aerogpuCurrentRenderTarget = null;
        aerogpuPresentCount = 0n;
        aerogpuLastPresentedFrame = null;
        aerogpuSubmitChain = Promise.resolve();
        cursorImage = null;
        cursorWidth = 0;
        cursorHeight = 0;
        cursorEnabled = false;
        cursorX = 0;
        cursorY = 0;
        cursorHotX = 0;
        cursorHotY = 0;
        cursorRenderEnabled = true;

        presenterUserOnError = runtimeOptions?.presenter?.onError;
        presenterInitOptions = { ...(runtimeOptions?.presenter ?? {}) };
        // Backend init installs its own error handler wrapper.
        presenterInitOptions.onError = undefined;

        presentFn = null;
        presentModule = null;
        wasmInitPromise = null;
        if (runtimeOptions?.wasmModuleUrl) {
          wasmInitPromise = perf
            .spanAsync("wasm:init", () => loadPresentFnFromModuleUrl(runtimeOptions.wasmModuleUrl!))
            .catch((err) => {
              sendError(err);
            })
            .finally(() => {
              wasmInitPromise = null;
            });
        }

        refreshFramebufferViews();
        void maybeSendReady();
        startTelemetryPolling();
      } catch (err) {
        sendError(err);
      } finally {
        perf.spanEnd("worker:init");
      }
      break;
    }

    case "resize": {
      const resize = msg as { width: number; height: number; dpr: number };
      outputWidthCss = resize.width;
      outputHeightCss = resize.height;
      outputDpr = resize.dpr || 1;

      if (presenterInitOptions) {
        presenterInitOptions.outputWidth = outputWidthCss;
        presenterInitOptions.outputHeight = outputHeightCss;
      }

      void (async () => {
        await maybeSendReady();
        if (!presenter) return;
        try {
          if (presenter.backend === "webgpu") surfaceReconfigures += 1;
          presenter.resize(presenterSrcWidth, presenterSrcHeight, outputDpr);
        } catch (err) {
          postPresenterError(err, presenter.backend);
        }
      })();
      break;
    }

    case "tick": {
      void (msg as { frameTimeMs?: unknown }).frameTimeMs;
      void handleTick();
      break;
    }

    case "submit_aerogpu": {
      const req = msg as GpuRuntimeSubmitAerogpuMessage;
      aerogpuSubmitChain = aerogpuSubmitChain
        .catch(() => {
          // Ensure a previous failed submission does not permanently stall the chain.
        })
        .then(() => handleSubmitAerogpu(req));
      break;
    }

    case "screenshot": {
      const req = msg as GpuRuntimeScreenshotRequestMessage;
      void (async () => {
        const postStub = (seq?: number) => {
          const rgba8 = new Uint8Array([0, 0, 0, 255]).buffer;
          postToMain(
            {
              type: "screenshot",
              requestId: req.requestId,
              width: 1,
              height: 1,
              rgba8,
              origin: "top-left",
              ...(typeof seq === "number" ? { frameSeq: seq } : {}),
            },
            [rgba8],
          );
        };

        const waitForNotPresenting = async (timeoutMs: number): Promise<boolean> => {
          const deadline = performance.now() + timeoutMs;
          while (presenting && performance.now() < deadline) {
            await new Promise((resolve) => setTimeout(resolve, 0));
          }
          return !presenting;
        };

        try {
          await maybeSendReady();
          const includeCursor = req.includeCursor === true;

          // Ensure the screenshot reflects the latest presented pixels. The shared
          // framebuffer producer can advance `frameSeq` before the presenter runs,
          // so relying on the header sequence alone can lead to mismatched
          // (seq, pixels) pairs in smoke tests and automation.
          if (frameState) {
            if (!(await waitForNotPresenting(200))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }

            if (!isDeviceLost && shouldPresentWithSharedState()) {
              await handleTick();
            }

            if (!(await waitForNotPresenting(200))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }
          }

          const seq = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;

          if (presenter && !isDeviceLost) {
            const prevCursorRenderEnabled = cursorRenderEnabled;
            const needsCursorDisabledForScreenshot = !includeCursor && presenter.backend !== "webgpu";
            if (needsCursorDisabledForScreenshot) {
              cursorRenderEnabled = false;
              syncCursorToPresenter();
            }

            try {
              const shot = await presenter.screenshot();
              let pixels = shot.pixels;

              // WebGPU screenshots read back the source texture only, so cursor composition
              // must be applied explicitly when requested.
              if (includeCursor && presenter.backend === "webgpu") {
                try {
                  const out = new Uint8Array(pixels);
                  compositeCursorOverRgba8(
                    out,
                    shot.width,
                    shot.height,
                    cursorEnabled,
                    cursorImage,
                    cursorWidth,
                    cursorHeight,
                    cursorX,
                    cursorY,
                    cursorHotX,
                    cursorHotY,
                  );
                  pixels = out.buffer;
                } catch {
                  // Ignore; screenshot cursor compositing is best-effort.
                }
              }
              postToMain(
                {
                  type: "screenshot",
                  requestId: req.requestId,
                  width: shot.width,
                  height: shot.height,
                  rgba8: pixels,
                  origin: "top-left",
                  ...(typeof seq === "number" ? { frameSeq: seq } : {}),
                },
                [pixels],
              );
            } finally {
              if (needsCursorDisabledForScreenshot) {
                cursorRenderEnabled = prevCursorRenderEnabled;
                syncCursorToPresenter();
                getCursorPresenter()?.redraw?.();
              }
            }
            return;
          }

          const last = aerogpuLastPresentedFrame;
          if (last) {
            const out = last.rgba8.slice(0);
            postToMain(
              {
                type: "screenshot",
                requestId: req.requestId,
                width: last.width,
                height: last.height,
                rgba8: out,
                origin: "top-left",
                ...(typeof seq === "number" ? { frameSeq: seq } : {}),
              },
              [out],
            );
            return;
          }

          // Headless mode: copy the source buffer directly.
          if (!runtimeCanvas) {
            const frame = getCurrentFrameInfo();
            if (!frame) {
              postStub(typeof seq === "number" ? seq : undefined);
              return;
            }

            const rowBytes = frame.width * BYTES_PER_PIXEL_RGBA8;
            const out = new Uint8Array(rowBytes * frame.height);
            for (let y = 0; y < frame.height; y += 1) {
              const srcStart = y * frame.strideBytes;
              const dstStart = y * rowBytes;
              out.set(frame.pixels.subarray(srcStart, srcStart + rowBytes), dstStart);
            }

            if (includeCursor) {
              try {
                compositeCursorOverRgba8(
                  out,
                  frame.width,
                  frame.height,
                  cursorEnabled,
                  cursorImage,
                  cursorWidth,
                  cursorHeight,
                  cursorX,
                  cursorY,
                  cursorHotX,
                  cursorHotY,
                );
              } catch {
                // Ignore; screenshot cursor compositing is best-effort.
              }
            }

            postToMain(
              {
                type: "screenshot",
                requestId: req.requestId,
                width: frame.width,
                height: frame.height,
                rgba8: out.buffer,
                origin: "top-left",
                frameSeq: frame.frameSeq,
              },
              [out.buffer],
            );
            return;
          }

          // Presenter not ready (or device lost): return a minimal stub instead of hanging.
          postStub(typeof seq === "number" ? seq : undefined);
        } catch (err) {
          const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
          const deviceLostCode = getDeviceLostCode(err);
          if (deviceLostCode) {
            const startRecovery = deviceLostCode !== "webgl_context_lost";
            handleDeviceLost(
              err instanceof Error ? err.message : String(err),
              { source: "screenshot", code: deviceLostCode, error: err },
              startRecovery,
            );
          } else {
            emitGpuEvent({
              time_ms: performance.now(),
              backend_kind: backendKindForEvent(),
              severity: "error",
              category: "Screenshot",
              message: err instanceof Error ? err.message : String(err),
              details: err instanceof Error ? { message: err.message, stack: err.stack } : String(err),
            });
          }
          postStub(typeof seqNow === "number" ? seqNow : undefined);
        }
      })();
      break;
    }

    case "cursor_set_image": {
      const req = msg as GpuRuntimeCursorSetImageMessage;
      const w = Math.max(0, req.width | 0);
      const h = Math.max(0, req.height | 0);
      if (w === 0 || h === 0) {
        postPresenterError(new PresenterError("invalid_cursor_image", "cursor_set_image width/height must be non-zero"));
        break;
      }

      cursorWidth = w;
      cursorHeight = h;
      cursorImage = new Uint8Array(req.rgba8);
      syncCursorToPresenter();
      redrawCursor();
      break;
    }

    case "cursor_set_state": {
      const req = msg as GpuRuntimeCursorSetStateMessage;
      cursorEnabled = !!req.enabled;
      cursorX = req.x | 0;
      cursorY = req.y | 0;
      cursorHotX = Math.max(0, req.hotX | 0);
      cursorHotY = Math.max(0, req.hotY | 0);
      syncCursorToPresenter();
      redrawCursor();
      break;
    }

    case "shutdown": {
      stopTelemetryPolling();
      uninstallContextLossHandlers();
      presenter?.destroy?.();
      presenter = null;
      runtimeInit = null;
      runtimeCanvas = null;
      runtimeOptions = null;
      runtimeReadySent = false;
      aerogpuTextures.clear();
      aerogpuCurrentRenderTarget = null;
      aerogpuLastPresentedFrame = null;
      ctx.close();
      break;
    }
  }
};

void currentConfig;
