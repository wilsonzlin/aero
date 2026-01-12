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
  GPU_PROTOCOL_NAME,
  GPU_PROTOCOL_VERSION,
  isGpuWorkerMessageBase,
  type FrameTimingsReport,
  type GpuRuntimeErrorEvent,
  type GpuRuntimeFallbackInfo,
  type GpuRuntimeCursorSetImageMessage,
  type GpuRuntimeCursorSetStateMessage,
  type GpuRuntimeInitMessage,
  type GpuRuntimeInitOptions,
  type GpuRuntimeInMessage,
  type GpuRuntimeOutMessage,
  type GpuRuntimeScreenshotRequestMessage,
  type GpuRuntimeSubmitAerogpuMessage,
  type GpuRuntimeStatsCountersV1,
} from "../ipc/gpu-protocol";

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
import {
  createAerogpuCpuExecutorState,
  decodeAerogpuAllocTable,
  executeAerogpuCmdStream,
  resetAerogpuCpuExecutorState,
  type AeroGpuCpuTexture,
  type AerogpuCpuExecutorState,
} from "./aerogpu-acmd-executor.ts";
import {
  AEROGPU_PRESENT_FLAG_VSYNC,
  AerogpuCmdOpcode,
  AerogpuCmdStreamIter,
} from "../../../emulator/protocol/aerogpu/aerogpu_cmd.ts";

type PresentFn = (dirtyRects?: DirtyRect[] | null) => void | boolean | Promise<void | boolean>;

const ctx = self as unknown as DedicatedWorkerGlobalScope;
void installWorkerPerfHandlers();

const GPU_MESSAGE_BASE = { protocol: GPU_PROTOCOL_NAME, protocolVersion: GPU_PROTOCOL_VERSION } as const;

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;
// `GpuRuntimeOutMessage` is a tagged union; built-in `Omit<>` would collapse it to
// only the keys shared by all variants. Use a distributive form so variant-specific
// payload fields (framesReceived, requestId, etc) remain type-checked.
type OutboundGpuRuntimeMessage = DistributiveOmit<GpuRuntimeOutMessage, "protocol" | "protocolVersion">;

const postToMain = (msg: OutboundGpuRuntimeMessage, transfer?: Transferable[]) => {
  ctx.postMessage({ ...msg, ...GPU_MESSAGE_BASE }, transfer ?? []);
};

const postRuntimeError = (message: string) => {
  if (!status) return;
  pushRuntimeEvent({ kind: 'log', level: 'error', message });
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
};

let role: WorkerRole = "gpu";
let status: Int32Array | null = null;
let guestU8: Uint8Array | null = null;

let frameState: Int32Array | null = null;

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;
let perfCurrentFrameId = 0;
let perfGpuMs = 0;
let perfUploadBytes = 0;
let latestFrameTimings: FrameTimingsReport | null = null;
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
// OffscreenCanvas only supports a single graphics context type. Once we have created a
// WebGPU or WebGL2 context on the canvas, attempts to initialize a different backend
// will inevitably fail (e.g. `getContext('webgl2')` returns null after `getContext('webgpu')`).
//
// Track which family has been created so recovery paths don't attempt impossible fallbacks.
let runtimeCanvasContextKind: "webgpu" | "webgl2" | null = null;
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
let presenterNeedsFullUpload = true;

// -----------------------------------------------------------------------------
// AeroGPU command submission (ACMD)
// -----------------------------------------------------------------------------

const aerogpuContexts = new Map<number, AerogpuCpuExecutorState>();
const aerogpuWasmExecutorContexts = new Set<number>();

type AerogpuLastPresentedFrame = NonNullable<AerogpuCpuExecutorState["lastPresentedFrame"]>;
let aerogpuLastPresentedFrame: AerogpuLastPresentedFrame | null = null;
let aerogpuPresentCount = 0n;
let aerogpuWasmPresentCount = 0n;
let aerogpuLastOutputSource: "framebuffer" | "aerogpu" = "framebuffer";

const getAerogpuContextState = (contextId: number): AerogpuCpuExecutorState => {
  const key = contextId >>> 0;
  const existing = aerogpuContexts.get(key);
  if (existing) return existing;
  const created = createAerogpuCpuExecutorState();
  aerogpuContexts.set(key, created);
  return created;
};

const resetAerogpuContexts = (): void => {
  for (const state of aerogpuContexts.values()) {
    resetAerogpuCpuExecutorState(state);
  }
  aerogpuContexts.clear();
  aerogpuWasmExecutorContexts.clear();
  aerogpuLastPresentedFrame = null;
  aerogpuPresentCount = 0n;
  aerogpuWasmPresentCount = 0n;
};

type AeroGpuWasmApi = typeof import("../wasm/aero-gpu");

let aerogpuWasm: AeroGpuWasmApi | null = null;
let aerogpuWasmLoadPromise: Promise<AeroGpuWasmApi> | null = null;
let aerogpuWasmD3d9InitPromise: Promise<void> | null = null;
let aerogpuWasmD3d9InitBackend: PresenterBackendKind | null = null;
let aerogpuWasmD3d9Backend: PresenterBackendKind | null = null;
let aerogpuWasmD3d9InternalCanvas: OffscreenCanvas | null = null;

async function loadAerogpuWasm(): Promise<AeroGpuWasmApi> {
  if (aerogpuWasm) return aerogpuWasm;
  if (!aerogpuWasmLoadPromise) {
    aerogpuWasmLoadPromise = (async () => {
      const mod = (await import("../wasm/aero-gpu")) as AeroGpuWasmApi;
      await mod.default();
      aerogpuWasm = mod;
      return mod;
    })();
  }
  return await aerogpuWasmLoadPromise;
}

async function tryGetAeroGpuWasmFrameTimings(): Promise<FrameTimingsReport | null> {
  if (presenter?.backend !== "webgl2_wgpu") return null;
  let wasm = aerogpuWasm;
  if (!wasm) {
    try {
      wasm = await loadAerogpuWasm();
    } catch {
      return null;
    }
  }

  try {
    const report = wasm.get_frame_timings();
    if (report && typeof report === "object") {
      latestFrameTimings = report;
    }
    return report ?? null;
  } catch {
    return null;
  }
}

