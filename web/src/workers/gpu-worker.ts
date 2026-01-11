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
  GpuRuntimeInMessage,
  GpuRuntimeFallbackInfo,
  GpuRuntimeInitMessage,
  GpuRuntimeInitOptions,
  GpuRuntimeOutMessage,
  GpuRuntimeScreenshotRequestMessage,
  GpuRuntimeSubmitAerogpuMessage,
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
let presenting = false;

let runtimeInit: GpuRuntimeInitMessage | null = null;
let runtimeCanvas: OffscreenCanvas | null = null;
let runtimeOptions: GpuRuntimeInitOptions | null = null;
let runtimeReadySent = false;

let outputWidthCss: number | null = null;
let outputHeightCss: number | null = null;
let outputDpr = 1;

let presenter: Presenter | null = null;
let presenterInitOptions: PresenterInitOptions | null = null;
let presenterUserOnError: ((error: PresenterError) => void) | undefined = undefined;
let presenterFallback: GpuRuntimeFallbackInfo | undefined = undefined;
let presenterInitPromise: Promise<void> | null = null;
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

const sendError = (err: unknown) => {
  if (err instanceof PresenterError) {
    postToMain({ type: "error", message: err.message, code: err.code, backend: presenter?.backend });
    postRuntimeError(err.message);
    return;
  }

  const message = err instanceof Error ? err.message : String(err);
  postToMain({ type: "error", message, backend: presenter?.backend });
  postRuntimeError(message);
};

const loadPresentFnFromModuleUrl = async (wasmModuleUrl: string) => {
  const mod: unknown = await import(/* @vite-ignore */ wasmModuleUrl);

  const maybePresent = (mod as { present?: unknown }).present;
  if (typeof maybePresent !== 'function') {
    throw new Error(`Module ${wasmModuleUrl} did not export a present() function`);
  }
  presentFn = maybePresent as PresentFn;
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
    const dirtyRects = dirtyWords ? dirtyTilesToRects(sharedFramebufferViews.layout, dirtyWords) : null;
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

  try {
    const frame = getCurrentFrameInfo();
    const dirtyRects = frame?.dirtyRects ?? null;

    if (presentFn) {
      const result = await presentFn(dirtyRects);
      return typeof result === "boolean" ? result : true;
    }

    if (presenter) {
      if (!frame) return false;

      if (frame.width !== presenterSrcWidth || frame.height !== presenterSrcHeight) {
        presenterSrcWidth = frame.width;
        presenterSrcHeight = frame.height;
        presenter.resize(frame.width, frame.height, outputDpr);
      }

      presenter.present(frame.pixels, frame.strideBytes);
      return true;
    }

    // Headless: treat as successfully presented so the shared frame state can
    // transition back to PRESENTED and avoid DIRTY→tick spam.
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
    const presentStartMs = performance.now();
        const didPresent = await presentOnce();
        perfGpuMs += performance.now() - presentStartMs;
        if (didPresent) {
          framesPresented += 1;

      const now = performance.now();
      if (lastFrameStartMs !== null) {
        telemetry.beginFrame(lastFrameStartMs);

        const frame = getCurrentFrameInfo();
        const textureUploadBytes = frame?.sharedLayout
          ? estimateTextureUploadBytes(frame.sharedLayout, frame.dirtyRects ?? null)
          : frame
            ? estimateFullFrameUploadBytes(frame.width, frame.height)
            : 0;
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
// Presenter backend init (OffscreenCanvas path)
// -----------------------------------------------------------------------------

function postPresenterError(err: unknown, backend?: PresenterBackendKind): void {
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
): Promise<Presenter> {
  if (backend === "webgpu" && runtimeOptions?.disableWebGpu === true) {
    throw new PresenterError("webgpu_disabled", "WebGPU backend was disabled by init options");
  }

  // Ensure backend errors are surfaced even if the caller didn't pass an onError.
  opts.onError = (e) => {
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
      presenter = await tryInitBackend(backend, canvas, width, height, dpr, opts);
      presenterSrcWidth = width;
      presenterSrcHeight = height;

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
        runtimeInit = init;
        runtimeCanvas = init.canvas ?? null;
        runtimeOptions = init.options ?? null;
        runtimeReadySent = false;

        outputWidthCss = runtimeOptions?.outputWidth ?? null;
        outputHeightCss = runtimeOptions?.outputHeight ?? null;
        outputDpr = runtimeOptions?.dpr ?? 1;

        frameState = new Int32Array(init.sharedFrameState);

        framesReceived = 0;
        framesPresented = 0;
        framesDropped = 0;
        lastSeenSeq = Atomics.load(frameState, FRAME_SEQ_INDEX);
        lastPresentedSeq = lastSeenSeq;

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

        presenterUserOnError = runtimeOptions?.presenter?.onError;
        presenterInitOptions = { ...(runtimeOptions?.presenter ?? {}) };
        // Backend init installs its own error handler wrapper.
        presenterInitOptions.onError = undefined;

        presentFn = null;
        if (runtimeOptions?.wasmModuleUrl) {
          void perf.spanAsync("wasm:init", () => loadPresentFnFromModuleUrl(runtimeOptions.wasmModuleUrl!)).catch(sendError);
        }

        refreshFramebufferViews();
        void maybeSendReady();
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
        try {
          await maybeSendReady();

          // Ensure the screenshot reflects the latest presented pixels. The shared
          // framebuffer producer can advance `frameSeq` before the presenter runs,
          // so relying on the header sequence alone can lead to mismatched
          // (seq, pixels) pairs in smoke tests and automation.
          if (frameState) {
            while (presenting) {
              await new Promise((resolve) => setTimeout(resolve, 0));
            }

            if (shouldPresentWithSharedState()) {
              await handleTick();
            }

            while (presenting) {
              await new Promise((resolve) => setTimeout(resolve, 0));
            }
          }

          const seq = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;

          if (presenter) {
            const shot = await presenter.screenshot();
            postToMain(
              {
                type: "screenshot",
                requestId: req.requestId,
                width: shot.width,
                height: shot.height,
                rgba8: shot.pixels,
                origin: "top-left",
                ...(typeof seq === "number" ? { frameSeq: seq } : {}),
              },
              [shot.pixels],
            );
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

          // Headless fallback: copy the source buffer directly.
          const frame = getCurrentFrameInfo();
          if (!frame) throw new PresenterError("not_initialized", "screenshot before framebuffer init");
          const rowBytes = frame.width * BYTES_PER_PIXEL_RGBA8;
          const out = new Uint8Array(rowBytes * frame.height);
          for (let y = 0; y < frame.height; y += 1) {
            const srcStart = y * frame.strideBytes;
            const dstStart = y * rowBytes;
            out.set(frame.pixels.subarray(srcStart, srcStart + rowBytes), dstStart);
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
        } catch (err) {
          postPresenterError(err, presenter?.backend);
        }
      })();
      break;
    }

    case "shutdown": {
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