async function ensureAerogpuWasmD3d9(backend: PresenterBackendKind): Promise<AeroGpuWasmApi> {
  const mod = await loadAerogpuWasm();

  // The wasm D3D9 executor is wgpu-backed. When the worker is using the raw WebGL2 presenter we
  // still run the executor on the wgpu WebGL2 backend (not the raw backend).
  const normalizedBackend: PresenterBackendKind = backend === "webgl2_raw" ? "webgl2_wgpu" : backend;

  // If WebGPU init failed previously, the executor may be running on the WebGL2 backend even
  // though the caller requested WebGPU. Treat that as a satisfied init so we don't keep retrying
  // WebGPU on every submit.
  if (normalizedBackend === "webgpu" && aerogpuWasmD3d9Backend === "webgl2_wgpu") return mod;

  // If an init for a different backend is in flight, wait for it to finish first.
  if (aerogpuWasmD3d9InitPromise && aerogpuWasmD3d9InitBackend !== normalizedBackend) {
    await aerogpuWasmD3d9InitPromise;
  }

  // Reset if the backend target changed (webgpu <-> webgl2_wgpu).
  if (aerogpuWasmD3d9Backend && aerogpuWasmD3d9Backend !== normalizedBackend) {
    aerogpuWasmD3d9Backend = null;
    aerogpuWasmD3d9InternalCanvas = null;
  }

  if (aerogpuWasmD3d9Backend === normalizedBackend) return mod;

  if (!aerogpuWasmD3d9InitPromise) {
    aerogpuWasmD3d9InitBackend = normalizedBackend;
    aerogpuWasmD3d9InitPromise = (async () => {
      const requiredFeatures = runtimeOptions?.presenter?.requiredFeatures as unknown as string[] | undefined;

      if (normalizedBackend === "webgpu") {
        try {
          await mod.init_aerogpu_d3d9(undefined, { preferWebGpu: true, disableWebGpu: false, requiredFeatures });
          aerogpuWasmD3d9Backend = "webgpu";
          return;
        } catch {
          // Fall back to the wgpu WebGL2 backend using an internal OffscreenCanvas.
        }
      }

      // WebGL2 path requires a surface; use a private OffscreenCanvas so we don't conflict with the
      // presenter's canvas context.
      aerogpuWasmD3d9InternalCanvas = new OffscreenCanvas(1, 1);
      await mod.init_aerogpu_d3d9(aerogpuWasmD3d9InternalCanvas, {
        preferWebGpu: false,
        disableWebGpu: true,
        requiredFeatures,
      });
      aerogpuWasmD3d9Backend = "webgl2_wgpu";
    })()
      .catch((err) => {
        // Ensure failed inits can be retried.
        aerogpuWasmD3d9Backend = null;
        aerogpuWasmD3d9InternalCanvas = null;
        throw err;
      })
      .finally(() => {
        aerogpuWasmD3d9InitPromise = null;
        aerogpuWasmD3d9InitBackend = null;
      });
  }

  await aerogpuWasmD3d9InitPromise;
  return mod;
}

// Ensure submissions execute serially even though message handlers are async.
let aerogpuSubmitChain: Promise<void> = Promise.resolve();

type AerogpuSubmitCompletionKind = "immediate" | "vsync";

type PendingAerogpuSubmitComplete = {
  requestId: number;
  completedFence: bigint;
  presentCount?: bigint;
  kind: AerogpuSubmitCompletionKind;
};

const aerogpuPendingSubmitComplete: PendingAerogpuSubmitComplete[] = [];

const postAerogpuSubmitComplete = (entry: PendingAerogpuSubmitComplete): void => {
  postToMain({
    type: "submit_complete",
    requestId: entry.requestId,
    completedFence: entry.completedFence,
    ...(entry.presentCount !== undefined ? { presentCount: entry.presentCount } : {}),
  });
};

const enqueueAerogpuSubmitComplete = (entry: PendingAerogpuSubmitComplete): void => {
  // Preserve the current immediate-completion behavior unless a vsync-paced present has
  // introduced a completion barrier.
  if (entry.kind === "immediate" && aerogpuPendingSubmitComplete.length === 0) {
    postAerogpuSubmitComplete(entry);
    return;
  }
  aerogpuPendingSubmitComplete.push(entry);
};

const flushAerogpuSubmitCompleteOnTick = (): void => {
  if (aerogpuPendingSubmitComplete.length === 0) return;

  const first = aerogpuPendingSubmitComplete[0]!;
  if (first.kind === "vsync") {
    // Complete at most one vsync-paced submission per tick, then release any immediate
    // submissions queued behind it.
    postAerogpuSubmitComplete(aerogpuPendingSubmitComplete.shift()!);
    while (aerogpuPendingSubmitComplete[0]?.kind === "immediate") {
      postAerogpuSubmitComplete(aerogpuPendingSubmitComplete.shift()!);
    }
    return;
  }

  // No vsync barrier at the head; flush any immediate completions.
  while (aerogpuPendingSubmitComplete[0]?.kind === "immediate") {
    postAerogpuSubmitComplete(aerogpuPendingSubmitComplete.shift()!);
  }
};

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

  // If the presenter does not implement the cursor APIs, there is nothing to redraw.
  if (!p.setCursorImageRgba8 && !p.setCursorState && !p.setCursorRenderEnabled) return;
  if (!presenter) return;

  // Best-effort fallback: re-present the last output so backends that only apply cursor state
  // during present() can reflect the latest cursor updates without clobbering the current
  // output source (framebuffer vs AeroGPU scanout).
  if (aerogpuLastOutputSource === "aerogpu") {
    const last = aerogpuLastPresentedFrame;
    if (!last) return;
    presenter.present(last.rgba8, last.width * BYTES_PER_PIXEL_RGBA8);
    return;
  }

  const frame = getCurrentFrameInfo();
  if (!frame) return;
  aerogpuLastOutputSource = "framebuffer";
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

const flushPerfFrameSample = (frameId: number) => {
  if (!perfWriter) return;
  if (frameId === 0) return;

  perfWriter.frameSample(frameId, {
    durations: { gpu_ms: perfGpuMs > 0 ? perfGpuMs : 0.01 },
  });
  if (perfUploadBytes > 0) {
    perfWriter.graphicsSample(frameId, {
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
    // First non-zero frame ID after enabling perf. If we already accumulated some
    // work while `frameId` was still 0, attribute that work to this first frame.
    if (perfGpuMs > 0 || perfUploadBytes > 0) {
      flushPerfFrameSample(frameId);
    }
    perfCurrentFrameId = frameId;
    return;
  }

  if (frameId !== perfCurrentFrameId) {
    // The shared frame ID is advanced by the main thread at the start of each RAF tick.
    // At the moment we observe a new `frameId`, our accumulated counters correspond to
    // work performed while the previous frame ID was active. Attribute that work to
    // the *new* frame ID so it merges with the main-thread frame-time sample.
    flushPerfFrameSample(frameId);
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

const bytesPerRowForUpload = (rowBytes: number, copyHeight: number, bytesPerRowAlignment: number): number => {
  if (copyHeight <= 1) return rowBytes;
  return alignUp(rowBytes, bytesPerRowAlignment);
};

const requiredDataLen = (bytesPerRow: number, rowBytes: number, copyHeight: number): number => {
  if (copyHeight <= 0) return 0;
  return bytesPerRow * (copyHeight - 1) + rowBytes;
};

const clampInt = (value: number, min: number, max: number): number =>
  Math.max(min, Math.min(max, Math.trunc(value)));

const bytesPerRowAlignmentForPresenterBackend = (backend: PresenterBackendKind | null): number => {
  // Telemetry-only estimate of texture upload bandwidth.
  //
  // WebGPU (and the wgpu-backed WebGL2 presenter) are constrained by the WebGPU 256-byte
  // bytesPerRow alignment requirement. The raw WebGL2 presenter uploads with gl.tex(Sub)Image2D
  // and does not require this padding.
  switch (backend) {
    case "webgl2_raw":
      return 1;
    case "webgpu":
    case "webgl2_wgpu":
      return COPY_BYTES_PER_ROW_ALIGNMENT;
    default:
      // Headless/unknown/custom presenter: no upload occurs, but keep the historical WebGPU-style
      // alignment so existing telemetry remains comparable.
      return COPY_BYTES_PER_ROW_ALIGNMENT;
  }
};

const estimateTextureUploadBytes = (
  layout: SharedFramebufferLayout | null,
  dirtyRects: DirtyRect[] | null,
  bytesPerRowAlignment: number,
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
    const bytesPerRow = bytesPerRowForUpload(rowBytes, h, bytesPerRowAlignment);
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

function sanitizeForPostMessage(value: unknown): unknown {
  if (value === undefined) return undefined;
  if (value === null) return null;
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean" || typeof value === "bigint") {
    return value;
  }

  // Errors are not consistently structured-cloneable across browsers. Convert them to plain objects so
  // telemetry/event reporting cannot crash the worker with a DataCloneError.
  if (value instanceof PresenterError) {
    return { name: value.name, code: value.code, message: value.message, stack: value.stack };
  }
  if (value instanceof Error) {
    return { name: value.name, message: value.message, stack: value.stack };
  }

  // Try the platform structured-clone implementation first (handles ArrayBuffer, Map, Set, etc).
  if (typeof structuredClone === "function") {
    try {
      return structuredClone(value);
    } catch {
      // Fall back below.
    }
  }

  // Best-effort JSON serialization for plain objects (converts nested Errors).
  try {
    return JSON.parse(
      JSON.stringify(value, (_key, v) => {
        if (typeof v === "bigint") return v.toString();
        if (v instanceof PresenterError) {
          return { name: v.name, code: v.code, message: v.message, stack: v.stack };
        }
        if (v instanceof Error) {
          return { name: v.name, message: v.message, stack: v.stack };
        }
        return v as unknown;
      }),
    );
  } catch {
    // Fall through to string.
  }

  try {
    return String(value);
  } catch {
    return undefined;
  }
}

function postGpuEvents(events: GpuRuntimeErrorEvent[]): void {
  if (events.length === 0) return;
  const sanitized = events.map((event) => {
    if (event.details === undefined) return event;
    return { ...event, details: sanitizeForPostMessage(event.details) };
  });
  postToMain({ type: "events", version: 1, events: sanitized });
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
  const sanitizedWasmStats = wasmStats === undefined ? undefined : sanitizeForPostMessage(wasmStats);
  const sanitizedFrameTimings = latestFrameTimings ? sanitizeForPostMessage(latestFrameTimings) : undefined;

  let wasm: unknown | undefined = sanitizedWasmStats;
  if (sanitizedFrameTimings !== undefined) {
    if (wasm === undefined) {
      wasm = { frameTimings: sanitizedFrameTimings };
    } else if (wasm && typeof wasm === "object" && !Array.isArray(wasm)) {
      wasm = { ...(wasm as Record<string, unknown>), frameTimings: sanitizedFrameTimings };
    } else {
      wasm = { wasm, frameTimings: sanitizedFrameTimings };
    }
  }
  postToMain({
    type: "stats",
    version: 1,
    timeMs: performance.now(),
    ...(backendKind ? { backendKind } : {}),
    counters: getStatsCounters(),
    ...(wasm === undefined ? {} : { wasm }),
  });
}

function mergeWasmTelemetry(wasmStats: unknown | undefined, frameTimings: unknown | undefined): unknown | undefined {
  if (wasmStats === undefined && frameTimings === undefined) return undefined;
  if (frameTimings === undefined) return wasmStats;
  if (wasmStats === undefined) return { frame_timings: frameTimings };
  if (typeof wasmStats === "object" && wasmStats !== null && !Array.isArray(wasmStats)) {
    return { ...(wasmStats as Record<string, unknown>), frame_timings: frameTimings };
  }
  return { stats: wasmStats, frame_timings: frameTimings };
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

function parseMaybeJson(value: unknown): unknown {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

async function tryGetWasmStats(): Promise<unknown | undefined> {
  const fn = getModuleExportFn<() => unknown | Promise<unknown>>(["get_gpu_stats", "getGpuStats"]);
  if (!fn) return undefined;
  try {
    return parseMaybeJson(await fn());
  } catch {
    return undefined;
  }
}

async function tryGetWasmFrameTimings(): Promise<unknown | undefined> {
  const fn = getModuleExportFn<() => unknown | Promise<unknown>>(["get_frame_timings", "getFrameTimings"]);
  if (!fn) return undefined;
  try {
    const value = await fn();
    if (value == null) return undefined;
    return parseMaybeJson(value);
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

function shouldPollAerogpuWasmTelemetry(): boolean {
  // Only the wgpu-backed WebGL2 presenter uses `aero-gpu-wasm` for frame presentation.
  return presenter?.backend === "webgl2_wgpu";
}

async function tryDrainAerogpuWasmEvents(): Promise<GpuRuntimeErrorEvent[]> {
  if (!shouldPollAerogpuWasmTelemetry()) return [];
  try {
    const mod = await loadAerogpuWasm();
    const raw = await mod.drain_gpu_events();
    return normalizeGpuEventBatch(raw);
  } catch {
    return [];
  }
}

async function tryGetAerogpuWasmTelemetry(): Promise<unknown | undefined> {
  if (!shouldPollAerogpuWasmTelemetry()) return undefined;
  try {
    const mod = await loadAerogpuWasm();
    const stats = parseMaybeJson(await mod.get_gpu_stats());
    let frameTimings: unknown | undefined = undefined;
    try {
      const timings = mod.get_frame_timings();
      if (timings != null) frameTimings = timings;
    } catch {
      frameTimings = undefined;
    }
    return mergeWasmTelemetry(stats === undefined ? undefined : stats, frameTimings);
  } catch {
    return undefined;
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

    const aerogpuEvents = await tryDrainAerogpuWasmEvents();
    if (aerogpuEvents.length > 0) {
      postGpuEvents(aerogpuEvents);
      for (const ev of aerogpuEvents) {
        if (isDeviceLost) break;
        if (ev.category.toLowerCase() === "devicelost" && (ev.severity === "error" || ev.severity === "fatal")) {
          handleDeviceLost(ev.message, { source: "aero-gpu-wasm", event: ev }, true);
          break;
        }
      }
    }

    if (isDeviceLost) return;

    const wasmStats = mergeWasmTelemetry(await tryGetWasmStats(), await tryGetWasmFrameTimings());
    const aerogpuWasmTelemetry = await tryGetAerogpuWasmTelemetry();
    postStatsMessage(wasmStats ?? aerogpuWasmTelemetry);
  } finally {
    telemetryTickInFlight = false;
  }
}

function startTelemetryPolling(): void {
  if (telemetryPollTimer !== null) return;
  const timer = setInterval(() => void telemetryTick(), TELEMETRY_POLL_INTERVAL_MS) as unknown as number;
  (timer as unknown as { unref?: () => void }).unref?.();
  telemetryPollTimer = timer;
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

  // The wgpu WebGL2 presenter calls into aero-gpu-wasm and may clear the wasm D3D9 executor state
  // via `destroy_gpu()`. On device loss we conservatively invalidate our D3D9 tracking so the
  // next submission can re-initialize as needed.
  aerogpuWasmD3d9Backend = null;
  aerogpuWasmD3d9InternalCanvas = null;
  aerogpuWasmD3d9InitBackend = null;

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
  presenterNeedsFullUpload = true;

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

const estimateFullFrameUploadBytes = (width: number, height: number, bytesPerRowAlignment: number): number => {
  const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
  const bytesPerRow = bytesPerRowForUpload(rowBytes, height, bytesPerRowAlignment);
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
        aerogpuLastOutputSource = "framebuffer";
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
        presenterNeedsFullUpload = true;
      }

      const dirtyPresenter = presenter as Presenter & {
        presentDirtyRects?: (frame: number | ArrayBuffer | ArrayBufferView, stride: number, dirtyRects: DirtyRect[]) => void;
      };
      const needsFullUpload = presenterNeedsFullUpload || aerogpuLastOutputSource !== "framebuffer";
      if (needsFullUpload) {
        presenter.present(frame.pixels, frame.strideBytes);
        presenterNeedsFullUpload = false;
      } else if (dirtyRects && dirtyRects.length > 0 && typeof dirtyPresenter.presentDirtyRects === "function") {
        dirtyPresenter.presentDirtyRects(frame.pixels, frame.strideBytes, dirtyRects);
        lastUploadDirtyRects = dirtyRects;
      } else {
        presenter.present(frame.pixels, frame.strideBytes);
      }
      aerogpuLastOutputSource = "framebuffer";
      clearSharedFramebufferDirty();
      return true;
    }

    // Headless: treat as successfully presented so the shared frame state can
    // transition back to PRESENTED and avoid DIRTY→tick spam.
    aerogpuLastOutputSource = "framebuffer";
    clearSharedFramebufferDirty();
    return true;
  } finally {
    telemetry.recordPresentLatencyMs(performance.now() - t0);
  }
};

// -----------------------------------------------------------------------------
// AeroGPU command submissions (ACMD)
// -----------------------------------------------------------------------------

const presentAerogpuTexture = (tex: AeroGpuCpuTexture): void => {
  if (!presenter) return;

  if (tex.width !== presenterSrcWidth || tex.height !== presenterSrcHeight) {
    presenterSrcWidth = tex.width;
    presenterSrcHeight = tex.height;
    presenter.resize(tex.width, tex.height, outputDpr);
    presenterNeedsFullUpload = true;
  }

  aerogpuLastOutputSource = "aerogpu";
  presenter.present(tex.data, tex.width * 4);
  presenterNeedsFullUpload = false;
};

type AerogpuCmdStreamAnalysis = { vsyncPaced: boolean; presentCount: bigint; requiresD3d9: boolean };

const analyzeAerogpuCmdStream = (cmdStream: ArrayBuffer): AerogpuCmdStreamAnalysis => {
  try {
    const iter = new AerogpuCmdStreamIter(cmdStream);
    const dv = iter.view;
    let vsyncPaced = false;
    let presentCount = 0n;
    let requiresD3d9 = false;

    for (const packet of iter) {
      const opcode = packet.hdr.opcode;
      if (opcode === AerogpuCmdOpcode.Present || opcode === AerogpuCmdOpcode.PresentEx) {
        presentCount += 1n;
        // flags is always after the scanout_id field (hdr + scanout_id => offset + 12).
        if (packet.offsetBytes + 16 <= packet.endBytes) {
          const flags = dv.getUint32(packet.offsetBytes + 12, true);
          if ((flags & AEROGPU_PRESENT_FLAG_VSYNC) !== 0) vsyncPaced = true;
        }
      }

      if (requiresD3d9) continue;
      switch (opcode) {
        // Opcodes handled by the lightweight TypeScript CPU executor.
        case AerogpuCmdOpcode.CreateBuffer:
        case AerogpuCmdOpcode.CreateTexture2d:
        case AerogpuCmdOpcode.DestroyResource:
        case AerogpuCmdOpcode.UploadResource:
        case AerogpuCmdOpcode.ResourceDirtyRange:
        case AerogpuCmdOpcode.CopyBuffer:
        case AerogpuCmdOpcode.CopyTexture2d:
        case AerogpuCmdOpcode.SetRenderTargets:
        case AerogpuCmdOpcode.Present:
        case AerogpuCmdOpcode.PresentEx:
        case AerogpuCmdOpcode.Flush:
          break;
        default:
          requiresD3d9 = true;
          break;
      }
    }

    return { vsyncPaced, presentCount, requiresD3d9 };
  } catch {
    // Malformed streams should not gate completion on tick (avoid deadlocks) and should not
    // force a wasm executor path selection.
    return { vsyncPaced: false, presentCount: 0n, requiresD3d9: false };
  }
};
const handleSubmitAerogpu = async (req: GpuRuntimeSubmitAerogpuMessage): Promise<void> => {
  const signalFence = typeof req.signalFence === "bigint" ? req.signalFence : BigInt(req.signalFence);
  const cmdAnalysis = analyzeAerogpuCmdStream(req.cmdStream);
  const vsyncPaced = cmdAnalysis.vsyncPaced;
  const rawContextId = (req as unknown as { contextId?: unknown }).contextId;
  const contextId = typeof rawContextId === "number" && Number.isFinite(rawContextId) ? rawContextId >>> 0 : 0;
  const aerogpuState = getAerogpuContextState(contextId);
  const requiresD3d9 = cmdAnalysis.requiresD3d9;
  const contextPrefersWasmExecutor = aerogpuWasmExecutorContexts.has(contextId);

  let presentCount: bigint | undefined = undefined;
  let submitOk = false;
  try {
    await maybeSendReady();

    const runCpu = () => {
      const allocTable = req.allocTable ? decodeAerogpuAllocTable(req.allocTable) : null;
      const presentDelta = executeAerogpuCmdStream(aerogpuState, req.cmdStream, {
        allocTable,
        guestU8,
        presentTexture: presentAerogpuTexture,
      });
      if (presentDelta > 0n) {
        aerogpuPresentCount += presentDelta;
        presentCount = aerogpuPresentCount;
        if (aerogpuState.lastPresentedFrame) {
          aerogpuLastPresentedFrame = aerogpuState.lastPresentedFrame;
          aerogpuLastOutputSource = "aerogpu";
        }
      }
      submitOk = true;
    };

    const forcedBackend = runtimeOptions?.forceBackend;
    const forceRawBackend = forcedBackend === "webgl2_raw";
    const selectedBackend = presenter?.backend ?? forcedBackend;
    const isWgpuBackend = selectedBackend === "webgpu" || selectedBackend === "webgl2_wgpu";

    const shouldUseWasmExecutor = !forceRawBackend && (contextPrefersWasmExecutor || requiresD3d9 || isWgpuBackend);

    if (shouldUseWasmExecutor) {
      const disableWebGpu = runtimeOptions?.disableWebGpu === true;
      const preferWebGpu = runtimeOptions?.preferWebGpu !== false;
      const backend: PresenterBackendKind =
        selectedBackend === "webgpu" || selectedBackend === "webgl2_wgpu"
          ? selectedBackend
          : selectedBackend === "webgl2_raw"
            ? "webgl2_wgpu"
            : disableWebGpu || !preferWebGpu
              ? "webgl2_wgpu"
              : "webgpu";

      let wasm: AeroGpuWasmApi | null = null;
      try {
        wasm = await ensureAerogpuWasmD3d9(backend);
        if (!wasm.has_submit_aerogpu_d3d9()) {
          throw new Error("aero-gpu wasm export submit_aerogpu_d3d9 is missing (outdated bundle?)");
        }
      } catch (err) {
        // If the wasm executor is unavailable, fall back to the lightweight CPU executor so
        // test harnesses can keep running even when the bundle is missing or WebGPU is unavailable.
        emitGpuEvent({
          time_ms: performance.now(),
          backend_kind: backendKindForEvent(),
          severity: "warn",
          category: "AerogpuInit",
          message: err instanceof Error ? err.message : String(err),
          details: err instanceof Error ? { message: err.message, stack: err.stack } : String(err),
        });
        runCpu();
      }

      if (wasm) {
        if (guestU8) {
          wasm.set_guest_memory(guestU8);
        } else {
          wasm.clear_guest_memory();
        }

        const cmdU8 = new Uint8Array(req.cmdStream);
        const allocTableU8 = req.allocTable ? new Uint8Array(req.allocTable) : undefined;

        const wasmSubmitResult = await wasm.submit_aerogpu_d3d9(cmdU8, signalFence, contextId, allocTableU8);
        aerogpuWasmExecutorContexts.add(contextId);

        if (typeof wasmSubmitResult.presentCount === "bigint") {
          const nextWasmPresentCount = wasmSubmitResult.presentCount;
          const wasmDelta = nextWasmPresentCount >= aerogpuWasmPresentCount ? nextWasmPresentCount - aerogpuWasmPresentCount : nextWasmPresentCount;
          aerogpuWasmPresentCount = nextWasmPresentCount;
          aerogpuPresentCount += wasmDelta;
          presentCount = aerogpuPresentCount;
          const shot = await wasm.request_screenshot_info();
          const requiredShotBytes = shot.width * shot.height * BYTES_PER_PIXEL_RGBA8;
          if (shot.width > 0 && shot.height > 0 && shot.rgba8.byteLength >= requiredShotBytes) {
            const frame = { width: shot.width, height: shot.height, rgba8: shot.rgba8 };
            aerogpuState.lastPresentedFrame = frame;
            aerogpuLastPresentedFrame = frame;
            aerogpuLastOutputSource = "aerogpu";

            if (presenter) {
              if (shot.width !== presenterSrcWidth || shot.height !== presenterSrcHeight) {
                presenterSrcWidth = shot.width;
                presenterSrcHeight = shot.height;
                if (presenter.backend === "webgpu") surfaceReconfigures += 1;
                presenter.resize(shot.width, shot.height, outputDpr);
                presenterNeedsFullUpload = true;
              }
              presenter.present(shot.rgba8, shot.width * BYTES_PER_PIXEL_RGBA8);
              presenterNeedsFullUpload = false;
            }
          } else {
            aerogpuLastOutputSource = "aerogpu";
          }
        }

        submitOk = true;
      }
    } else {
      runCpu();
    }
  } catch (err) {
    sendError(err);
  }

  enqueueAerogpuSubmitComplete({
    requestId: req.requestId,
    completedFence: signalFence,
    ...(presentCount !== undefined ? { presentCount } : {}),
    kind: submitOk && vsyncPaced ? "vsync" : "immediate",
  });
};

const handleTick = async () => {
  syncPerfFrame();
  const perfEnabled = !!perfWriter && !!perfFrameHeader && Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
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
    const presentStartMs = perfEnabled ? performance.now() : 0;
    const didPresent = await presentOnce();
    const presentWallMs = perfEnabled ? performance.now() - presentStartMs : 0;

    let presentGpuMs = presentWallMs;
    if (presenter?.backend === "webgl2_wgpu" && didPresent) {
      const timings = await tryGetAeroGpuWasmFrameTimings();
      const gpuUs = timings?.gpu_us;
      if (typeof gpuUs === "number" && Number.isFinite(gpuUs)) {
        presentGpuMs = gpuUs / 1000;
      }
    }

    if (perfEnabled) perfGpuMs += presentGpuMs;
    if (didPresent) {
      presentsSucceeded += 1;
      framesPresented += 1;

      const now = performance.now();
      if (lastFrameStartMs !== null) {
        telemetry.beginFrame(lastFrameStartMs);

        const frame = getCurrentFrameInfo();
        const bytesPerRowAlignment = bytesPerRowAlignmentForPresenterBackend(presenter?.backend ?? null);
        const textureUploadBytes = frame?.sharedLayout
          ? estimateTextureUploadBytes(frame.sharedLayout, lastUploadDirtyRects, bytesPerRowAlignment)
          : frame
            ? estimateFullFrameUploadBytes(frame.width, frame.height, bytesPerRowAlignment)
            : 0;
        telemetry.recordTextureUploadBytes(textureUploadBytes);
        perf.counter("textureUploadBytes", textureUploadBytes);
        if (perfEnabled) perfUploadBytes += textureUploadBytes;
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
      // `WgpuWebGl2Presenter.init()` calls `aero-gpu-wasm.destroy_gpu()` to reset its own state.
      // That also clears the wasm D3D9 executor state.
      aerogpuWasmD3d9Backend = null;
      aerogpuWasmD3d9InternalCanvas = null;
      aerogpuWasmD3d9InitBackend = null;
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
  const prevPresenterBackend = presenter?.backend ?? null;
  presenter?.destroy?.();
  presenter = null;
  latestFrameTimings = null;
  presenterFallback = undefined;
  presenterErrorGeneration += 1;
  const generation = presenterErrorGeneration;

  if (prevPresenterBackend === "webgl2_wgpu") {
    // `WgpuWebGl2Presenter.destroy()` calls `aero-gpu-wasm.destroy_gpu()`, which clears both the
    // legacy presenter state and the D3D9 executor state.
    aerogpuWasmD3d9Backend = null;
    aerogpuWasmD3d9InternalCanvas = null;
    aerogpuWasmD3d9InitBackend = null;
  }

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
    backends = preferWebGpu ? ["webgpu", "webgl2_wgpu", "webgl2_raw"] : ["webgl2_wgpu", "webgl2_raw", "webgpu"];
    if (disableWebGpu && !preferWebGpu) {
      // When WebGPU is disabled and WebGL2 is preferred, never attempt WebGPU.
      backends = ["webgl2_wgpu", "webgl2_raw"];
    }
  }

  if (runtimeCanvasContextKind) {
    const filtered = backends.filter((backend) => {
      if (runtimeCanvasContextKind === "webgpu") return backend === "webgpu";
      return backend === "webgl2_raw" || backend === "webgl2_wgpu";
    });
    if (filtered.length === 0) {
      throw new PresenterError(
        "backend_incompatible",
        `Canvas already has a ${runtimeCanvasContextKind} context; cannot init backends [${backends.join(", ")}]`,
      );
    }
    backends = filtered;
  }

  const firstBackend = backends[0];
  let firstError: unknown | null = null;
  let lastError: unknown | null = null;

  for (const backend of backends) {
    try {
      presenter = await tryInitBackend(backend, canvas, width, height, dpr, opts, generation);
      presenterSrcWidth = width;
      presenterSrcHeight = height;
      presenterNeedsFullUpload = true;
      runtimeCanvasContextKind = presenter.backend === "webgpu" ? "webgpu" : "webgl2";
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

      // Warm up the wasm-backed AeroGPU D3D9 executor when we have a wgpu-based presenter so the
      // first submit_aerogpu doesn't pay the init cost.
      if (presenter.backend === "webgpu" || presenter.backend === "webgl2_wgpu") {
        void ensureAerogpuWasmD3d9(presenter.backend)
          .then((wasm) => {
            if (guestU8) {
              wasm.set_guest_memory(guestU8);
            } else {
              wasm.clear_guest_memory();
            }
          })
          .catch((err) => {
            emitGpuEvent({
              time_ms: performance.now(),
              backend_kind: backendKindForEvent(),
              severity: "warn",
              category: "AerogpuInit",
              message: err instanceof Error ? err.message : String(err),
              details: err instanceof Error ? { message: err.message, stack: err.stack } : String(err),
            });
          });
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

  const existingPresenter = presenter;
  if (existingPresenter) {
    runtimeReadySent = true;
    postToMain({ type: "ready", backendKind: existingPresenter.backend, fallback: presenterFallback });
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
  const readyPresenter: Presenter | null = presenter;
  if (!readyPresenter) return;

  runtimeReadySent = true;
  postToMain({ type: "ready", backendKind: readyPresenter.backend, fallback: presenterFallback });
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
  const views = createSharedMemoryViews(segments);
  status = views.status;
  // Guest physical addresses (GPAs) in AeroGPU submissions are byte offsets into this view.
  guestU8 = views.guestU8;
  if (aerogpuWasm) {
    try {
      // If aero-gpu-wasm is already loaded (e.g. via the webgl2_wgpu presenter), plumb the
      // shared guest RAM view immediately so alloc_table submissions can resolve GPAs.
      aerogpuWasm.set_guest_memory(guestU8);
    } catch {
      // Ignore; wasm module may not have been initialized yet.
    }
  }

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
  const timer = setInterval(() => {
    drainRuntimeCommands();
    if (status && Atomics.load(status, StatusIndex.StopRequested) === 1) {
      shutdownRuntime();
    }
  }, 8) as unknown as number;
  (timer as unknown as { unref?: () => void }).unref?.();
  runtimePollTimer = timer;
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

  if (!isGpuWorkerMessageBase(data) || typeof (data as { type?: unknown }).type !== "string") return;
  const msg = data as GpuRuntimeInMessage;

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
        const nextCanvas = init.canvas ?? null;
        if (runtimeCanvas !== nextCanvas) {
          runtimeCanvasContextKind = null;
        }
        runtimeCanvas = nextCanvas;
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
        latestFrameTimings = null;
        presenterFallback = undefined;
        presenterInitPromise = null;
        presenterSrcWidth = 0;
        presenterSrcHeight = 0;
        presenterNeedsFullUpload = true;

        resetAerogpuContexts();
        aerogpuLastOutputSource = "framebuffer";
        // Reset wasm-backed executor state (if it was used previously).
        aerogpuWasmD3d9InitPromise = null;
        aerogpuWasmD3d9InitBackend = null;
        aerogpuWasmD3d9Backend = null;
        aerogpuWasmD3d9InternalCanvas = null;
        if (aerogpuWasm) {
          try {
            aerogpuWasm.clear_guest_memory();
          } catch {
            // Ignore; best-effort cleanup.
          }
          try {
            aerogpuWasm.destroy_gpu();
          } catch {
            // Ignore; wasm module may not be initialized.
          }
        }

        // Headless mode: if the caller explicitly forced a wgpu-based backend, pre-initialize the
        // wasm D3D9 executor immediately so headless AeroGPU submissions can run without waiting
        // for a presenter surface.
        const forcedBackend = runtimeOptions?.forceBackend;
        if (!runtimeCanvas && (forcedBackend === "webgpu" || forcedBackend === "webgl2_wgpu")) {
          void ensureAerogpuWasmD3d9(forcedBackend)
            .then((wasm) => {
              if (guestU8) {
                wasm.set_guest_memory(guestU8);
              } else {
                wasm.clear_guest_memory();
              }
            })
            .catch((err) => {
              emitGpuEvent({
                time_ms: performance.now(),
                backend_kind: backendKindForEvent(),
                severity: "warn",
                category: "AerogpuInit",
                message: err instanceof Error ? err.message : String(err),
                details: err instanceof Error ? { message: err.message, stack: err.stack } : String(err),
              });
            });
        }
        aerogpuSubmitChain = Promise.resolve();
        aerogpuPendingSubmitComplete.length = 0;
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
        const wasmModuleUrl = runtimeOptions?.wasmModuleUrl;
        if (wasmModuleUrl) {
          wasmInitPromise = perf
            .spanAsync("wasm:init", () => loadPresentFnFromModuleUrl(wasmModuleUrl))
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
      flushAerogpuSubmitCompleteOnTick();
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
          if (isDeviceLost && recoveryPromise) {
            // If a recovery attempt is already underway, wait briefly so the screenshot
            // can capture real pixels instead of immediately returning the 1x1 stub.
            // (Still bounded so the API cannot hang indefinitely.)
            await Promise.race([
              recoveryPromise,
              new Promise((resolve) => setTimeout(resolve, 750)),
            ]);
            await maybeSendReady();
          }
          const includeCursor = req.includeCursor === true;

          // Ensure the screenshot reflects the latest presented pixels. The shared
          // framebuffer producer can advance `frameSeq` before the presenter runs,
          // so relying on the header sequence alone can lead to mismatched
          // (seq, pixels) pairs in smoke tests and automation.
          if (frameState) {
            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }

            if (!isDeviceLost && aerogpuLastOutputSource === "framebuffer" && shouldPresentWithSharedState()) {
              await handleTick();
            }

            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }
          }

          const seq = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;

          const tryPostWebgl2WgpuFramebufferScreenshot = (): boolean => {
            if (!presenter || presenter.backend !== "webgl2_wgpu") return false;
            if (aerogpuLastOutputSource !== "framebuffer") return false;
            refreshFramebufferViews();

            let width = 0;
            let height = 0;
            let strideBytes = 0;
            let pixels: Uint8Array | null = null;
            let frameSeq: number | null = typeof seq === "number" ? seq : null;

            if (sharedFramebufferViews) {
              const layout = sharedFramebufferViews.layout;
              width = layout.width;
              height = layout.height;
              strideBytes = layout.strideBytes;
              const header = sharedFramebufferViews.header;

              const activeIndex = Atomics.load(header, SharedFramebufferHeaderIndex.ACTIVE_INDEX) & 1;
              const activePixels = activeIndex === 0 ? sharedFramebufferViews.slot0 : sharedFramebufferViews.slot1;

              const desiredSeq = frameSeq;
              if (desiredSeq != null) {
                const buf0Seq = Atomics.load(header, SharedFramebufferHeaderIndex.BUF0_FRAME_SEQ);
                const buf1Seq = Atomics.load(header, SharedFramebufferHeaderIndex.BUF1_FRAME_SEQ);
                if (buf0Seq === desiredSeq) {
                  pixels = sharedFramebufferViews.slot0;
                } else if (buf1Seq === desiredSeq) {
                  pixels = sharedFramebufferViews.slot1;
                } else {
                  pixels = activePixels;
                  frameSeq = activeIndex === 0 ? buf0Seq : buf1Seq;
                }
              } else {
                pixels = activePixels;
                frameSeq = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
              }
            } else {
              const frame = getCurrentFrameInfo();
              if (!frame) return false;
              width = frame.width;
              height = frame.height;
              strideBytes = frame.strideBytes;
              pixels = frame.pixels;
              if (frameSeq == null) frameSeq = frame.frameSeq;
            }

            if (!pixels) return false;

            const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
            const out = new Uint8Array(rowBytes * height);
            for (let y = 0; y < height; y += 1) {
              const srcStart = y * strideBytes;
              const dstStart = y * rowBytes;
              out.set(pixels.subarray(srcStart, srcStart + rowBytes), dstStart);
            }

            if (includeCursor) {
              try {
                compositeCursorOverRgba8(
                  out,
                  width,
                  height,
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
                width,
                height,
                rgba8: out.buffer,
                origin: "top-left",
                ...(typeof frameSeq === "number" ? { frameSeq } : {}),
              },
              [out.buffer],
            );
            return true;
          };

          if (tryPostWebgl2WgpuFramebufferScreenshot()) return;

          const tryPostPresenterScreenshot = async (forceUpload: boolean): Promise<boolean> => {
            if (!presenter || isDeviceLost) return false;
            if (forceUpload) {
              if (aerogpuLastOutputSource === "aerogpu") {
                const last = aerogpuLastPresentedFrame;
                if (last) {
                  if (last.width !== presenterSrcWidth || last.height !== presenterSrcHeight) {
                    presenterSrcWidth = last.width;
                    presenterSrcHeight = last.height;
                    if (presenter.backend === "webgpu") surfaceReconfigures += 1;
                    presenter.resize(last.width, last.height, outputDpr);
                    presenterNeedsFullUpload = true;
                  }
                  presenter.present(last.rgba8, last.width * BYTES_PER_PIXEL_RGBA8);
                  presenterNeedsFullUpload = false;
                }
              } else {
                const frame = getCurrentFrameInfo();
                if (frame) {
                  if (frame.width !== presenterSrcWidth || frame.height !== presenterSrcHeight) {
                    presenterSrcWidth = frame.width;
                    presenterSrcHeight = frame.height;
                    if (presenter.backend === "webgpu") surfaceReconfigures += 1;
                    presenter.resize(frame.width, frame.height, outputDpr);
                    presenterNeedsFullUpload = true;
                  }
                  presenter.present(frame.pixels, frame.strideBytes);
                  aerogpuLastOutputSource = "framebuffer";
                  presenterNeedsFullUpload = false;
                }
              }
            }

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

              if (includeCursor && presenter.backend === "webgl2_wgpu") {
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
              return true;
            } finally {
              if (needsCursorDisabledForScreenshot) {
                cursorRenderEnabled = prevCursorRenderEnabled;
                syncCursorToPresenter();
                getCursorPresenter()?.redraw?.();
              }
            }
          };

          const tryScreenshot = async (forceUpload: boolean): Promise<boolean> => {
            try {
              return await tryPostPresenterScreenshot(forceUpload);
            } catch (err) {
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
              return false;
            }
          };

          // Fast path: capture immediately when the presenter is healthy.
          if (await tryScreenshot(false)) return;

          // If we raced with a device-loss + recovery cycle, wait for recovery and retry once.
          if ((isDeviceLost || !presenter) && recoveryPromise) {
            await Promise.race([
              recoveryPromise,
              new Promise((resolve) => setTimeout(resolve, 1500)),
            ]);
            await maybeSendReady();
            if (await tryScreenshot(true)) return;
          }

          const last = aerogpuLastPresentedFrame;
          if (last) {
            const out = last.rgba8.slice(0);
            if (includeCursor) {
              try {
                compositeCursorOverRgba8(
                  new Uint8Array(out),
                  last.width,
                  last.height,
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
      resetAerogpuContexts();
      aerogpuLastOutputSource = "framebuffer";
      aerogpuWasmD3d9InitPromise = null;
      aerogpuWasmD3d9InitBackend = null;
      aerogpuWasmD3d9Backend = null;
      aerogpuWasmD3d9InternalCanvas = null;
      presenterNeedsFullUpload = true;
      if (aerogpuWasm) {
        try {
          aerogpuWasm.clear_guest_memory();
        } catch {
          // Ignore; best-effort cleanup.
        }
        try {
          aerogpuWasm.destroy_gpu();
        } catch {
          // Ignore.
        }
      }
      aerogpuPendingSubmitComplete.length = 0;
      ctx.close();
      break;
    }
  }
};

void currentConfig;
