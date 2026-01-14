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

import "../../gpu-cache/persistent_cache.ts";

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
  type GpuRuntimeScreenshotPresentedRequestMessage,
  type GpuRuntimeSubmitAerogpuMessage,
  type GpuRuntimeOutputSource,
  type GpuRuntimePresentUploadV1,
  type GpuRuntimeScanoutSnapshotV1,
  type GpuRuntimeStatsCountersV1,
} from "../ipc/gpu-protocol";

import { linearizeSrgbRgba8InPlace } from "../utils/srgb";

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
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_B8G8R8A8_SRGB,
  SCANOUT_FORMAT_B8G8R8X8_SRGB,
  SCANOUT_FORMAT_B5G5R5A1,
  SCANOUT_FORMAT_B5G6R5,
  SCANOUT_FORMAT_R8G8B8A8,
  SCANOUT_FORMAT_R8G8B8X8,
  SCANOUT_FORMAT_R8G8B8A8_SRGB,
  SCANOUT_FORMAT_R8G8B8X8_SRGB,
  SCANOUT_SOURCE_LEGACY_VBE_LFB,
  SCANOUT_SOURCE_WDDM,
  ScanoutStateIndex,
  trySnapshotScanoutState as trySnapshotScanoutStateBounded,
  type ScanoutStateSnapshot,
} from "../ipc/scanout_state";
import {
  CURSOR_FORMAT_B8G8R8A8,
  CURSOR_FORMAT_B8G8R8X8,
  CURSOR_FORMAT_R8G8B8A8,
  CURSOR_FORMAT_R8G8B8X8,
  CURSOR_FORMAT_B8G8R8A8_SRGB,
  CURSOR_FORMAT_B8G8R8X8_SRGB,
  CURSOR_FORMAT_R8G8B8A8_SRGB,
  CURSOR_FORMAT_R8G8B8X8_SRGB,
  trySnapshotCursorState as trySnapshotCursorStateBounded,
  type CursorStateSnapshot,
} from "../ipc/cursor_state";

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
import {
  chooseDirtyRectsForUpload,
  estimateFullFrameUploadBytes,
  estimateTextureUploadBytes,
} from "../gpu/dirty-rect-policy";
import type { AeroConfig } from '../config/aero_config';
import { VRAM_BASE_PADDR } from '../arch/guest_phys.ts';
import {
  createSharedMemoryViews,
  ringRegionsForWorker,
  setReadyFlag,
  StatusIndex,
  type GuestRamLayout,
  type WorkerRole,
} from '../runtime/shared_layout';
import {
  MAX_SCANOUT_RGBA8_BYTES,
  readScanoutRgba8FromGuestRam,
  tryComputeScanoutRgba8ByteLength,
} from "../runtime/scanout_readback";
import { RingBuffer } from '../ipc/ring_buffer';
import { decodeCommand, encodeEvent, type Command, type Event } from '../ipc/protocol';
import {
  guestPaddrToRamOffset as guestPaddrToRamOffsetRaw,
  guestRangeInBounds as guestRangeInBoundsRaw,
} from "../arch/guest_ram_translate.ts";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import {
  type CoordinatorToWorkerSnapshotMessage,
  type VmSnapshotPausedMessage,
  type VmSnapshotResumedMessage,
  serializeVmSnapshotError,
} from "../runtime/snapshot_protocol";

import type { Presenter, PresenterBackendKind, PresenterInitOptions } from "../gpu/presenter";
import { PresenterError } from "../gpu/presenter";
import { RawWebGl2Presenter } from "../gpu/raw-webgl2-presenter-backend";
import { didPresenterPresent, presentOutcomeDeltas } from "./present-outcome.ts";
import {
  createAerogpuCpuExecutorState,
  decodeAerogpuAllocTable,
  aerogpuCpuExecutorSupportsOpcode,
  executeAerogpuCmdStream,
  resetAerogpuCpuExecutorState,
  type AeroGpuCpuTexture,
  type AerogpuCpuExecutorState,
} from "./aerogpu-acmd-executor.ts";
import { AerogpuCmdStreamIter } from "../../../emulator/protocol/aerogpu/aerogpu_cmd.ts";
import { AerogpuFormat, aerogpuFormatToString } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";

import { convertScanoutToRgba8, type ScanoutSwizzleKind } from "./scanout_swizzle.ts";

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

function toTransferableArrayBuffer(view: Uint8Array): ArrayBuffer {
  // `postMessage(..., [buf])` only accepts transferable `ArrayBuffer`s (not `SharedArrayBuffer`),
  // and screenshot/cursor protocols require a tight-packed backing store.
  const bufLike = view.buffer;
  if (bufLike instanceof ArrayBuffer && view.byteOffset === 0 && view.byteLength === bufLike.byteLength) {
    return bufLike;
  }
  const buf = new ArrayBuffer(view.byteLength);
  new Uint8Array(buf).set(view);
  return buf;
}

const postRuntimeError = (message: string) => {
  if (!status) return;
  pushRuntimeEvent({ kind: 'log', level: 'error', message });
  ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
};

let role: WorkerRole = "gpu";
let status: Int32Array | null = null;
let guestU8: Uint8Array | null = null;
let guestU32: Uint32Array | null = null;
let guestLayout: GuestRamLayout | null = null;
let vramU8: Uint8Array | null = null;
let vramU32: Uint32Array | null = null;
let vramBasePaddr = 0;
let vramSizeBytes = 0;
let vramMissingScanoutErrorSent = false;

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

let scanoutState: Int32Array | null = null;
let wddmScanoutRgba: Uint8Array | null = null;
let wddmScanoutWidth = 0;
let wddmScanoutHeight = 0;
let wddmScanoutFormat: number | null = null;
// Compatibility fallback for harnesses that do not provide `scanoutState`.
//
// When set, treat WDDM/AeroGPU as owning scanout so legacy shared-framebuffer updates do not
// "flash back" over WDDM output.
let wddmOwnsScanoutFallback = false;
let hwCursorState: Int32Array | null = null;

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
let recoveriesAttemptedWddm = 0;
let recoveriesSucceededWddm = 0;
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
let presenterErrorEventGeneration = -1;
const presenterErrorEventKeys = new Set<string>();
let presenterInitBackendHint: PresenterBackendKind | null = null;
let presenterInitBackendHintGeneration = 0;
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
// Tracks which source was last uploaded to the presenter. Despite the name, this is used for
// framebuffer + AeroGPU + WDDM scanout output selection (cursor redraw + screenshot force uploads).
let aerogpuLastOutputSource: GpuRuntimeOutputSource = "framebuffer";

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

const extractVramU8FromWorkerInit = (init: WorkerInitMessage): Uint8Array | null => {
  // The VRAM aperture is optional and may be supplied under different field names depending on the
  // embedding environment / runtime version. Accept either:
  // - a `Uint8Array` view directly, or
  // - an `ArrayBuffer`/`SharedArrayBuffer` plus optional offset/length metadata.
  const rec = init as unknown as Record<string, unknown>;
  const raw =
    rec["vramU8"] ??
    rec["vramMemoryU8"] ??
    rec["vramMemory"] ??
    rec["vram"] ??
    rec["aerogpuVram"] ??
    rec["aerogpuVramMemory"] ??
    null;

  if (raw instanceof Uint8Array) return raw;

  if (!(raw instanceof ArrayBuffer) && !(raw instanceof SharedArrayBuffer)) return null;
  const buf = raw;

  const offsetRaw = rec["vramOffsetBytes"] ?? rec["vramMemoryOffsetBytes"] ?? 0;
  const lengthRaw = rec["vramByteLength"] ?? rec["vramSizeBytes"] ?? rec["vramMemoryByteLength"] ?? null;

  const offset = typeof offsetRaw === "number" && Number.isFinite(offsetRaw) ? Math.max(0, Math.trunc(offsetRaw)) : 0;
  const maxLen = buf.byteLength - offset;
  const length =
    typeof lengthRaw === "number" && Number.isFinite(lengthRaw)
      ? Math.max(0, Math.min(maxLen, Math.trunc(lengthRaw)))
      : maxLen;

  if (offset < 0 || length < 0 || offset + length > buf.byteLength) {
    return new Uint8Array(buf);
  }
  return new Uint8Array(buf, offset, length);
};

const syncAerogpuWasmMemoryViews = (wasm: AeroGpuWasmApi): void => {
  if (guestU8) wasm.set_guest_memory(guestU8);
  else wasm.clear_guest_memory();

  if (vramU8) wasm.set_vram_memory(vramU8);
  else wasm.clear_vram_memory();
};

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
let aerogpuSubmitInFlight: Promise<void> | null = null;

type PendingAerogpuSubmitComplete = {
  requestId: number;
  completedFence: bigint;
  presentCount?: bigint;
};

const postAerogpuSubmitComplete = (entry: PendingAerogpuSubmitComplete): void => {
  postToMain({
    type: "submit_complete",
    requestId: entry.requestId,
    completedFence: entry.completedFence,
    ...(entry.presentCount !== undefined ? { presentCount: entry.presentCount } : {}),
  });
};

let framesReceived = 0;
let framesPresented = 0;
let framesDropped = 0;

let lastSeenSeq = 0;
let lastPresentedSeq = 0;
let lastUploadDirtyRects: DirtyRect[] | null = null;
let lastPresentUploadKind: GpuRuntimePresentUploadV1["kind"] = "none";
let lastPresentUploadDirtyRectCount = 0;

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

// Track what cursor image has been uploaded to the current presenter so cursor *position* updates
// don't repeatedly re-upload the texture.
let cursorPresenterLastImageOwner: CursorPresenter | null = null;
let cursorPresenterLastImage: Uint8Array | null = null;
let cursorPresenterLastImageWidth = 0;
let cursorPresenterLastImageHeight = 0;

// -----------------------------------------------------------------------------
// Hardware cursor (CursorState descriptor) tracking.
// -----------------------------------------------------------------------------

// The legacy cursor APIs (cursor_set_image/state) are still supported for harnesses and
// pre-WDDM demos. The hardware cursor path becomes authoritative once we observe a
// non-default CursorState publish (generation != 0 or any non-zero fields).
let hwCursorActive = false;
let hwCursorLastGeneration: number | null = null;
let hwCursorLastImageKey: string | null = null;
let hwCursorLastVramMissingEventKey: string | null = null;

const getCursorPresenter = (): CursorPresenter | null => presenter as unknown as CursorPresenter | null;

// -----------------------------------------------------------------------------
// VM snapshot pause/resume (guest-memory access barrier)
// -----------------------------------------------------------------------------

let snapshotPaused = false;
let snapshotResumePromise: Promise<void> | null = null;
let snapshotResumeResolve: (() => void) | null = null;

let snapshotPausePromise: Promise<void> | null = null;
let snapshotPauseEpoch = 0;

// Tracks async tasks that may touch guest RAM/VRAM/shared state and therefore must fully complete
// before we ACK `vm.snapshot.paused`.
//
// Includes:
// - tick/present work (`handleTick()`)
// - screenshot requests (which can force a tick/present and/or read scanout/cursor state)
let snapshotBarrierInFlightCount = 0;
let snapshotBarrierInFlightPromise: Promise<void> | null = null;
let snapshotBarrierInFlightResolve: (() => void) | null = null;

const beginSnapshotBarrierTask = (): void => {
  snapshotBarrierInFlightCount += 1;
};

const endSnapshotBarrierTask = (): void => {
  snapshotBarrierInFlightCount -= 1;
  if (snapshotBarrierInFlightCount <= 0) {
    snapshotBarrierInFlightCount = 0;
    snapshotBarrierInFlightResolve?.();
    snapshotBarrierInFlightPromise = null;
    snapshotBarrierInFlightResolve = null;
  }
};

type SnapshotGuestMemoryBackup = {
  guestU8: Uint8Array | null;
  guestU32: Uint32Array | null;
  vramU8: Uint8Array | null;
  vramU32: Uint32Array | null;
  scanoutState: Int32Array | null;
  hwCursorState: Int32Array | null;
  sharedFramebufferViews: SharedFramebufferViews | null;
  sharedFramebufferLayoutKey: string | null;
  framebufferProtocolViews: FramebufferProtocolViews | null;
  framebufferProtocolLayoutKey: string | null;
};

// When snapshot-paused, we must not touch guest RAM/VRAM. To enforce this across all
// code paths (present/tick/screenshot/etc), we temporarily clear the worker's guest
// memory views and restore them on resume.
let snapshotGuestMemoryBackup: SnapshotGuestMemoryBackup | null = null;

const disableGuestMemoryAccessForSnapshot = (): void => {
  if (snapshotGuestMemoryBackup) return;
  snapshotGuestMemoryBackup = {
    guestU8,
    guestU32,
    vramU8,
    vramU32,
    scanoutState,
    hwCursorState,
    sharedFramebufferViews,
    sharedFramebufferLayoutKey,
    framebufferProtocolViews,
    framebufferProtocolLayoutKey,
  };
  guestU8 = null;
  guestU32 = null;
  vramU8 = null;
  vramU32 = null;
  scanoutState = null;
  (globalThis as unknown as { __aeroScanoutState?: Int32Array }).__aeroScanoutState = undefined;
  hwCursorState = null;
  (globalThis as unknown as { __aeroCursorState?: Int32Array }).__aeroCursorState = undefined;
  sharedFramebufferViews = null;
  sharedFramebufferLayoutKey = null;
  framebufferProtocolViews = null;
  framebufferProtocolLayoutKey = null;
  (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer = undefined;
  if (aerogpuWasm) {
    try {
      syncAerogpuWasmMemoryViews(aerogpuWasm);
    } catch {
      // Ignore; wasm module may not be initialized.
    }
  }
};

const restoreGuestMemoryAccessAfterSnapshot = (): void => {
  const backup = snapshotGuestMemoryBackup;
  if (!backup) return;
  snapshotGuestMemoryBackup = null;
  guestU8 = backup.guestU8;
  guestU32 = backup.guestU32;
  vramU8 = backup.vramU8;
  vramU32 = backup.vramU32;
  scanoutState = backup.scanoutState;
  (globalThis as unknown as { __aeroScanoutState?: Int32Array }).__aeroScanoutState = scanoutState ?? undefined;
  hwCursorState = backup.hwCursorState;
  (globalThis as unknown as { __aeroCursorState?: Int32Array }).__aeroCursorState = hwCursorState ?? undefined;
  sharedFramebufferViews = backup.sharedFramebufferViews;
  sharedFramebufferLayoutKey = backup.sharedFramebufferLayoutKey;
  framebufferProtocolViews = backup.framebufferProtocolViews;
  framebufferProtocolLayoutKey = backup.framebufferProtocolLayoutKey;
  (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer =
    sharedFramebufferViews ?? undefined;
  if (aerogpuWasm) {
    try {
      syncAerogpuWasmMemoryViews(aerogpuWasm);
    } catch {
      // Ignore; wasm module may not be initialized.
    }
  }
};

const waitUntilSnapshotResumed = async (): Promise<void> => {
  while (snapshotPaused) {
    if (!snapshotResumePromise) {
      snapshotResumePromise = new Promise<void>((resolve) => {
        snapshotResumeResolve = resolve;
      });
    }
    await snapshotResumePromise;
  }
};

const handleSnapshotResume = (): void => {
  // If a snapshot pause attempt is in flight (waiting for an async present/submission), a
  // resume can arrive before the worker has ACKed `vm.snapshot.paused` (e.g. coordinator timeout
  // + best-effort resume). Bump the epoch + clear the pause promise so any in-progress pause
  // handler can observe cancellation and avoid disabling guest memory after we've resumed.
  snapshotPauseEpoch += 1;
  snapshotPausePromise = null;
  restoreGuestMemoryAccessAfterSnapshot();
  snapshotPaused = false;
  snapshotResumeResolve?.();
  snapshotResumePromise = null;
  snapshotResumeResolve = null;
};

const ensureSnapshotPaused = async (): Promise<void> => {
  // Once guest-memory access is disabled, the snapshot pause barrier is fully established.
  // Any subsequent pause requests can acknowledge immediately without waiting on work that is
  // blocked by snapshot pause (e.g. submit_aerogpu tasks gated on `waitUntilSnapshotResumed()`).
  if (snapshotGuestMemoryBackup) return;
  if (snapshotPausePromise) return snapshotPausePromise;

  snapshotPaused = true;
  const pauseEpoch = snapshotPauseEpoch;
  const inFlightSubmit = aerogpuSubmitInFlight;
  const promise = (async () => {
    if (inFlightSubmit) {
      // Best-effort: wait for any in-progress ACMD submission to complete so we don't race
      // snapshot save/restore with guest-memory reads/writes.
      await Promise.race([inFlightSubmit.catch(() => {}), waitUntilSnapshotResumed()]);
    }

    if (!snapshotPaused || snapshotPauseEpoch !== pauseEpoch) {
      throw new Error("Snapshot pause canceled by resume.");
    }

    // Also wait for any in-flight tick/present work to finish; ticks are spawned as "fire and
    // forget" tasks, so we must explicitly track/drain them before acknowledging snapshot pause.
    while (snapshotBarrierInFlightCount > 0) {
      if (!snapshotPaused || snapshotPauseEpoch !== pauseEpoch) {
        throw new Error("Snapshot pause canceled by resume.");
      }
      if (!snapshotBarrierInFlightPromise) {
        snapshotBarrierInFlightPromise = new Promise<void>((resolve) => {
          snapshotBarrierInFlightResolve = resolve;
        });
      }
      // Allow snapshot resume to cancel the pause attempt even if a tick/present is hung.
      await Promise.race([snapshotBarrierInFlightPromise, waitUntilSnapshotResumed()]);
    }

    if (!snapshotPaused || snapshotPauseEpoch !== pauseEpoch) {
      throw new Error("Snapshot pause canceled by resume.");
    }
    // Once paused, prevent *any* guest RAM/VRAM access (including scanout/cursor readback)
    // until the coordinator resumes the worker.
    disableGuestMemoryAccessForSnapshot();
  })();
  snapshotPausePromise = promise;
  promise.then(
    () => {
      if (snapshotPausePromise === promise) snapshotPausePromise = null;
    },
    () => {
      if (snapshotPausePromise === promise) snapshotPausePromise = null;
    },
  );
  return promise;
};

const syncCursorToPresenter = (): void => {
  const p = getCursorPresenter();
  if (!p) {
    cursorPresenterLastImageOwner = null;
    return;
  }

  if (p.setCursorRenderEnabled) {
    try {
      p.setCursorRenderEnabled(cursorRenderEnabled);
    } catch (err) {
      postPresenterError(err, presenter?.backend);
    }
  }

  if (cursorImage && cursorWidth > 0 && cursorHeight > 0 && p.setCursorImageRgba8) {
    const presenterChanged = cursorPresenterLastImageOwner !== p;
    const imageChanged =
      cursorPresenterLastImage !== cursorImage ||
      cursorPresenterLastImageWidth !== cursorWidth ||
      cursorPresenterLastImageHeight !== cursorHeight;
    if (presenterChanged || imageChanged) {
      try {
        p.setCursorImageRgba8(cursorImage, cursorWidth, cursorHeight);
        cursorPresenterLastImageOwner = p;
        cursorPresenterLastImage = cursorImage;
        cursorPresenterLastImageWidth = cursorWidth;
        cursorPresenterLastImageHeight = cursorHeight;
      } catch (err) {
        postPresenterError(err, presenter?.backend);
      }
    }
  }

  if (p.setCursorState) {
    try {
      p.setCursorState(cursorEnabled, cursorX, cursorY, cursorHotX, cursorHotY);
    } catch (err) {
      postPresenterError(err, presenter?.backend);
    }
  }
};

const redrawCursor = (): void => {
  if (snapshotPaused) return;
  try {
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
    // output source (framebuffer vs AeroGPU vs WDDM scanout).
    if (aerogpuLastOutputSource === "aerogpu") {
      const last = aerogpuLastPresentedFrame;
      if (!last) return;
      presenter.present(last.rgba8, last.width * BYTES_PER_PIXEL_RGBA8);
      presenterNeedsFullUpload = false;
      return;
    }

    if (aerogpuLastOutputSource === "wddm_scanout") {
      const lastScanout = wddmScanoutRgba;
      if (lastScanout && wddmScanoutWidth > 0 && wddmScanoutHeight > 0) {
        if (wddmScanoutWidth !== presenterSrcWidth || wddmScanoutHeight !== presenterSrcHeight) {
          presenterSrcWidth = wddmScanoutWidth;
          presenterSrcHeight = wddmScanoutHeight;
          if (presenter.backend === "webgpu") surfaceReconfigures += 1;
          presenter.resize(wddmScanoutWidth, wddmScanoutHeight, outputDpr);
          presenterNeedsFullUpload = true;
        }
        presenter.present(lastScanout, wddmScanoutWidth * BYTES_PER_PIXEL_RGBA8);
        presenterNeedsFullUpload = false;
        return;
      }
      // If the scanout cache is unavailable, fall through and attempt to read the current frame.
    }

    const frame = getCurrentFrameInfo();
    if (!frame) return;
    aerogpuLastOutputSource = frame.outputSource;
    presenter.present(frame.pixels, frame.strideBytes);
  } catch (err) {
    postPresenterError(err, presenter?.backend);
  }
};

const MAX_HW_CURSOR_DIM = 256;

const tryReadHwCursorImageRgba8 = (
  basePaddr: bigint,
  width: number,
  height: number,
  pitchBytes: number,
  format: number,
): Uint8Array | null => {
  if (basePaddr === 0n) return null;
  if (basePaddr > BigInt(Number.MAX_SAFE_INTEGER)) return null;

  const w = Math.max(0, width | 0);
  const h = Math.max(0, height | 0);
  const pitch = pitchBytes >>> 0;
  if (w === 0 || h === 0 || pitch === 0) return null;

  const rowBytes = w * 4;
  if (pitch < rowBytes) return null;

  // Required bytes = pitch*(h-1) + rowBytes (same as Rust cursor validation), not pitch*h.
  // The last row only needs `rowBytes` bytes, even if `pitch` is larger.
  const requiredBytesBig = BigInt(pitch) * BigInt(h - 1) + BigInt(rowBytes);
  if (requiredBytesBig > BigInt(Number.MAX_SAFE_INTEGER)) return null;
  const requiredBytes = Number(requiredBytesBig);

  const fmt = format >>> 0;
  const isSrgb =
    fmt === CURSOR_FORMAT_B8G8R8A8_SRGB ||
    fmt === CURSOR_FORMAT_B8G8R8X8_SRGB ||
    fmt === CURSOR_FORMAT_R8G8B8A8_SRGB ||
    fmt === CURSOR_FORMAT_R8G8B8X8_SRGB;
  const kind: "bgra" | "bgrx" | "rgba" | "rgbx" | null =
    fmt === CURSOR_FORMAT_B8G8R8A8 || fmt === CURSOR_FORMAT_B8G8R8A8_SRGB
      ? "bgra"
      : fmt === CURSOR_FORMAT_B8G8R8X8 || fmt === CURSOR_FORMAT_B8G8R8X8_SRGB
        ? "bgrx"
        : fmt === CURSOR_FORMAT_R8G8B8A8 || fmt === CURSOR_FORMAT_R8G8B8A8_SRGB
          ? "rgba"
          : fmt === CURSOR_FORMAT_R8G8B8X8 || fmt === CURSOR_FORMAT_R8G8B8X8_SRGB
            ? "rgbx"
            : null;
  if (!kind) return null;

  // VRAM aperture fast path (BAR1 backing).
  //
  // Hardware cursor surfaces are frequently allocated in VRAM by WDDM drivers. When the cursor
  // state descriptor points into the shared VRAM aperture, read directly from `vramU8` instead of
  // guest RAM.
  const vram = vramU8;
  if (vram && vramSizeBytes > 0) {
    const vramBase = BigInt(vramBasePaddr >>> 0);
    const vramEnd = vramBase + BigInt(vramSizeBytes >>> 0);
    const endPaddr = basePaddr + requiredBytesBig;
    if (basePaddr >= vramBase && endPaddr <= vramEnd) {
      const startBig = basePaddr - vramBase;
      if (startBig <= BigInt(Number.MAX_SAFE_INTEGER)) {
        const start = Number(startBig);
        const end = start + requiredBytes;
        if (end >= start && end <= vram.byteLength) {
          const src = vram.subarray(start, end);
          const out = new Uint8Array(rowBytes * h);
          convertScanoutToRgba8({
            src,
            srcStrideBytes: pitch,
            dst: out,
            dstStrideBytes: rowBytes,
            width: w,
            height: h,
            kind: kind as ScanoutSwizzleKind,
          });
          if (isSrgb) linearizeSrgbRgba8InPlace(out);
          return out;
        }
      }
      return null;
    }
  }

  const guest = guestU8;
  const layout = guestLayout;
  if (!guest || !layout) return null;

  const base = Number(basePaddr);
  if (!Number.isFinite(base) || base < 0) return null;

  const ramBytes = guest.byteLength;
  try {
    if (!guestRangeInBoundsRaw(ramBytes, base, requiredBytes)) return null;
  } catch {
    return null;
  }

  const start = guestPaddrToRamOffsetRaw(ramBytes, base);
  if (start === null) return null;
  const end = start + requiredBytes;
  if (end < start || end > guest.byteLength) return null;

  const src = guest.subarray(start, end);
  const out = new Uint8Array(rowBytes * h);
  convertScanoutToRgba8({
    src,
    srcStrideBytes: pitch,
    dst: out,
    dstStrideBytes: rowBytes,
    width: w,
    height: h,
    kind: kind as ScanoutSwizzleKind,
  });

  if (isSrgb) linearizeSrgbRgba8InPlace(out);
  return out;
};

const syncHardwareCursorFromState = (): void => {
  const words = hwCursorState;
  if (!words) return;

  let snap: CursorStateSnapshot | null;
  try {
    snap = trySnapshotCursorStateBounded(words);
  } catch {
    snap = null;
  }
  if (!snap) return;

  if (!hwCursorActive) {
    const anyNonZero =
      (snap.generation >>> 0) !== 0 ||
      (snap.enable >>> 0) !== 0 ||
      (snap.width >>> 0) !== 0 ||
      (snap.height >>> 0) !== 0 ||
      ((snap.basePaddrLo | snap.basePaddrHi) >>> 0) !== 0;
    if (!anyNonZero) return;
    hwCursorActive = true;
  }

  const gen = snap.generation >>> 0;

  const w = Math.min(snap.width >>> 0, MAX_HW_CURSOR_DIM);
  const h = Math.min(snap.height >>> 0, MAX_HW_CURSOR_DIM);
  const pitchBytes = snap.pitchBytes >>> 0;
  const format = snap.format >>> 0;
  const basePaddr = (BigInt(snap.basePaddrHi >>> 0) << 32n) | BigInt(snap.basePaddrLo >>> 0);

  const imageKey = `${basePaddr.toString(16)}:${w}:${h}:${pitchBytes}:${format}`;
  const generationChanged = hwCursorLastGeneration !== gen;
  const keyChanged = hwCursorLastImageKey !== imageKey;

  let imageUpdated = false;
  if (generationChanged || keyChanged) {
    hwCursorLastGeneration = gen;
    hwCursorLastImageKey = imageKey;
    imageUpdated = true;

    const rgba =
      w > 0 && h > 0 && pitchBytes > 0 && basePaddr !== 0n
        ? tryReadHwCursorImageRgba8(basePaddr, w, h, pitchBytes, format)
        : null;
    if (rgba) {
      cursorImage = rgba;
      cursorWidth = w;
      cursorHeight = h;
    } else {
      // Diagnostics: when the cursor surface points into the VRAM aperture but the GPU worker was
      // started without a shared VRAM buffer, we cannot read the cursor image. Surface this once
      // per (base/size/format) key to avoid spamming events on cursor movement updates (which also
      // bump the seqlock generation).
      const vramSize = vramSizeBytes >>> 0;
      if (vramSize > 0 && !vramU8 && basePaddr !== 0n) {
        const vramBase = BigInt(vramBasePaddr >>> 0);
        const vramEnd = vramBase + BigInt(vramSize);
        if (basePaddr >= vramBase && basePaddr < vramEnd) {
          if (hwCursorLastVramMissingEventKey !== imageKey) {
            hwCursorLastVramMissingEventKey = imageKey;
            emitGpuEvent({
              time_ms: performance.now(),
              backend_kind: backendKindForEvent(),
              severity: "warn",
              category: "CursorReadback",
              message: "Cursor: base_paddr points into VRAM but VRAM is unavailable",
              details: {
                cursor: {
                  generation: snap.generation >>> 0,
                  enable: snap.enable >>> 0,
                  x: snap.x | 0,
                  y: snap.y | 0,
                  hotX: snap.hotX >>> 0,
                  hotY: snap.hotY >>> 0,
                  width: snap.width >>> 0,
                  height: snap.height >>> 0,
                  pitchBytes: snap.pitchBytes >>> 0,
                  format: snap.format >>> 0,
                  format_str: aerogpuFormatToString(snap.format >>> 0),
                  base_paddr: formatU64Hex(snap.basePaddrHi, snap.basePaddrLo),
                },
                vram_base_paddr: `0x${(vramBasePaddr >>> 0).toString(16)}`,
                vram_size_bytes: vramSize,
              },
            });
          }
        }
      }

      cursorImage = null;
      cursorWidth = 0;
      cursorHeight = 0;
      cursorPresenterLastImage = null;
      cursorPresenterLastImageWidth = 0;
      cursorPresenterLastImageHeight = 0;
    }
  }

  const enabledFlag = (snap.enable >>> 0) !== 0;
  const hasImage = !!cursorImage && cursorWidth > 0 && cursorHeight > 0;
  const nextEnabled = enabledFlag && hasImage;
  const nextX = snap.x | 0;
  const nextY = snap.y | 0;
  const nextHotX = snap.hotX >>> 0;
  const nextHotY = snap.hotY >>> 0;

  const stateChanged =
    cursorEnabled !== nextEnabled ||
    cursorX !== nextX ||
    cursorY !== nextY ||
    cursorHotX !== nextHotX ||
    cursorHotY !== nextHotY;

  cursorEnabled = nextEnabled;
  cursorX = nextX;
  cursorY = nextY;
  cursorHotX = nextHotX;
  cursorHotY = nextHotY;

  if (imageUpdated || stateChanged) {
    syncCursorToPresenter();
    redrawCursor();
  }
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
      // Mirror the presenter shader's alpha policy:
      // outA = a + dstA * (1 - a)
      const dstA = dst[dstOff + 3]!;
      dst[dstOff + 3] = Math.min(255, a + Math.floor((dstA * invA + 127) / 255));
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
  if (snapshotPaused) return;
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

// -----------------------------------------------------------------------------
// Scanout readback (guest RAM/VRAM -> RGBA8 presenter frame)
// -----------------------------------------------------------------------------

type ScanoutReadback = { width: number; height: number; strideBytes: number; rgba8: Uint8Array };

// Per-tick safety limit to avoid pathological allocations/copies on corrupt scanout descriptors.
//
// Reuse the same 256 MiB limit as other scanout readback paths (e.g. screenshots) so a corrupt or
// malicious scanout descriptor cannot OOM/crash the GPU worker.

let wddmScanoutRgbaCapacity = 0;
let wddmScanoutRgbaU32: Uint32Array | null = null;

let lastScanoutReadbackErrorGeneration: number | null = null;
let lastScanoutReadbackErrorReason: string | null = null;

const emitScanoutReadbackInvalid = (snap: ScanoutStateSnapshot, reason: string, details?: Record<string, unknown>): void => {
  // Avoid spamming: only emit once per (generation, reason) pair.
  if (lastScanoutReadbackErrorGeneration === snap.generation && lastScanoutReadbackErrorReason === reason) return;
  lastScanoutReadbackErrorGeneration = snap.generation;
  lastScanoutReadbackErrorReason = reason;

  emitGpuEvent({
    time_ms: performance.now(),
    backend_kind: backendKindForEvent(),
    severity: "warn",
    category: "ScanoutReadback",
    message: reason,
    details: {
      scanout: {
        generation: snap.generation >>> 0,
        source: snap.source >>> 0,
        format: snap.format >>> 0,
        format_str: aerogpuFormatToString(snap.format >>> 0),
        width: snap.width >>> 0,
        height: snap.height >>> 0,
        pitchBytes: snap.pitchBytes >>> 0,
        base_paddr: formatU64Hex(snap.basePaddrHi, snap.basePaddrLo),
      },
      ...(details ?? {}),
    },
  });
};

const fillRgba8Solid = (dst: Uint8Array, r: number, g: number, b: number, a: number): void => {
  const rr = r & 0xff;
  const gg = g & 0xff;
  const bb = b & 0xff;
  const aa = a & 0xff;
  for (let i = 0; i + 3 < dst.length; i += 4) {
    dst[i + 0] = rr;
    dst[i + 1] = gg;
    dst[i + 2] = bb;
    dst[i + 3] = aa;
  }
};

const ensureScanoutRgbaCapacity = (requiredBytes: number): Uint8Array | null => {
  if (requiredBytes <= 0) return null;
  if (wddmScanoutRgba && wddmScanoutRgbaCapacity >= requiredBytes) {
    if (!wddmScanoutRgbaU32 || wddmScanoutRgbaU32.buffer !== wddmScanoutRgba.buffer) {
      wddmScanoutRgbaU32 = new Uint32Array(
        wddmScanoutRgba.buffer,
        wddmScanoutRgba.byteOffset,
        wddmScanoutRgba.byteLength >>> 2,
      );
    }
    return wddmScanoutRgba;
  }

  try {
    wddmScanoutRgba = new Uint8Array(requiredBytes);
  } catch {
    wddmScanoutRgba = null;
    wddmScanoutRgbaCapacity = 0;
    wddmScanoutRgbaU32 = null;
    return null;
  }
  wddmScanoutRgbaCapacity = requiredBytes;
  wddmScanoutRgbaU32 = new Uint32Array(
    wddmScanoutRgba.buffer,
    wddmScanoutRgba.byteOffset,
    wddmScanoutRgba.byteLength >>> 2,
  );
  return wddmScanoutRgba;
};

function isWddmDisabledScanoutDescriptor(snap: Pick<ScanoutStateSnapshot, "source" | "basePaddrLo" | "basePaddrHi" | "width" | "height" | "pitchBytes">): boolean {
  if ((snap.source >>> 0) !== SCANOUT_SOURCE_WDDM) return false;
  const hasBasePaddr = ((snap.basePaddrLo | snap.basePaddrHi) >>> 0) !== 0;
  if (hasBasePaddr) return false;
  return (snap.width >>> 0) === 0 && (snap.height >>> 0) === 0 && (snap.pitchBytes >>> 0) === 0;
}

function isWddmDisabledScanoutState(words: Int32Array): boolean {
  // Defensive: a disabled scanout descriptor is defined by all-zero geometry + base_paddr.
  // Read the values individually using Atomics so we can make a best-effort determination even if
  // `snapshotScanoutState` is temporarily failing due to a wedged busy bit.
  const source = Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
  if (source !== SCANOUT_SOURCE_WDDM) return false;
  const lo = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_LO) >>> 0;
  const hi = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_HI) >>> 0;
  if (((lo | hi) >>> 0) !== 0) return false;
  const width = Atomics.load(words, ScanoutStateIndex.WIDTH) >>> 0;
  const height = Atomics.load(words, ScanoutStateIndex.HEIGHT) >>> 0;
  const pitchBytes = Atomics.load(words, ScanoutStateIndex.PITCH_BYTES) >>> 0;
  return width === 0 && height === 0 && pitchBytes === 0;
}

const tryReadScanoutRgba8 = (snap: ScanoutStateSnapshot): ScanoutReadback | null => {
  if (snapshotPaused) return null;
  const source = snap.source >>> 0;
  const wantsScanout = source === SCANOUT_SOURCE_WDDM || source === SCANOUT_SOURCE_LEGACY_VBE_LFB;
  if (!wantsScanout) return null;

  // WDDM publishes `base/width/height/pitch=0` when scanout is disabled but ownership is retained.
  // Treat this as a valid "blank" state (not an invalid descriptor).
  if (isWddmDisabledScanoutDescriptor(snap)) return null;

  const fmt = snap.format >>> 0;
  const isSrgb =
    fmt === SCANOUT_FORMAT_B8G8R8X8_SRGB ||
    fmt === SCANOUT_FORMAT_B8G8R8A8_SRGB ||
    fmt === SCANOUT_FORMAT_R8G8B8A8_SRGB ||
    fmt === SCANOUT_FORMAT_R8G8B8X8_SRGB;
  // Supported scanout formats:
  // - BGRX/RGBX: force alpha=255 so presenters (which may render with blending enabled) do not show
  //              random transparency from uninitialized X bytes.
  // - BGRA/RGBA: preserve alpha (required for correctness + matches `scanout_swizzle.ts` behavior).
  // - B5G6R5: opaque 16bpp; expand to RGBA with alpha=255.
  // - B5G5R5A1: 16bpp with 1-bit alpha; expand alpha to 0 or 255.
  let kind: ScanoutSwizzleKind | null = null;
  let srcBytesPerPixel: number;
  let isB5G6R5 = false;
  let isB5G5R5A1 = false;
  switch (fmt) {
    case SCANOUT_FORMAT_B8G8R8X8:
    case SCANOUT_FORMAT_B8G8R8X8_SRGB:
      kind = "bgrx";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_B8G8R8A8:
    case SCANOUT_FORMAT_B8G8R8A8_SRGB:
      kind = "bgra";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_R8G8B8A8:
    case SCANOUT_FORMAT_R8G8B8A8_SRGB:
      kind = "rgba";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_R8G8B8X8:
    case SCANOUT_FORMAT_R8G8B8X8_SRGB:
      kind = "rgbx";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_B5G6R5:
      srcBytesPerPixel = 2;
      isB5G6R5 = true;
      break;
    case SCANOUT_FORMAT_B5G5R5A1:
      srcBytesPerPixel = 2;
      isB5G5R5A1 = true;
      break;
    default:
      emitScanoutReadbackInvalid(snap, "Scanout: unsupported format", {
        expected: [
          SCANOUT_FORMAT_B8G8R8X8,
          SCANOUT_FORMAT_B8G8R8A8,
          SCANOUT_FORMAT_B8G8R8X8_SRGB,
          SCANOUT_FORMAT_B8G8R8A8_SRGB,
          SCANOUT_FORMAT_B5G6R5,
          SCANOUT_FORMAT_B5G5R5A1,
          SCANOUT_FORMAT_R8G8B8A8,
          SCANOUT_FORMAT_R8G8B8A8_SRGB,
          SCANOUT_FORMAT_R8G8B8X8,
          SCANOUT_FORMAT_R8G8B8X8_SRGB,
        ],
        got: fmt,
      });
      return null;
  }

  const width = snap.width >>> 0;
  const height = snap.height >>> 0;
  const pitchBytes = snap.pitchBytes >>> 0;
  if (width === 0 || height === 0) {
    emitScanoutReadbackInvalid(snap, "Scanout: width/height must be non-zero");
    return null;
  }
  // Guard against corrupt descriptors that could otherwise trigger huge loops even if the
  // total byte count is within `MAX_SCANOUT_RGBA8_BYTES` (e.g. width=1, height=64M).
  const MAX_WDDM_SCANOUT_DIM = 16384;
  if (width > MAX_WDDM_SCANOUT_DIM || height > MAX_WDDM_SCANOUT_DIM) {
    emitScanoutReadbackInvalid(snap, "Scanout: width/height exceeds size limit", {
      width,
      height,
      maxDim: MAX_WDDM_SCANOUT_DIM,
    });
    return null;
  }

  const srcRowBytes = width * srcBytesPerPixel;
  if (!Number.isSafeInteger(srcRowBytes) || srcRowBytes <= 0) {
    emitScanoutReadbackInvalid(snap, "Scanout: invalid srcRowBytes", { srcRowBytes, width, srcBytesPerPixel });
    return null;
  }
  if (pitchBytes < srcRowBytes) {
    emitScanoutReadbackInvalid(snap, "Scanout: pitchBytes < rowBytes", { rowBytes: srcRowBytes, pitchBytes });
    return null;
  }

  const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
  const outputBytesU64 = BigInt(width) * BigInt(height) * BigInt(BYTES_PER_PIXEL_RGBA8);
  const outputBytes = tryComputeScanoutRgba8ByteLength(width, height, MAX_SCANOUT_RGBA8_BYTES);
  if (outputBytes === null) {
    emitScanoutReadbackInvalid(snap, "Scanout: framebuffer exceeds size budget", {
      budgetBytes: MAX_SCANOUT_RGBA8_BYTES,
      outputBytes: outputBytesU64.toString(),
    });
    return null;
  }

  const basePaddr = (BigInt(snap.basePaddrHi >>> 0) << 32n) | BigInt(snap.basePaddrLo >>> 0);
  if (basePaddr === 0n) {
    // WDDM uses base_paddr=0 as a placeholder descriptor for the host-side AeroGPU path.
    // Legacy VBE scanout should always point at a real framebuffer.
    if (source === SCANOUT_SOURCE_LEGACY_VBE_LFB) {
      emitScanoutReadbackInvalid(snap, "Scanout: base_paddr is 0");
    }
    return null;
  }
  if (basePaddr > BigInt(Number.MAX_SAFE_INTEGER)) {
    emitScanoutReadbackInvalid(snap, "Scanout: base_paddr exceeds JS safe integer range");
    return null;
  }
  const basePaddrNum = Number(basePaddr);

  const requiredReadBytesU64 = (BigInt(height) - 1n) * BigInt(pitchBytes) + BigInt(srcRowBytes);
  if (requiredReadBytesU64 > BigInt(Number.MAX_SAFE_INTEGER)) {
    emitScanoutReadbackInvalid(snap, "Scanout: framebuffer byte range exceeds JS safe integer range", {
      requiredReadBytes: requiredReadBytesU64.toString(),
    });
    return null;
  }
  const requiredReadBytes = Number(requiredReadBytesU64);

  // Resolve the backing store for base_paddr (guest RAM or VRAM aperture).
  let src: Uint8Array;
  let srcU32: Uint32Array | null = null;
  let srcOffset = 0;

  const vramBase = vramBasePaddr >>> 0;
  const vramSize = vramSizeBytes >>> 0;
  const vramEnd = vramBase + vramSize;
  const baseEnd = basePaddrNum + requiredReadBytes;
  if (vramSize > 0 && basePaddrNum >= vramBase && basePaddrNum < vramEnd) {
    const vram = vramU8;
    if (!vram) {
      emitScanoutReadbackInvalid(snap, "Scanout: base_paddr points into VRAM but VRAM is unavailable", {
        backing: "vram",
        vramBasePaddr: `0x${vramBase.toString(16)}`,
        vramSizeBytes: vramSize,
      });
      return null;
    }
    const offset = basePaddrNum - vramBase;
    if (!Number.isSafeInteger(offset) || offset < 0) {
      emitScanoutReadbackInvalid(snap, "Scanout: invalid VRAM offset", { backing: "vram", offset });
      return null;
    }
    if (!Number.isSafeInteger(baseEnd) || baseEnd > vramEnd) {
      emitScanoutReadbackInvalid(snap, "Scanout: scanout range exceeds VRAM aperture", {
        backing: "vram",
        requiredReadBytes,
        vramSizeBytes: vramSize,
      });
      return null;
    }
    if (offset + requiredReadBytes > vram.byteLength) {
      emitScanoutReadbackInvalid(snap, "Scanout: scanout range exceeds VRAM buffer length", {
        backing: "vram",
        offset,
        requiredReadBytes,
        vramLen: vram.byteLength,
      });
      return null;
    }
    src = vram;
    srcU32 = vramU32;
    srcOffset = offset;
  } else {
    const guest = guestU8;
    if (!guest) {
      emitScanoutReadbackInvalid(snap, "Scanout: guest memory is not available");
      return null;
    }

    const ramBytes = guest.byteLength;
    try {
      if (!guestRangeInBoundsRaw(ramBytes, basePaddrNum, requiredReadBytes)) {
        emitScanoutReadbackInvalid(snap, "Scanout: base_paddr range is outside guest RAM (or crosses an MMIO hole)", {
          requiredReadBytes,
          guestLen: ramBytes,
        });
        return null;
      }
    } catch (err) {
      emitScanoutReadbackInvalid(snap, "Scanout: failed to validate guest range", { error: err });
      return null;
    }

    const ramOffset = guestPaddrToRamOffsetRaw(ramBytes, basePaddrNum);
    if (ramOffset === null) {
      emitScanoutReadbackInvalid(snap, "Scanout: base_paddr is not backed by RAM");
      return null;
    }
    if (ramOffset + requiredReadBytes > ramBytes) {
      emitScanoutReadbackInvalid(snap, "Scanout: translated RAM range is out of bounds", {
        ramOffset,
        requiredReadBytes,
        guestLen: ramBytes,
      });
      return null;
    }
    src = guest;
    srcU32 = guestU32;
    srcOffset = ramOffset;
  }

  const out = ensureScanoutRgbaCapacity(outputBytes);
  const outU32 = wddmScanoutRgbaU32;
  if (!out) return null;

  const canUseU32 =
    kind !== null &&
    (srcOffset & 3) === 0 &&
    (pitchBytes & 3) === 0 &&
    !!srcU32 &&
    !!outU32 &&
    (out.byteOffset & 3) === 0;

  if (kind !== null) {
    // -------------------------------------------------------------------------
    // 32bpp scanout conversion (BGRX/BGRA/RGBA/RGBX).
    // -------------------------------------------------------------------------
    if (canUseU32) {
      const src32 = srcU32!;
      const dst32 = outU32!;
      const baseIndex = srcOffset >>> 2;
      const pitchWords = pitchBytes >>> 2;
      let dstRowBase = 0;
      switch (kind) {
        case "rgba":
          for (let y = 0; y < height; y += 1) {
            const srcRowBase = baseIndex + y * pitchWords;
            for (let x = 0; x < width; x += 1) {
              dst32[dstRowBase + x] = src32[srcRowBase + x]!;
            }
            dstRowBase += width;
          }
          break;
        case "rgbx":
          for (let y = 0; y < height; y += 1) {
            const srcRowBase = baseIndex + y * pitchWords;
            for (let x = 0; x < width; x += 1) {
              dst32[dstRowBase + x] = (src32[srcRowBase + x]! | 0xff000000) >>> 0;
            }
            dstRowBase += width;
          }
          break;
        case "bgra":
          for (let y = 0; y < height; y += 1) {
            const srcRowBase = baseIndex + y * pitchWords;
            for (let x = 0; x < width; x += 1) {
              const v = src32[srcRowBase + x]!;
              // BGRA u32 = 0xAARRGGBB -> RGBA u32 = 0xAABBGGRR
              dst32[dstRowBase + x] =
                ((v & 0xff000000) | ((v >>> 16) & 0xff) | (v & 0xff00) | ((v & 0xff) << 16)) >>> 0;
            }
            dstRowBase += width;
          }
          break;
        case "bgrx":
          for (let y = 0; y < height; y += 1) {
            const srcRowBase = baseIndex + y * pitchWords;
            for (let x = 0; x < width; x += 1) {
              const v = src32[srcRowBase + x]!;
              // BGRX u32 = 0xXXRRGGBB -> RGBA u32 = 0xFFBBGGRR
              dst32[dstRowBase + x] = (((v >>> 16) & 0xff) | (v & 0xff00) | ((v & 0xff) << 16) | 0xff000000) >>> 0;
            }
            dstRowBase += width;
          }
          break;
      }
    } else {
      const swapRb = kind === "bgrx" || kind === "bgra";
      const preserveAlpha = kind === "bgra" || kind === "rgba";
      for (let y = 0; y < height; y += 1) {
        const srcRowStart = srcOffset + y * pitchBytes;
        const dstRowStart = y * rowBytes;
        for (let x = 0; x < rowBytes; x += BYTES_PER_PIXEL_RGBA8) {
          const c0 = src[srcRowStart + x]!;
          const c1 = src[srcRowStart + x + 1]!;
          const c2 = src[srcRowStart + x + 2]!;
          const r = swapRb ? c2 : c0;
          const g = c1;
          const b = swapRb ? c0 : c2;
          const a = preserveAlpha ? src[srcRowStart + x + 3]! : 255;
          out[dstRowStart + x + 0] = r;
          out[dstRowStart + x + 1] = g;
          out[dstRowStart + x + 2] = b;
          out[dstRowStart + x + 3] = a;
        }
      }
    }
  } else if (isB5G6R5 || isB5G5R5A1) {
    // -------------------------------------------------------------------------
    // 16bpp scanout conversion (B5G6R5 / B5G5R5A1).
    // -------------------------------------------------------------------------
    if (!outU32) return null;
    const dst32 = outU32;
    for (let y = 0; y < height; y += 1) {
      let srcOff = srcOffset + y * pitchBytes;
      const dstRowBase = y * width;
      for (let x = 0; x < width; x += 1) {
        const lo = src[srcOff++]!;
        const hi = src[srcOff++]!;
        const pix = (lo | (hi << 8)) >>> 0;
        if (isB5G6R5) {
          const b = pix & 0x1f;
          const g = (pix >>> 5) & 0x3f;
          const r = (pix >>> 11) & 0x1f;
          const r8 = ((r << 3) | (r >>> 2)) & 0xff;
          const g8 = ((g << 2) | (g >>> 4)) & 0xff;
          const b8 = ((b << 3) | (b >>> 2)) & 0xff;
          dst32[dstRowBase + x] = (r8 | (g8 << 8) | (b8 << 16) | (0xff << 24)) >>> 0;
        } else {
          const b = pix & 0x1f;
          const g = (pix >>> 5) & 0x1f;
          const r = (pix >>> 10) & 0x1f;
          const a = (pix >>> 15) & 0x1;
          const r8 = ((r << 3) | (r >>> 2)) & 0xff;
          const g8 = ((g << 3) | (g >>> 2)) & 0xff;
          const b8 = ((b << 3) | (b >>> 2)) & 0xff;
          dst32[dstRowBase + x] = (r8 | (g8 << 8) | (b8 << 16) | ((a ? 0xff : 0) << 24)) >>> 0;
        }
      }
    }
  }

  // If the scanout is an sRGB format, decode sRGB -> linear before uploading to the presenter.
  //
  // Presenters blend cursor overlays in linear space and then encode to sRGB for output. If we
  // were to upload sRGB-encoded bytes as if they were linear, we'd effectively double-encode
  // gamma and the scanout would appear incorrect.
  if (isSrgb) {
    linearizeSrgbRgba8InPlace(out);
  }

  wddmScanoutWidth = width;
  wddmScanoutHeight = height;
  wddmScanoutFormat = fmt;
  return { width, height, strideBytes: rowBytes, rgba8: out };
};
const noteWddmScanoutFallback = (): void => {
  if (scanoutState) return;
  wddmOwnsScanoutFallback = true;
};

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
  const scanout = snapshotScanoutForTelemetry();
  postToMain({
    type: 'metrics',
    framesReceived,
    framesPresented,
    framesDropped,
    telemetry: telemetry.snapshot(),
    outputSource: aerogpuLastOutputSource,
    presentUpload: presentUploadForTelemetry(),
    ...(scanout ? { scanout } : {}),
  });
};

function backendKindForEvent(): string {
  if (presenter) return presenter.backend;
  if (runtimeCanvas) return "unknown";
  return "headless";
}

function trySnapshotScanoutState(): ScanoutStateSnapshot | null {
  if (snapshotPaused) return null;
  const words = scanoutState;
  if (!words) return null;
  try {
    return trySnapshotScanoutStateBounded(words);
  } catch {
    return null;
  }
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
    return {
      name: value.name,
      code: value.code,
      message: value.message,
      stack: value.stack,
      // Preserve the underlying error cause when present. This is extremely useful for debugging
      // (e.g. WebAssembly traps, WebGPU validation errors) and is safe because we sanitize it
      // recursively into a structured-cloneable shape.
      ...(value.cause === undefined ? {} : { cause: sanitizeForPostMessage(value.cause) }),
    };
  }
  if (value instanceof Error) {
    const cause = (value as unknown as { cause?: unknown }).cause;
    return {
      name: value.name,
      message: value.message,
      stack: value.stack,
      ...(cause === undefined ? {} : { cause: sanitizeForPostMessage(cause) }),
    };
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

function presenterErrorCodeToCategory(code: string): string {
  const normalized = code.toLowerCase();

  // Init / backend selection failures.
  if (
    normalized.includes("init") ||
    normalized.includes("unavailable") ||
    normalized.includes("disabled") ||
    normalized.includes("no_adapter") ||
    normalized.includes("no_device") ||
    normalized.includes("no_backend") ||
    normalized.includes("backend_incompatible") ||
    normalized.includes("unknown_backend") ||
    normalized.includes("missing_wasm_memory")
  ) {
    return "Init";
  }

  // Surface / context / present problems.
  if (
    normalized.includes("context") ||
    normalized.includes("surface") ||
    normalized === "webgl_error" ||
    normalized.includes("present") ||
    normalized.includes("resize")
  ) {
    return "Surface";
  }

  // Shader compilation / linking.
  if (normalized.includes("shader") || normalized.includes("program") || normalized.includes("wgsl")) {
    return "ShaderCompile";
  }

  if (normalized.includes("pipeline")) {
    return "PipelineCreate";
  }

  if (normalized.includes("screenshot")) {
    return "Screenshot";
  }

  if (normalized.includes("out_of_memory") || normalized.includes("outofmemory") || normalized.includes("oom")) {
    return "OutOfMemory";
  }

  if (
    normalized.includes("invalid") ||
    normalized.includes("oob") ||
    normalized.includes("too_small") ||
    normalized.includes("validation")
  ) {
    return "Validation";
  }

  return "Unknown";
}

function presenterErrorToSeverity(
  code: string | null,
  category: string,
  opts: { isInitFailure: boolean },
): GpuRuntimeErrorEvent["severity"] {
  const normalized = (code ?? "").toLowerCase();

  if (category === "OutOfMemory") return "fatal";

  if (category === "Init" && opts.isInitFailure) return "fatal";

  // Future-proofing: when backends start emitting surface timeout/outdated events we want
  // those to be non-fatal so rendering can continue.
  if (
    category === "Surface" &&
    (normalized.includes("timeout") ||
      normalized.includes("timed_out") ||
      normalized.includes("outdated") ||
      normalized.includes("out_of_date"))
  ) {
    return "warn";
  }

  return "error";
}

function shouldEmitPresenterErrorEvent(key: string): boolean {
  if (presenterErrorEventGeneration !== presenterErrorGeneration) {
    presenterErrorEventGeneration = presenterErrorGeneration;
    presenterErrorEventKeys.clear();
  }
  if (presenterErrorEventKeys.has(key)) return false;
  presenterErrorEventKeys.add(key);
  // Defensive bound: some error sources can include unique IDs in their messages, which would
  // otherwise cause the dedupe cache to grow without limit over a long-running session.
  if (presenterErrorEventKeys.size > 1024) {
    presenterErrorEventKeys.clear();
    presenterErrorEventKeys.add(key);
  }
  return true;
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
    recoveries_attempted_wddm: recoveriesAttemptedWddm,
    recoveries_succeeded_wddm: recoveriesSucceededWddm,
  };
}

function formatU64Hex(hi: number, lo: number): string {
  // Keep the value JSON-friendly (no BigInt) and stable-width for debugging.
  const v = (BigInt(hi >>> 0) << 32n) | BigInt(lo >>> 0);
  return `0x${v.toString(16).padStart(16, "0")}`;
}

function snapshotScanoutForTelemetry(): GpuRuntimeScanoutSnapshotV1 | undefined {
  if (snapshotPaused) return undefined;
  if (!scanoutState) return undefined;
  let snap: ScanoutStateSnapshot | null;
  try {
    snap = trySnapshotScanoutStateBounded(scanoutState);
  } catch {
    return undefined;
  }
  if (!snap) return undefined;
  return {
    source: snap.source,
    base_paddr: formatU64Hex(snap.basePaddrHi, snap.basePaddrLo),
    width: snap.width,
    height: snap.height,
    pitchBytes: snap.pitchBytes,
    format: snap.format,
    format_str: aerogpuFormatToString(snap.format >>> 0),
    generation: snap.generation,
  };
}

function presentUploadForTelemetry(): GpuRuntimePresentUploadV1 {
  if (lastPresentUploadKind === "dirty_rects") {
    return { kind: "dirty_rects", dirtyRectCount: lastPresentUploadDirtyRectCount };
  }
  return { kind: lastPresentUploadKind };
}

function postStatsMessage(wasmStats?: unknown): void {
  const backendKind = presenter?.backend ?? (runtimeCanvas ? undefined : "headless");
  const sanitizedWasmStats = wasmStats === undefined ? undefined : sanitizeForPostMessage(wasmStats);
  const sanitizedFrameTimings = latestFrameTimings ? sanitizeForPostMessage(latestFrameTimings) : undefined;
  const scanout = snapshotScanoutForTelemetry();

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
    outputSource: aerogpuLastOutputSource,
    presentUpload: presentUploadForTelemetry(),
    ...(scanout ? { scanout } : {}),
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
  // Poll `aero-gpu-wasm` telemetry whenever the wasm module is actually in use.
  //
  // - The wgpu-backed WebGL2 presenter (`webgl2_wgpu`) uses `aero-gpu-wasm` for frame presentation.
  // - The wasm D3D9 executor is also used for AeroGPU command execution on both the WebGPU and
  //   wgpu-WebGL2 backends.
  //
  // Avoid loading the wasm module *only* for telemetry: only poll once the D3D9 executor has been
  // initialized (or is in the process of initializing), or when the wasm presenter is active.
  if (presenter?.backend === "webgl2_wgpu") return true;
  return aerogpuWasmD3d9Backend !== null || aerogpuWasmD3d9InitPromise !== null || aerogpuWasm !== null;
}

async function tryDrainAerogpuWasmEvents(): Promise<GpuRuntimeErrorEvent[]> {
  if (!shouldPollAerogpuWasmTelemetry()) return [];
  try {
    const mod = await loadAerogpuWasm();
    const modAny = mod as unknown as Record<string, unknown>;
    const fn =
      (modAny["drain_gpu_events"] as unknown) ??
      (modAny["drain_gpu_error_events"] as unknown) ??
      (modAny["take_gpu_events"] as unknown) ??
      (modAny["take_gpu_error_events"] as unknown) ??
      (modAny["drainGpuEvents"] as unknown);
    if (typeof fn !== "function") return [];
    const raw = await (fn as (...args: unknown[]) => unknown)();
    return normalizeGpuEventBatch(raw);
  } catch {
    return [];
  }
}

async function tryGetAerogpuWasmTelemetry(): Promise<unknown | undefined> {
  if (!shouldPollAerogpuWasmTelemetry()) return undefined;
  try {
    const mod = await loadAerogpuWasm();
    const modAny = mod as unknown as Record<string, unknown>;
    const statsFn = (modAny["get_gpu_stats"] as unknown) ?? (modAny["getGpuStats"] as unknown);
    if (typeof statsFn !== "function") return undefined;
    const stats = parseMaybeJson(await (statsFn as (...args: unknown[]) => unknown)());
    let frameTimings: unknown | undefined = undefined;
    try {
      const timingsFn = (modAny["get_frame_timings"] as unknown) ?? (modAny["getFrameTimings"] as unknown);
      if (typeof timingsFn === "function") {
        const timings = (timingsFn as (...args: unknown[]) => unknown)();
        if (timings != null) frameTimings = timings;
      }
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
  if (snapshotPaused) return;

  telemetryTickInFlight = true;
  beginSnapshotBarrierTask();
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
    endSnapshotBarrierTask();
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
    const preventDefault = (ev as unknown as { preventDefault?: unknown }).preventDefault;
    if (typeof preventDefault === "function") {
      (preventDefault as () => void).call(ev);
    }
    handleDeviceLost("WebGL context lost", { source: "webglcontextlost" }, false);
  };
  onWebglContextRestored = () => {
    if (!isDeviceLost) return;
    void attemptRecovery("webglcontextrestored");
  };

  try {
    const addEventListener = (canvas as unknown as { addEventListener?: unknown }).addEventListener;
    if (typeof addEventListener === "function") {
      (addEventListener as (type: string, listener: unknown, options?: unknown) => void).call(
        canvas,
        "webglcontextlost",
        onWebglContextLost,
        { passive: false },
      );
      (addEventListener as (type: string, listener: unknown, options?: unknown) => void).call(
        canvas,
        "webglcontextrestored",
        onWebglContextRestored,
      );
    }
  } catch {
    // Best-effort: some OffscreenCanvas implementations do not expose these events.
  }
}

function uninstallContextLossHandlers(): void {
  const canvas = canvasWithContextLossHandlers;
  if (!canvas) return;
  try {
    const removeEventListener = (canvas as unknown as { removeEventListener?: unknown }).removeEventListener;
    if (typeof removeEventListener === "function") {
      if (onWebglContextLost) {
        (removeEventListener as (type: string, listener: unknown, options?: unknown) => void).call(
          canvas,
          "webglcontextlost",
          onWebglContextLost,
        );
      }
      if (onWebglContextRestored) {
        (removeEventListener as (type: string, listener: unknown, options?: unknown) => void).call(
          canvas,
          "webglcontextrestored",
          onWebglContextRestored,
        );
      }
    }
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
  const scanoutSnap = trySnapshotScanoutState();
  const scanoutIsWddm =
    scanoutSnap?.source === SCANOUT_SOURCE_WDDM ||
    (!scanoutSnap &&
      (() => {
        if (snapshotPaused) return wddmOwnsScanoutFallback;
        const words = scanoutState;
        if (!words) return wddmOwnsScanoutFallback;
        try {
          return (Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0) === SCANOUT_SOURCE_WDDM;
        } catch {
          return wddmOwnsScanoutFallback;
        }
      })());
  const enrichedDetails: Record<string, unknown> = {
    ...(details === undefined ? {} : { cause: details }),
    ...(scanoutSnap ? { scanout: { ...scanoutSnap, format_str: aerogpuFormatToString(scanoutSnap.format >>> 0) } } : {}),
    scanout_is_wddm: scanoutIsWddm,
    wddm_owns_scanout_fallback: wddmOwnsScanoutFallback,
    aerogpu_last_output_source: aerogpuLastOutputSource,
    aerogpu_has_last_presented_frame: !!aerogpuLastPresentedFrame,
  };
  emitGpuEvent({
    time_ms: performance.now(),
    backend_kind: backend,
    severity: "error",
    category: "DeviceLost",
    message,
    details: enrichedDetails,
  });

  // For WebGL context-loss events (`PresenterError.code === "webgl_context_lost"`), we defer
  // destroying the presenter until a restore/recovery attempt begins. This:
  // - avoids issuing WebGL resource deletion calls while the context is lost, and
  // - allows DEV harnesses that use `WEBGL_lose_context` to call `restoreContext()` (the raw
  //   WebGL2 presenter needs a live context reference for that).
  const preservePresenterForRestore = startRecovery === false;
  if (!preservePresenterForRestore) {
    presenter?.destroy?.();
    presenter = null;
  }
  cursorPresenterLastImageOwner = null;
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

  recoveryPromise = (async () => {
    // Snapshot pause is a guest-memory access barrier. Defer recovery work (which may inspect
    // scanout/framebuffer state) until the coordinator resumes the GPU worker.
    await waitUntilSnapshotResumed();

    const scanoutAtStart = trySnapshotScanoutState();
    const scanoutWasWddm =
      scanoutAtStart?.source === SCANOUT_SOURCE_WDDM ||
      (!scanoutAtStart &&
        (() => {
          if (snapshotPaused) return wddmOwnsScanoutFallback;
          const words = scanoutState;
          if (!words) return wddmOwnsScanoutFallback;
          try {
            return (Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0) === SCANOUT_SOURCE_WDDM;
          } catch {
            return wddmOwnsScanoutFallback;
          }
        })());
    if (scanoutWasWddm) recoveriesAttemptedWddm += 1;
    emitGpuEvent({
      time_ms: performance.now(),
      backend_kind: backendKindForEvent(),
      severity: "info",
      category: "DeviceLost",
      message: `Attempting GPU recovery (${reason})`,
      details: {
        reason,
        ...(scanoutAtStart
          ? { scanout: { ...scanoutAtStart, format_str: aerogpuFormatToString(scanoutAtStart.format >>> 0) } }
          : {}),
        scanout_is_wddm: scanoutWasWddm,
        wddm_owns_scanout_fallback: wddmOwnsScanoutFallback,
        aerogpu_last_output_source: aerogpuLastOutputSource,
        aerogpu_has_last_presented_frame: !!aerogpuLastPresentedFrame,
      },
    });

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
      // Snapshot pause is a guest-memory access barrier. Defer recovery until resume so we can
      // safely inspect scanout/framebuffer state (which may require guest RAM/VRAM readback).
      await waitUntilSnapshotResumed();
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

    const scanoutAtEnd = trySnapshotScanoutState();
    const scanoutIsWddm =
      scanoutAtEnd?.source === SCANOUT_SOURCE_WDDM ||
      (!scanoutAtEnd &&
        (() => {
          if (snapshotPaused) return wddmOwnsScanoutFallback;
          const words = scanoutState;
          if (!words) return wddmOwnsScanoutFallback;
          try {
            return (Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0) === SCANOUT_SOURCE_WDDM;
          } catch {
            return wddmOwnsScanoutFallback;
          }
        })());
    if (scanoutIsWddm) recoveriesSucceededWddm += 1;

    emitGpuEvent({
      time_ms: performance.now(),
      backend_kind: backendKindForEvent(),
      severity: "info",
      category: "DeviceLost",
      message: "GPU recovery succeeded",
      details: {
        ...(scanoutAtEnd ? { scanout: { ...scanoutAtEnd, format_str: aerogpuFormatToString(scanoutAtEnd.format >>> 0) } } : {}),
        scanout_is_wddm: scanoutIsWddm,
        wddm_owns_scanout_fallback: wddmOwnsScanoutFallback,
        aerogpu_last_output_source: aerogpuLastOutputSource,
        aerogpu_has_last_presented_frame: !!aerogpuLastPresentedFrame,
      },
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
  // Route through the presenter error handler so non-device-lost backend failures emit structured
  // events (and are still forwarded through the legacy `type:"error"` message path).
  postPresenterError(err, presenter?.backend);
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

// When scanout is WDDM-owned, the main-thread frame scheduler keeps sending ticks even if the
// legacy shared framebuffer is idle (FRAME_STATUS=PRESENTED). In that state we still need to
// execute a present pass so the worker can poll/present the active scanout source and clear
// any lingering legacy dirty flags (see docs/16-aerogpu-vga-vesa-compat.md §5).
//
// This *must not* participate in the shared-framebuffer DIRTY->PRESENTING->PRESENTED pacing
// contract. We only use this claim as a best-effort way to suppress tick spam while an async
// present is in flight.
const claimPresentWhileIdleForScanout = () => {
  if (!frameState) return;
  Atomics.compareExchange(frameState, FRAME_STATUS_INDEX, FRAME_PRESENTED, FRAME_PRESENTING);
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
  outputSource: GpuRuntimeOutputSource;
  sharedLayout?: SharedFramebufferLayout;
  dirtyRects?: DirtyRect[] | null;
};

type ScanoutFrameInfo = {
  width: number;
  height: number;
  strideBytes: number;
  pixels: Uint8Array;
};

const tryReadScanoutFrame = (snap: ScanoutStateSnapshot): ScanoutFrameInfo | null => {
  const source = snap.source >>> 0;
  const wantsScanout = source === SCANOUT_SOURCE_WDDM || source === SCANOUT_SOURCE_LEGACY_VBE_LFB;
  if (!wantsScanout) return null;

  const hasBasePaddr = ((snap.basePaddrLo | snap.basePaddrHi) >>> 0) !== 0;
  // WDDM uses base_paddr=0 as a placeholder descriptor for the host-side AeroGPU path.
  // However, once WDDM has claimed scanout it may also publish a *disabled* descriptor
  // (`base/width/height/pitch=0`) when scanout is turned off. That must still suppress legacy
  // output; present a blank frame instead of falling back.
  if (source === SCANOUT_SOURCE_WDDM && !hasBasePaddr && !isWddmDisabledScanoutDescriptor(snap)) return null;

  const shot = tryReadScanoutRgba8(snap);
  if (shot) {
    return { width: shot.width, height: shot.height, strideBytes: shot.strideBytes, pixels: shot.rgba8 };
  }

  // Scanout owns output but we failed to read/convert it. Present a black fallback instead of
  // falling back to the legacy shared framebuffer.
  let width = snap.width >>> 0;
  let height = snap.height >>> 0;
  const MAX_SCANOUT_DIM = 16384;
  let outputBytes: number | null =
    width > 0 && height > 0 && width <= MAX_SCANOUT_DIM && height <= MAX_SCANOUT_DIM
      ? tryComputeScanoutRgba8ByteLength(width, height, MAX_SCANOUT_RGBA8_BYTES)
      : null;
  if (outputBytes === null) {
    width = 1;
    height = 1;
    outputBytes = BYTES_PER_PIXEL_RGBA8;
  }

  const out = ensureScanoutRgbaCapacity(outputBytes);
  if (!out) return null;
  // `ensureScanoutRgbaCapacity` may return an oversized cached buffer; only populate the prefix
  // the presenter will consume so we don't spend O(capacity) time blanking after a large mode.
  fillRgba8Solid(out.subarray(0, outputBytes), 0, 0, 0, 0xff);

  wddmScanoutWidth = width;
  wddmScanoutHeight = height;
  return { width, height, strideBytes: width * BYTES_PER_PIXEL_RGBA8, pixels: out };
};

const getCurrentFrameInfo = (): CurrentFrameInfo | null => {
  if (snapshotPaused) return null;
  refreshFramebufferViews();

  if (scanoutState) {
    const words = scanoutState;
    let snap: ScanoutStateSnapshot | null;
    try {
      snap = trySnapshotScanoutStateBounded(words);
    } catch {
      snap = null;
    }

    if (snap) {
      const source = snap.source >>> 0;
      const wantsScanout = source === SCANOUT_SOURCE_WDDM || source === SCANOUT_SOURCE_LEGACY_VBE_LFB;
      if (wantsScanout) {
        const hasBasePaddr = ((snap.basePaddrLo | snap.basePaddrHi) >>> 0) !== 0;
        // WDDM uses base_paddr=0 as a placeholder descriptor for the host-side AeroGPU path.
        const scanoutIsWddmDisabled = isWddmDisabledScanoutDescriptor(snap);
        if (source !== SCANOUT_SOURCE_WDDM || hasBasePaddr || scanoutIsWddmDisabled) {
          const frame = tryReadScanoutFrame(snap);
          if (frame) {
            // Scanout descriptors use guest physical addresses (base_paddr) and can point at padded
            // guest surfaces (pitchBytes). We normalize to a tightly-packed RGBA8 buffer so existing
            // presenter backends can consume it directly.
            const seq = frameState ? Atomics.load(frameState, FRAME_SEQ_INDEX) : 0;
            return { ...frame, frameSeq: seq, outputSource: "wddm_scanout" };
          }
 
          // Scanout owns output (WDDM with a real base_paddr, or legacy VBE), but we failed to
          // read/convert it. Do not fall back to the legacy shared framebuffer, which would cause
          // output to "flash back" over the scanout-owned output.
          return null;
        }
      }
    } else {
      // If the scanout descriptor is unreadable (busy-bit wedge / time budget exceeded) but appears
      // to be scanout-owned, avoid falling back to the legacy framebuffer.
      try {
        const source = Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
        if (source === SCANOUT_SOURCE_WDDM || source === SCANOUT_SOURCE_LEGACY_VBE_LFB) {
          const lo = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_LO) >>> 0;
          const hi = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_HI) >>> 0;
          const hasBasePaddr = ((lo | hi) >>> 0) !== 0;
          if (source === SCANOUT_SOURCE_WDDM && !hasBasePaddr && !isWddmDisabledScanoutState(words)) return null;
 
          const out = ensureScanoutRgbaCapacity(BYTES_PER_PIXEL_RGBA8);
          if (!out) return null;
          fillRgba8Solid(out.subarray(0, BYTES_PER_PIXEL_RGBA8), 0, 0, 0, 0xff);
          wddmScanoutWidth = 1;
          wddmScanoutHeight = 1;
          const seq = frameState ? Atomics.load(frameState, FRAME_SEQ_INDEX) : 0;
          return {
            width: 1,
            height: 1,
            strideBytes: BYTES_PER_PIXEL_RGBA8,
            pixels: out,
            frameSeq: seq,
            outputSource: "wddm_scanout",
          };
        }
      } catch {
        // Ignore and fall back to the legacy framebuffer sources.
      }
    }
  }

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
      outputSource: "framebuffer",
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
      outputSource: "framebuffer",
    };
  }

  return null;
};

const presentOnce = async (): Promise<boolean> => {
  const t0 = performance.now();
  lastUploadDirtyRects = null;
  lastPresentUploadKind = "none";
  lastPresentUploadDirtyRectCount = 0;

  try {
    const frame = getCurrentFrameInfo();
    const dirtyRects = frame?.dirtyRects ?? null;
    let scanoutSnap: ScanoutStateSnapshot | null = null;
    if (scanoutState) {
      try {
        scanoutSnap = trySnapshotScanoutStateBounded(scanoutState);
      } catch {
        scanoutSnap = null;
      }
    }

    // Back-compat/runtime guard: older unit tests/harnesses may start the GPU worker without
    // providing a VRAM `SharedArrayBuffer`. If WDDM publishes a scanout descriptor pointing into
    // the VRAM aperture, fail fast with a clear error instead of silently presenting garbage.
    if (!vramMissingScanoutErrorSent && scanoutSnap?.source === SCANOUT_SOURCE_WDDM) {
      const baseLo = scanoutSnap.basePaddrLo >>> 0;
      const baseHi = scanoutSnap.basePaddrHi >>> 0;
      if ((baseLo | baseHi) !== 0) {
        const base = (BigInt(baseHi) << 32n) | BigInt(baseLo);
        const vramBase = BigInt(vramBasePaddr >>> 0);
        const vramEnd = vramBase + BigInt(vramSizeBytes >>> 0);
        const baseInVram = vramSizeBytes > 0 && base >= vramBase && base < vramEnd;
        if (baseInVram && !vramU8) {
          vramMissingScanoutErrorSent = true;
          const message =
            "WDDM scanout points into the VRAM aperture, but this GPU worker was started without a shared VRAM buffer. " +
            "Ensure WorkerInitMessage.vram is provided by the coordinator (COOP/COEP + SharedArrayBuffer required).";
          emitGpuEvent({
            time_ms: performance.now(),
            backend_kind: backendKindForEvent(),
            severity: "error",
            category: "Scanout",
            message,
            details: {
              scanout: {
                ...scanoutSnap,
                format_str: aerogpuFormatToString(scanoutSnap.format >>> 0),
              },
              vram_base_paddr: `0x${vramBase.toString(16)}`,
              vram_size_bytes: vramSizeBytes,
            },
          });
          postRuntimeError(message);
        }
      }
    }
    if (isDeviceLost) return false;

    const wddmOwnsScanout =
      scanoutSnap?.source === SCANOUT_SOURCE_WDDM ||
      (!scanoutSnap &&
        (() => {
          const words = scanoutState;
          if (!words) return wddmOwnsScanoutFallback;
          try {
            return (Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0) === SCANOUT_SOURCE_WDDM;
          } catch {
            return wddmOwnsScanoutFallback;
          }
        })());
    const scanoutOwnsOutput =
      wddmOwnsScanout ||
      scanoutSnap?.source === SCANOUT_SOURCE_LEGACY_VBE_LFB ||
      (!scanoutSnap &&
        (() => {
          const words = scanoutState;
          if (!words) return false;
          try {
            return (Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0) === SCANOUT_SOURCE_LEGACY_VBE_LFB;
          } catch {
            return false;
          }
        })()) ||
      frame?.outputSource === "wddm_scanout";

    const clearSharedFramebufferDirty = () => {
      if (!sharedFramebufferViews) return;
      // `frame_dirty` is a producer->consumer "new frame" flag. Clearing it is
      // optional, but doing so allows producers to detect consumer liveness (and
      // some implementations may wait for it).
      //
      const header = sharedFramebufferViews.header;
      if (frame?.sharedLayout) {
        // Normal legacy shared-framebuffer present: avoid clearing a newer frame.
        const seqNow = Atomics.load(header, SharedFramebufferHeaderIndex.FRAME_SEQ);
        if (seqNow !== frame.frameSeq) return;
      } else if (!scanoutOwnsOutput) {
        return;
      }
      Atomics.store(header, SharedFramebufferHeaderIndex.FRAME_DIRTY, 0);
      Atomics.notify(header, SharedFramebufferHeaderIndex.FRAME_DIRTY);
    };

    if (presentFn) {
      const bytesPerRowAlignment = bytesPerRowAlignmentForPresenterBackend(presenter?.backend ?? null);
      const chosenDirtyRects =
        frame?.sharedLayout === undefined
          ? dirtyRects
          : chooseDirtyRectsForUpload(frame.sharedLayout, dirtyRects, bytesPerRowAlignment);
      lastUploadDirtyRects = chosenDirtyRects;
      const predictedKind: GpuRuntimePresentUploadV1["kind"] =
        chosenDirtyRects && chosenDirtyRects.length > 0 ? "dirty_rects" : "full";
      const predictedDirtyCount = chosenDirtyRects ? chosenDirtyRects.length : 0;
      // `presentFn` can be synchronous (returns void/boolean) or async (returns a Promise). Avoid
      // unconditionally `await`ing the result so synchronous presenters can complete in the same
      // task (important for deterministic unit tests + avoids extra microtask churn).
      const maybeResult = presentFn(chosenDirtyRects);
      const result =
        maybeResult && typeof (maybeResult as unknown as { then?: unknown }).then === "function"
          ? await (maybeResult as Promise<void | boolean>)
          : (maybeResult as void | boolean);
      const didPresent = didPresenterPresent(result);
      if (didPresent) {
        lastPresentUploadKind = predictedKind;
        lastPresentUploadDirtyRectCount = predictedKind === "dirty_rects" ? predictedDirtyCount : 0;
        const hasBasePaddr = !!scanoutSnap && ((scanoutSnap.basePaddrLo | scanoutSnap.basePaddrHi) >>> 0) !== 0;
        const wddmDisabled =
          wddmOwnsScanout &&
          !hasBasePaddr &&
          (scanoutSnap ? isWddmDisabledScanoutDescriptor(scanoutSnap) : (scanoutState ? isWddmDisabledScanoutState(scanoutState) : false));
        // In WDDM scanout mode, treat the output as non-legacy so cursor redraw fallback
        // does not clobber the active scanout with a stale shared framebuffer upload.
        aerogpuLastOutputSource = wddmOwnsScanout ? (hasBasePaddr || wddmDisabled ? "wddm_scanout" : "aerogpu") : (frame?.outputSource ?? "framebuffer");
      }
      // Even when a frame is intentionally dropped (e.g. surface timeout/outdated), clear the shared
      // framebuffer dirty flag: the frame was consumed by the worker and retrying immediately can cause
      // tick storms / stall producers. Drops are still accounted for via the returned boolean and worker
      // metrics.
      //
      // However, snapshot pause is a guest-memory access barrier; avoid mutating the shared framebuffer
      // while snapshot save/restore is in progress.
      if (!snapshotPaused) {
        clearSharedFramebufferDirty();
      }
      return didPresent;
    }

    if (presenter) {
      // If scanoutState indicates WDDM owns scanout and we have a most-recent AeroGPU frame,
      // prefer that over the legacy shared framebuffer. This prevents "flash back" to legacy
      // output after WDDM has claimed scanout, matching `docs/16-aerogpu-vga-vesa-compat.md` §5.
      if (wddmOwnsScanout) {
        const hasBasePaddr =
          !!scanoutSnap && ((scanoutSnap.basePaddrLo | scanoutSnap.basePaddrHi) >>> 0) !== 0;
        const wddmDisabled =
          !hasBasePaddr && (scanoutSnap ? isWddmDisabledScanoutDescriptor(scanoutSnap) : (scanoutState ? isWddmDisabledScanoutState(scanoutState) : false));
        // When the scanout descriptor points at a real guest surface (base_paddr != 0),
        // `getCurrentFrameInfo()` will attempt to read/present it directly from guest RAM.
        // Only use the last AeroGPU-presented texture as a fallback for legacy placeholder
        // descriptors (base_paddr == 0) or when `scanoutState` is unavailable.
        if (!hasBasePaddr) {
          if (wddmDisabled) {
            const out = ensureScanoutRgbaCapacity(BYTES_PER_PIXEL_RGBA8);
            if (!out) {
              clearSharedFramebufferDirty();
              return false;
            }
            fillRgba8Solid(out.subarray(0, BYTES_PER_PIXEL_RGBA8), 0, 0, 0, 0xff);
            wddmScanoutWidth = 1;
            wddmScanoutHeight = 1;
            if (1 !== presenterSrcWidth || 1 !== presenterSrcHeight) {
              presenterSrcWidth = 1;
              presenterSrcHeight = 1;
              if (presenter.backend === "webgpu") surfaceReconfigures += 1;
              presenter.resize(1, 1, outputDpr);
              presenterNeedsFullUpload = true;
            }
            const result = presenter.present(out, BYTES_PER_PIXEL_RGBA8);
            presenterNeedsFullUpload = false;
            const didPresent = didPresenterPresent(result);
            if (didPresent) {
              lastPresentUploadKind = "full";
              lastPresentUploadDirtyRectCount = 0;
              aerogpuLastOutputSource = "wddm_scanout";
            }
            clearSharedFramebufferDirty();
            return didPresent;
          }

          const last = aerogpuLastPresentedFrame;
          if (last) {
            if (last.width !== presenterSrcWidth || last.height !== presenterSrcHeight) {
              presenterSrcWidth = last.width;
              presenterSrcHeight = last.height;
              if (presenter.backend === "webgpu") surfaceReconfigures += 1;
              presenter.resize(last.width, last.height, outputDpr);
              presenterNeedsFullUpload = true;
            }

            if (presenterNeedsFullUpload || aerogpuLastOutputSource !== "aerogpu") {
              const result = presenter.present(last.rgba8, last.width * BYTES_PER_PIXEL_RGBA8);
              presenterNeedsFullUpload = false;
              const didPresent = didPresenterPresent(result);
              if (didPresent) {
                lastPresentUploadKind = "full";
                lastPresentUploadDirtyRectCount = 0;
                aerogpuLastOutputSource = "aerogpu";
              }
              // Even when a frame is intentionally dropped (e.g. surface timeout/outdated),
              // clear the shared framebuffer dirty flag: the frame was consumed by the worker
              // and retrying immediately can cause tick storms / stall producers. Drop vs
              // presented is reflected in the returned boolean.
              clearSharedFramebufferDirty();
              return didPresent;
            }
            aerogpuLastOutputSource = "aerogpu";
            clearSharedFramebufferDirty();
            return true;
          }
        }

        // Placeholder descriptor (base_paddr == 0) but we have no AeroGPU pixels yet; do not
        // fall back to the legacy shared framebuffer (that would "steal" scanout).
        if (!hasBasePaddr) {
          clearSharedFramebufferDirty();
          return false;
        }
        // base_paddr != 0: `frame` should already contain the scanout pixels. Fall through to the
        // normal presentation path below.
      }

      if (!frame) {
        if (scanoutOwnsOutput) {
          clearSharedFramebufferDirty();
        }
        return false;
      }

      // If WDDM owns scanout (base_paddr != 0), never present legacy framebuffer bytes.
      if (wddmOwnsScanout && frame.outputSource !== "wddm_scanout") {
        clearSharedFramebufferDirty();
        return false;
      }

      if (frame.width !== presenterSrcWidth || frame.height !== presenterSrcHeight) {
        presenterSrcWidth = frame.width;
        presenterSrcHeight = frame.height;
        if (presenter.backend === "webgpu") surfaceReconfigures += 1;
        presenter.resize(frame.width, frame.height, outputDpr);
        presenterNeedsFullUpload = true;
      }

      const framebufferOutputSource: GpuRuntimeOutputSource = frame.outputSource;

      const needsFullUpload = presenterNeedsFullUpload || aerogpuLastOutputSource !== framebufferOutputSource;
      if (needsFullUpload) {
        const result = presenter.present(frame.pixels, frame.strideBytes);
        presenterNeedsFullUpload = false;
        const didPresent = didPresenterPresent(result);
        if (didPresent) {
          lastPresentUploadKind = "full";
          aerogpuLastOutputSource = framebufferOutputSource;
        }
        // See comment above: clearing dirty on drop avoids tick storms / producer stalls.
        clearSharedFramebufferDirty();
        return didPresent;
      } else if (typeof presenter.presentDirtyRects === "function") {
        const bytesPerRowAlignment = bytesPerRowAlignmentForPresenterBackend(presenter.backend);
        const chosenDirtyRects =
          frame.sharedLayout === undefined
            ? dirtyRects
            : chooseDirtyRectsForUpload(frame.sharedLayout, dirtyRects, bytesPerRowAlignment);
        if (chosenDirtyRects && chosenDirtyRects.length > 0) {
          lastUploadDirtyRects = chosenDirtyRects;
          const result = presenter.presentDirtyRects(frame.pixels, frame.strideBytes, chosenDirtyRects);
          const didPresent = didPresenterPresent(result);
          if (didPresent) {
            lastPresentUploadKind = "dirty_rects";
            lastPresentUploadDirtyRectCount = chosenDirtyRects.length;
            aerogpuLastOutputSource = framebufferOutputSource;
          }
          clearSharedFramebufferDirty();
          return didPresent;
        } else {
          const result = presenter.present(frame.pixels, frame.strideBytes);
          const didPresent = didPresenterPresent(result);
          if (didPresent) {
            lastPresentUploadKind = "full";
            aerogpuLastOutputSource = framebufferOutputSource;
          }
          clearSharedFramebufferDirty();
          return didPresent;
        }
      } else {
        const result = presenter.present(frame.pixels, frame.strideBytes);
        const didPresent = didPresenterPresent(result);
        if (didPresent) {
          lastPresentUploadKind = "full";
          aerogpuLastOutputSource = framebufferOutputSource;
        }
        clearSharedFramebufferDirty();
        return didPresent;
      }
      // Unreachable: all branches return above.
    }

    // Headless: treat as successfully presented so the shared frame state can
    // transition back to PRESENTED and avoid DIRTY→tick spam.
    const scanoutUsesGuestBuffer =
      !!scanoutSnap &&
      scanoutSnap.source === SCANOUT_SOURCE_WDDM &&
      ((scanoutSnap.basePaddrLo | scanoutSnap.basePaddrHi) >>> 0) !== 0 &&
      frame?.outputSource === "wddm_scanout";
    const headlessWddmDisabled =
      wddmOwnsScanout &&
      !scanoutUsesGuestBuffer &&
      (scanoutSnap ? isWddmDisabledScanoutDescriptor(scanoutSnap) : (scanoutState ? isWddmDisabledScanoutState(scanoutState) : false));
    if (scanoutUsesGuestBuffer || headlessWddmDisabled) {
      aerogpuLastOutputSource = "wddm_scanout";
    } else if (wddmOwnsScanout && aerogpuLastPresentedFrame) {
      aerogpuLastOutputSource = "aerogpu";
    } else {
      aerogpuLastOutputSource = frame?.outputSource ?? "framebuffer";
    }
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
  // Legacy fallback: some harnesses do not provide `scanoutState`. Once the AeroGPU command
  // stream starts presenting, treat scanout as WDDM-owned so legacy shared-framebuffer demo
  // frames can't "steal" the output.
  noteWddmScanoutFallback();

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
  lastPresentUploadKind = "full";
};

type AerogpuCmdStreamAnalysis = { requiresD3d9: boolean };

const analyzeAerogpuCmdStream = (cmdStream: ArrayBuffer): AerogpuCmdStreamAnalysis => {
  try {
    const iter = new AerogpuCmdStreamIter(cmdStream);
    let requiresD3d9 = false;

    for (const packet of iter) {
      const opcode = packet.hdr.opcode;
      if (!aerogpuCpuExecutorSupportsOpcode(opcode)) {
        requiresD3d9 = true;
        break;
      }
    }

    return { requiresD3d9 };
  } catch {
    // Malformed streams should not force a wasm executor path selection. (Execution will still
    // proceed via the lightweight CPU executor, which is expected to surface errors without
    // wedging the runtime.)
    return { requiresD3d9: false };
  }
};
const handleSubmitAerogpu = async (req: GpuRuntimeSubmitAerogpuMessage): Promise<void> => {
  const signalFence = typeof req.signalFence === "bigint" ? req.signalFence : BigInt(req.signalFence);
  const cmdAnalysis = analyzeAerogpuCmdStream(req.cmdStream);
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
        vramU8,
        vramBasePaddr,
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
        syncAerogpuWasmMemoryViews(wasm);

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
          noteWddmScanoutFallback();
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
               lastPresentUploadKind = "full";
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

  postAerogpuSubmitComplete({
    requestId: req.requestId,
    completedFence: signalFence,
    ...(presentCount !== undefined ? { presentCount } : {}),
  });
};

const handleTick = async () => {
  beginSnapshotBarrierTask();
  try {
    syncPerfFrame();
    const perfEnabled =
      !!perfWriter && !!perfFrameHeader && Atomics.load(perfFrameHeader, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    if (snapshotPaused) {
      // Snapshot pause is a guest-memory access barrier: avoid touching the shared framebuffer,
      // scanout descriptors, or cursor state until resumed.
      return;
    }
    refreshFramebufferViews();
    maybeUpdateFramesReceivedFromSeq();
    await maybeSendReady();

    if (presenting) {
      maybePostMetrics();
      return;
    }

    if (snapshotPaused) {
      // Snapshot pause must act as a guest-memory access barrier (no scanout/cursor readback).
      // We keep the worker responsive (metrics/heartbeats), but avoid any guest RAM/VRAM touches.
      maybePostMetrics();
      return;
    }

    syncHardwareCursorFromState();

    // When scanout is owned by a ScanoutState-programmed framebuffer (e.g. WDDM scanout or legacy
    // VBE LFB), the frame scheduler may continue ticking even if the legacy shared framebuffer is
    // idle (FRAME_STATUS=PRESENTED). Read the scanout source to decide whether we should run a
    // present pass even when `frameState` isn't DIRTY.
    const scanoutWantsPresentWhenIdle = (() => {
      const words = scanoutState;
      if (!words) return wddmOwnsScanoutFallback;
      try {
        const src = Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
        return src === SCANOUT_SOURCE_WDDM || src === SCANOUT_SOURCE_LEGACY_VBE_LFB;
      } catch {
        return wddmOwnsScanoutFallback;
      }
    })();

    if (frameState) {
      const shouldPresentShared = shouldPresentWithSharedState();
      if (!shouldPresentShared && !scanoutWantsPresentWhenIdle) {
        maybePostMetrics();
        return;
      }

      if (shouldPresentShared) {
        if (!claimPresentWithSharedState()) {
          maybePostMetrics();
          return;
        }
        computeDroppedFromSeqForPresent();
      } else if (scanoutWantsPresentWhenIdle) {
        // No shared framebuffer work is pending, but scanout is ScanoutState-owned. Execute a present
        // pass anyway so the active scanout can be polled/presented and any legacy dirty flags
        // can be cleared. Use a best-effort PRESENTED->PRESENTING transition to avoid overlapping
        // presents / tick spam, but do not disturb DIRTY pacing if a shared frame arrives.
        claimPresentWhileIdleForScanout();
      }
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
      const outcome = presentOutcomeDeltas(didPresent);
      presentsSucceeded += outcome.presentsSucceeded;
      // `framesPresented/framesDropped` count successful/failed *present passes* in this worker,
      // regardless of whether the pixels came from the legacy shared framebuffer or WDDM scanout.
      // When scanout is WDDM-owned, the frame scheduler keeps ticking even if `framesReceived`
      // (shared framebuffer seq) is not advancing, so these counters may diverge.
      framesPresented += outcome.framesPresented;
      framesDropped += outcome.framesDropped;
      if (didPresent) {
        const now = performance.now();
        if (lastFrameStartMs !== null) {
          telemetry.beginFrame(lastFrameStartMs);

          const bytesPerRowAlignment = bytesPerRowAlignmentForPresenterBackend(presenter?.backend ?? null);
          let textureUploadBytes = 0;
          if (lastPresentUploadKind !== "none") {
            switch (aerogpuLastOutputSource) {
              case "aerogpu": {
                const last = aerogpuLastPresentedFrame;
                textureUploadBytes = last
                  ? estimateFullFrameUploadBytes(last.width, last.height, bytesPerRowAlignment)
                  : 0;
                break;
              }
              case "wddm_scanout": {
                const scanout = snapshotScanoutForTelemetry();
                textureUploadBytes = scanout
                  ? estimateFullFrameUploadBytes(scanout.width, scanout.height, bytesPerRowAlignment)
                  : 0;
                break;
              }
              case "framebuffer":
              default: {
                const frame = getCurrentFrameInfo();
                textureUploadBytes = frame?.sharedLayout
                  ? estimateTextureUploadBytes(frame.sharedLayout, lastUploadDirtyRects, bytesPerRowAlignment)
                  : frame
                    ? estimateFullFrameUploadBytes(frame.width, frame.height, bytesPerRowAlignment)
                    : 0;
                break;
              }
            }
          }
          telemetry.recordTextureUploadBytes(textureUploadBytes);
          perf.counter("textureUploadBytes", textureUploadBytes);
          if (perfEnabled) perfUploadBytes += textureUploadBytes;
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
  } finally {
    endSnapshotBarrierTask();
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

  const backend_kind = backend ?? presenter?.backend ?? backendKindForEvent();
  // Best-effort init-vs-runtime classification:
  // - before READY is sent: treat as init failure (often fatal)
  // - after READY: treat as runtime/present failure
  //
  // This avoids misclassifying `presentFn`-mode exceptions as init failures just because the
  // built-in presenter is not in use.
  const isInitFailure = !runtimeReadySent && !!runtimeCanvas;

  // WebGPU validation errors can surface asynchronously as `GPUUncapturedErrorEvent`s.
  // Surface these as structured diagnostics events instead of treating them as fatal worker errors.
  if (err instanceof PresenterError && err.code === "webgpu_uncaptured_error") {
    const cause = err.cause as unknown as { name?: unknown; message?: unknown } | null;
    const causeNameRaw = typeof cause?.name === "string" ? cause.name : "";
    const causeMessageRaw = typeof cause?.message === "string" ? cause.message : "";
    const haystack = `${causeNameRaw} ${causeMessageRaw} ${err.message}`.toLowerCase();

    let category = "Unknown";
    if (haystack.includes("outofmemory") || haystack.includes("out of memory")) {
      category = "OutOfMemory";
    } else if (haystack.includes("device lost") || haystack.includes("devicelost")) {
      category = "DeviceLost";
    } else if (haystack.includes("shader") || haystack.includes("wgsl") || haystack.includes("naga")) {
      category = "ShaderCompile";
    } else if (haystack.includes("pipeline") || haystack.includes("createpipeline") || haystack.includes("renderpipeline")) {
      category = "PipelineCreate";
    } else if (haystack.includes("surface") || haystack.includes("getcurrenttexture") || haystack.includes("canvas")) {
      category = "Surface";
    } else if (haystack.includes("validation")) {
      category = "Validation";
    }

    const severity = presenterErrorToSeverity(err.code, category, { isInitFailure });
    // Dedupe by the error message so distinct uncaptured errors are still surfaced.
    const dedupeKey = `${backend_kind}:${err.code}:${causeNameRaw}:${err.message}`;
    if (shouldEmitPresenterErrorEvent(dedupeKey)) {
      emitGpuEvent({
        time_ms: performance.now(),
        backend_kind,
        severity,
        category,
        message: err.message,
        details: { code: err.code, message: err.message, stack: err.stack, cause: err.cause },
      });
    }
    return;
  }

  let message = err instanceof Error ? err.message : String(err);
  let code: string | null = null;
  let category = isInitFailure ? "Init" : "Unknown";
  let details: unknown | undefined = undefined;

  if (err instanceof PresenterError) {
    message = err.message;
    code = err.code;
    category = presenterErrorCodeToCategory(err.code);
    details = { code: err.code, message: err.message, stack: err.stack, cause: err.cause };
  } else if (err instanceof Error) {
    const anyErr = err as Error & { cause?: unknown };
    details = {
      name: anyErr.name,
      message: anyErr.message,
      stack: anyErr.stack,
      ...(anyErr.cause !== undefined ? { cause: anyErr.cause } : {}),
    };
  } else {
    details = err;
  }

  const severity = presenterErrorToSeverity(code, category, { isInitFailure });
  const dedupeKey =
    err instanceof PresenterError
      ? `${backend_kind}:${err.code}`
      : err instanceof Error
        ? `${backend_kind}:${err.name}:${err.message}`
        : `${backend_kind}:${message}`;

  if (shouldEmitPresenterErrorEvent(dedupeKey)) {
    emitGpuEvent({
      time_ms: performance.now(),
      backend_kind,
      severity,
      category,
      message,
      ...(details === undefined ? {} : { details }),
    });
  }

  if (err instanceof PresenterError) {
    postToMain({ type: "error", message: err.message, code: err.code, backend: backend ?? presenter?.backend });
    postRuntimeError(err.message);
    return;
  }

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
  cursorPresenterLastImageOwner = null;
  latestFrameTimings = null;
  presenterFallback = undefined;
  presenterErrorGeneration += 1;
  const generation = presenterErrorGeneration;
  presenterInitBackendHintGeneration = generation;
  presenterInitBackendHint = null;

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
    // Prefer the raw WebGL2 presenter as the WebGL2 fallback path. The wgpu-backed WebGL2 presenter
    // is useful for exercising the Rust/WASM GPU stack, but it can be less reliable in some
    // headless environments.
    backends = preferWebGpu ? ["webgpu", "webgl2_raw", "webgl2_wgpu"] : ["webgl2_raw", "webgl2_wgpu", "webgpu"];
    if (disableWebGpu && !preferWebGpu) {
      // When WebGPU is disabled and WebGL2 is preferred, never attempt WebGPU.
      backends = ["webgl2_raw", "webgl2_wgpu"];
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

  // For init failure diagnostics, keep track of the backend we were attempting when the error
  // occurred. `postPresenterError()` can use this to populate `backend_kind` even when the
  // presenter was never successfully created.
  presenterInitBackendHint = backends[0] ?? null;

  const firstBackend = backends[0];
  let firstError: unknown | null = null;
  let lastError: unknown | null = null;

  for (const backend of backends) {
    try {
      presenterInitBackendHint = backend;
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
        emitGpuEvent({
          time_ms: performance.now(),
          backend_kind: backend,
          severity: "warn",
          category: "Init",
          message: `GPU backend init fell back from ${firstBackend} to ${backend}`,
          details: { from: firstBackend, to: backend, reason },
        });
      }

      // Warm up the wasm-backed AeroGPU D3D9 executor when we have a wgpu-based presenter so the
      // first submit_aerogpu doesn't pay the init cost.
      if (presenter.backend === "webgpu" || presenter.backend === "webgl2_wgpu") {
        void ensureAerogpuWasmD3d9(presenter.backend)
          .then((wasm) => {
            syncAerogpuWasmMemoryViews(wasm);
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
  if (snapshotPaused) return;

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
        const backendHint =
          presenterInitBackendHintGeneration === presenterErrorGeneration ? presenterInitBackendHint ?? undefined : undefined;
        postPresenterError(err, backendHint);
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
  hwCursorActive = false;
  hwCursorLastGeneration = null;
  hwCursorLastImageKey = null;
  hwCursorLastVramMissingEventKey = null;
  const segments = {
    control: init.controlSab,
    guestMemory: init.guestMemory,
    vram: init.vram,
    scanoutState: init.scanoutState,
    scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
    cursorState: init.cursorState,
    cursorStateOffsetBytes: init.cursorStateOffsetBytes ?? 0,
    ioIpc: init.ioIpcSab,
    sharedFramebuffer: init.sharedFramebuffer,
    sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
  };
  const views = createSharedMemoryViews(segments);
  status = views.status;
  guestLayout = views.guestLayout;
  scanoutState = views.scanoutStateI32 ?? null;
  wddmOwnsScanoutFallback = false;
  (globalThis as unknown as { __aeroScanoutState?: Int32Array }).__aeroScanoutState = scanoutState ?? undefined;
  hwCursorState = views.cursorStateI32 ?? null;
  (globalThis as unknown as { __aeroCursorState?: Int32Array }).__aeroCursorState = hwCursorState ?? undefined;
  // `guestU8` is a flat backing store of guest RAM bytes (length = `guest_size`). On PC/Q35 with
  // ECAM/PCI holes + high-RAM remap, GPAs in AeroGPU submissions are guest *physical* addresses
  // (which may be >=4GiB); executors must translate them back into this view before slicing.
  guestLayout = views.guestLayout;
  guestU8 = views.guestU8;
  guestU32 = new Uint32Array(guestU8.buffer, guestU8.byteOffset, guestU8.byteLength >>> 2);
  vramU8 = views.vramSizeBytes > 0 ? views.vramU8 : null;
  vramU32 = vramU8 ? new Uint32Array(vramU8.buffer, vramU8.byteOffset, vramU8.byteLength >>> 2) : null;
  vramBasePaddr = (init.vramBasePaddr ?? VRAM_BASE_PADDR) >>> 0;
  vramSizeBytes = (init.vramSizeBytes ?? views.vramSizeBytes) >>> 0;
  vramMissingScanoutErrorSent = false;
  if (aerogpuWasm) {
    try {
      // If aero-gpu-wasm is already loaded (e.g. via the webgl2_wgpu presenter), plumb the
      // shared guest RAM view immediately so alloc_table submissions can resolve GPAs.
      syncAerogpuWasmMemoryViews(aerogpuWasm);
    } catch {
      // Ignore; wasm module may not have been initialized yet.
    }
  }

  if (snapshotPaused && snapshotGuestMemoryBackup) {
    // Snapshot pause can race worker init (e.g. coordinator pauses workers while a GPU worker is
    // still starting). If we are already snapshot-paused, treat the freshly initialized guest
    // memory views as the "restored" state and keep guest-memory access disabled until resume.
    //
    // This ensures:
    // - init does not re-enable guest-memory access while paused, and
    // - vm.snapshot.resume restores the correct (new) views instead of the pre-init null backup.
    snapshotGuestMemoryBackup.guestU8 = guestU8;
    snapshotGuestMemoryBackup.guestU32 = guestU32;
    snapshotGuestMemoryBackup.vramU8 = vramU8;
    snapshotGuestMemoryBackup.vramU32 = vramU32;
    snapshotGuestMemoryBackup.scanoutState = scanoutState;
    snapshotGuestMemoryBackup.hwCursorState = hwCursorState;
    snapshotGuestMemoryBackup.sharedFramebufferViews = sharedFramebufferViews;
    snapshotGuestMemoryBackup.sharedFramebufferLayoutKey = sharedFramebufferLayoutKey;
    snapshotGuestMemoryBackup.framebufferProtocolViews = framebufferProtocolViews;
    snapshotGuestMemoryBackup.framebufferProtocolLayoutKey = framebufferProtocolLayoutKey;

    guestU8 = null;
    guestU32 = null;
    vramU8 = null;
    vramU32 = null;
    scanoutState = null;
    (globalThis as unknown as { __aeroScanoutState?: Int32Array }).__aeroScanoutState = undefined;
    hwCursorState = null;
    (globalThis as unknown as { __aeroCursorState?: Int32Array }).__aeroCursorState = undefined;
    sharedFramebufferViews = null;
    sharedFramebufferLayoutKey = null;
    framebufferProtocolViews = null;
    framebufferProtocolLayoutKey = null;
    (globalThis as unknown as { __aeroSharedFramebuffer?: SharedFramebufferViews }).__aeroSharedFramebuffer = undefined;

    if (aerogpuWasm) {
      try {
        syncAerogpuWasmMemoryViews(aerogpuWasm);
      } catch {
        // Ignore; wasm module may not have been initialized yet.
      }
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

  const snapshotMsg = data as Partial<CoordinatorToWorkerSnapshotMessage>;
  if (typeof snapshotMsg.kind === "string" && snapshotMsg.kind.startsWith("vm.snapshot.")) {
    const requestId = snapshotMsg.requestId;
    if (typeof requestId !== "number") return;
    switch (snapshotMsg.kind) {
      case "vm.snapshot.pause": {
        void (async () => {
          try {
            await ensureSnapshotPaused();
            ctx.postMessage({ kind: "vm.snapshot.paused", requestId, ok: true } satisfies VmSnapshotPausedMessage);
          } catch (err) {
            ctx.postMessage({
              kind: "vm.snapshot.paused",
              requestId,
              ok: false,
              error: serializeVmSnapshotError(err),
            } satisfies VmSnapshotPausedMessage);
          }
        })();
        return;
      }
      case "vm.snapshot.resume": {
        try {
          handleSnapshotResume();
          ctx.postMessage({ kind: "vm.snapshot.resumed", requestId, ok: true } satisfies VmSnapshotResumedMessage);
        } catch (err) {
          ctx.postMessage({
            kind: "vm.snapshot.resumed",
            requestId,
            ok: false,
            error: serializeVmSnapshotError(err),
          } satisfies VmSnapshotResumedMessage);
        }
        return;
      }
      default:
        return;
    }
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
        // `scanoutState` is provided via the runtime `WorkerInitMessage` (coordinator protocol),
        // not the gpu-protocol `GpuRuntimeInitMessage`. Preserve any existing scanout wiring so
        // WDDM scanout continues to work when the worker is used in the full runtime.
        //
        // (Legacy smoke harnesses that do not provide `scanoutState` will continue to rely on the
        // `wddmOwnsScanoutFallback` heuristic.)
        wddmOwnsScanoutFallback = false;
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
        recoveriesAttemptedWddm = 0;
        recoveriesSucceededWddm = 0;
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
        cursorPresenterLastImageOwner = null;
        latestFrameTimings = null;
        presenterFallback = undefined;
        presenterInitPromise = null;
        presenterSrcWidth = 0;
        presenterSrcHeight = 0;
        presenterNeedsFullUpload = true;

        resetAerogpuContexts();
        aerogpuLastOutputSource = "framebuffer";
        wddmScanoutRgba = null;
        wddmScanoutWidth = 0;
        wddmScanoutHeight = 0;
        wddmScanoutFormat = null;
        wddmScanoutRgbaCapacity = 0;
        wddmScanoutRgbaU32 = null;
        lastScanoutReadbackErrorGeneration = null;
        lastScanoutReadbackErrorReason = null;
        // Reset wasm-backed executor state (if it was used previously).
        aerogpuWasmD3d9InitPromise = null;
        aerogpuWasmD3d9InitBackend = null;
        aerogpuWasmD3d9Backend = null;
        aerogpuWasmD3d9InternalCanvas = null;
        if (aerogpuWasm) {
          try {
            aerogpuWasm.clear_guest_memory();
            aerogpuWasm.clear_vram_memory();
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
              syncAerogpuWasmMemoryViews(wasm);
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
        aerogpuSubmitInFlight = null;
        cursorImage = null;
        cursorWidth = 0;
        cursorHeight = 0;
        cursorPresenterLastImageOwner = null;
        cursorPresenterLastImage = null;
        cursorPresenterLastImageWidth = 0;
        cursorPresenterLastImageHeight = 0;
        cursorEnabled = false;
        cursorX = 0;
        cursorY = 0;
        cursorHotX = 0;
        cursorHotY = 0;
        cursorRenderEnabled = true;
        hwCursorActive = false;
        hwCursorLastGeneration = null;
        hwCursorLastImageKey = null;
        hwCursorLastVramMissingEventKey = null;

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
      if (!snapshotPaused) {
        void handleTick();
      }
      break;
    }

    case "debug_context_loss": {
      // Dev-only harness hook: simulate context loss/restoration for the raw WebGL2 backend.
      // Production builds may ignore this message.
      const env = (import.meta as unknown as { env?: unknown }).env;
      const isDev = typeof env === "object" && env !== null && (env as Record<string, unknown>).DEV === true;
      if (!isDev) break;
      const action = (msg as { action?: unknown }).action;
      if (action !== "lose" && action !== "restore") break;
      if (presenter?.backend !== "webgl2_raw") break;
      const raw = presenter as unknown as {
        debugLoseContext?: () => boolean;
        debugRestoreContext?: () => boolean;
      };
      try {
        const ok = action === "lose" ? raw.debugLoseContext?.() : raw.debugRestoreContext?.();
        emitGpuEvent({
          time_ms: performance.now(),
          backend_kind: backendKindForEvent(),
          severity: "info",
          category: "Debug",
          message: `debug_context_loss: action=${action} ok=${String(ok ?? false)}`,
        });
      } catch (err) {
        emitGpuEvent({
          time_ms: performance.now(),
          backend_kind: backendKindForEvent(),
          severity: "warn",
          category: "Debug",
          message: `debug_context_loss failed: ${err instanceof Error ? err.message : String(err)}`,
        });
      }
      break;
    }

    case "submit_aerogpu": {
      const req = msg as GpuRuntimeSubmitAerogpuMessage;
      aerogpuSubmitChain = aerogpuSubmitChain
        .catch(() => {
          // Ensure a previous failed submission does not permanently stall the chain.
        })
        .then(() => {
          const task = (async () => {
            // Snapshot pause acts as a barrier: do not execute any new ACMD work (which may
            // read/write guest RAM/VRAM) until the coordinator resumes the GPU worker.
            await waitUntilSnapshotResumed();
            await handleSubmitAerogpu(req);
          })();
          aerogpuSubmitInFlight = task;
          return task.finally(() => {
            if (aerogpuSubmitInFlight === task) {
              aerogpuSubmitInFlight = null;
            }
          });
        });
      break;
    }

    case "screenshot": {
      const req = msg as GpuRuntimeScreenshotRequestMessage;
      void (async () => {
        beginSnapshotBarrierTask();
        try {
          const postStub = (seq?: number) => {
            const rgba8 = new ArrayBuffer(4);
            new Uint8Array(rgba8).set([0, 0, 0, 255]);
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

            if (snapshotPaused) {
              // Snapshot pause must not touch guest RAM/VRAM. Respond with a stub screenshot (the
              // caller can retry after resume if desired).
              const seqNow = frameState ? lastPresentedSeq : undefined;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }
            const includeCursor = req.includeCursor === true;

          // Ensure the screenshot corresponds to the latest frame the presenter has actually
          // consumed. The shared framebuffer producer can advance `frameSeq` before the presenter
          // runs, so relying on the header sequence alone can lead to mismatched (seq, pixels)
          // pairs in smoke tests and automation.
          const scanoutSource = (() => {
            const words = scanoutState;
            if (!words) return wddmOwnsScanoutFallback ? SCANOUT_SOURCE_WDDM : null;
            try {
              return Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
            } catch {
              return null;
            }
          })();
          const scanoutWantsTickForScreenshot =
            scanoutSource === SCANOUT_SOURCE_WDDM || scanoutSource === SCANOUT_SOURCE_LEGACY_VBE_LFB;
          if (frameState) {
            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }

            if (!isDeviceLost) {
              // Ensure a present pass runs before readback so the screenshot reflects the pixels that are
              // actually visible (last uploaded to the presenter), especially when scanout is WDDM-owned
              // and the legacy shared framebuffer is idle.
              const shouldForceTick =
                scanoutWantsTickForScreenshot ||
                (aerogpuLastOutputSource === "framebuffer" && shouldPresentWithSharedState());
              if (shouldForceTick) {
                await handleTick();
              }
            }

            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }
          }

          if (includeCursor) {
            // `cursorEnabled/cursorImage` are normally kept in sync with the shared CursorState
            // descriptor during `handleTick()`. However, screenshot requests can arrive while the
            // frame scheduler is idle (no ticks/presents), so we must explicitly sync the hardware
            // cursor state here to ensure software cursor composition is up to date.
            //
            // This is bounded (uses `trySnapshotCursorState`) and should not block indefinitely.
            if (!presenting) {
              syncHardwareCursorFromState();
            }
          }

          const seq = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;

            const tryPostWddmScanoutScreenshot = (): boolean => {
              const words = scanoutState;
              if (!words) return false;

              let snap: ScanoutStateSnapshot | null;
              try {
                snap = trySnapshotScanoutStateBounded(words);
              } catch {
                snap = null;
              }

              if (!snap) {
                // If scanout is WDDM-owned / VBE-owned and base_paddr is non-zero, but the scanout
                // descriptor cannot be snapshotted, treat it as unreadable and return the
                // stub rather than falling back to legacy framebuffer paths.
                try {
                  const source = Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
                  if (source === SCANOUT_SOURCE_WDDM || source === SCANOUT_SOURCE_LEGACY_VBE_LFB) {
                    const lo = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_LO) >>> 0;
                    const hi = Atomics.load(words, ScanoutStateIndex.BASE_PADDR_HI) >>> 0;
                    if (((lo | hi) >>> 0) !== 0) {
                      postStub(typeof seq === "number" ? seq : undefined);
                      return true;
                    }
                    if (source === SCANOUT_SOURCE_WDDM && isWddmDisabledScanoutState(words)) {
                      postStub(typeof seq === "number" ? seq : undefined);
                      return true;
                    }
                  }
                } catch {
                  // Ignore and fall through to the legacy screenshot paths.
                }
                return false;
              }

              const source = snap.source >>> 0;
              const scanoutIsWddm = source === SCANOUT_SOURCE_WDDM;
              const scanoutIsVbe = source === SCANOUT_SOURCE_LEGACY_VBE_LFB;
              if (!scanoutIsWddm && !scanoutIsVbe) return false;

              const hasBasePaddr = ((snap.basePaddrLo | snap.basePaddrHi) >>> 0) !== 0;
              // WDDM uses base_paddr=0 as a placeholder descriptor for the host-side AeroGPU path.
              if (scanoutIsWddm && !hasBasePaddr) {
                if (isWddmDisabledScanoutDescriptor(snap)) {
                  postStub(typeof seq === "number" ? seq : undefined);
                  return true;
                }
                return false;
              }
              if (scanoutIsVbe && !hasBasePaddr) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              const guest = guestU8;
              if (!guest) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }
              const vram = vramU8;

              try {
              const width = snap.width >>> 0;
              const height = snap.height >>> 0;
              const pitchBytes = snap.pitchBytes >>> 0;
              const format = snap.format >>> 0;
              const basePaddr = (BigInt(snap.basePaddrHi >>> 0) << 32n) | BigInt(snap.basePaddrLo >>> 0);

              if (width === 0 || height === 0 || basePaddr === 0n) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              // Guard against corrupt descriptors that would otherwise trigger huge loops even if the
              // total byte count is still under the MAX_SCREENSHOT_BYTES cap (e.g. width=1, height=64M).
              const MAX_SCANOUT_DIM = 16384;
              if (width > MAX_SCANOUT_DIM || height > MAX_SCANOUT_DIM) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              // Supported scanout formats (AeroGPU formats).
              const srcBytesPerPixel = (() => {
                switch (format) {
                  case SCANOUT_FORMAT_B8G8R8X8:
                  case SCANOUT_FORMAT_B8G8R8X8_SRGB:
                  case SCANOUT_FORMAT_B8G8R8A8:
                  case SCANOUT_FORMAT_B8G8R8A8_SRGB:
                  case SCANOUT_FORMAT_R8G8B8A8:
                  case SCANOUT_FORMAT_R8G8B8A8_SRGB:
                  case SCANOUT_FORMAT_R8G8B8X8:
                  case SCANOUT_FORMAT_R8G8B8X8_SRGB:
                    return 4;
                  case SCANOUT_FORMAT_B5G6R5:
                  case SCANOUT_FORMAT_B5G5R5A1:
                    return 2;
                  default:
                    return 0;
                }
              })();
              if (srcBytesPerPixel === 0) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              const srcRowBytes = width * srcBytesPerPixel;
              if (!Number.isSafeInteger(srcRowBytes) || srcRowBytes <= 0) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }
              if (pitchBytes < srcRowBytes) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              const rowBytes = width * BYTES_PER_PIXEL_RGBA8;
              if (!Number.isSafeInteger(rowBytes) || rowBytes <= 0) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              const outBytes = tryComputeScanoutRgba8ByteLength(width, height, MAX_SCANOUT_RGBA8_BYTES);
              if (outBytes === null) {
                postStub(typeof seq === "number" ? seq : undefined);
                return true;
              }

              // Determine whether the scanout surface is backed by VRAM (BAR1 aperture) or guest RAM.
              // This affects which readback helper we can use.
              const scanoutIsInVram = (() => {
                if (!vram || vramSizeBytes === 0) return false;

                // The scanout surface occupies `srcRowBytes` bytes on the last row, not the full `pitchBytes`.
                // This matches `tryReadScanoutRgba8` (and typical linear framebuffer semantics): the
                // framebuffer byte length is `(height-1)*pitchBytes + srcRowBytes`.
                const requiredSrcBytesBig = (BigInt(height) - 1n) * BigInt(pitchBytes) + BigInt(srcRowBytes);
                if (requiredSrcBytesBig > BigInt(Number.MAX_SAFE_INTEGER)) return false;
                const requiredSrcBytes = Number(requiredSrcBytesBig);

                const vramBase = BigInt(vramBasePaddr >>> 0);
                const vramEnd = vramBase + BigInt(vramSizeBytes >>> 0);
                const endPaddr = basePaddr + requiredSrcBytesBig;
                if (basePaddr < vramBase || endPaddr > vramEnd) return false;

                const startBig = basePaddr - vramBase;
                if (startBig > BigInt(Number.MAX_SAFE_INTEGER)) return false;
                const start = Number(startBig);

                const end = start + requiredSrcBytes;
                if (end < start || end > vram.byteLength) return false;
                return true;
              })();

              const helperCompatibleFormat =
                format === SCANOUT_FORMAT_B8G8R8X8 ||
                format === SCANOUT_FORMAT_B8G8R8X8_SRGB ||
                format === SCANOUT_FORMAT_B8G8R8A8 ||
                format === SCANOUT_FORMAT_B8G8R8A8_SRGB ||
                format === SCANOUT_FORMAT_B5G6R5 ||
                format === SCANOUT_FORMAT_B5G5R5A1 ||
                format === SCANOUT_FORMAT_R8G8B8A8 ||
                format === SCANOUT_FORMAT_R8G8B8A8_SRGB ||
                format === SCANOUT_FORMAT_R8G8B8X8 ||
                format === SCANOUT_FORMAT_R8G8B8X8_SRGB;

              // Screenshot buffers must be transferable to the main thread, which means the
              // backing store must be an `ArrayBuffer` (not a `SharedArrayBuffer`).
              let out: Uint8Array<ArrayBuffer>;
              if (!scanoutIsInVram && helperCompatibleFormat) {
                if (!guest) {
                  postStub(typeof seq === "number" ? seq : undefined);
                  return true;
                }

                // Guest RAM-backed scanout: use the unit-tested helper (handles guest paddr translation,
                // pitch padding, and BGRA/BGRX/RGBA/RGBX swizzle + alpha policy).
                out = readScanoutRgba8FromGuestRam(guest, { basePaddr, width, height, pitchBytes, format }).rgba8;
                // `readScanoutRgba8FromGuestRam` returns bytes in the source scanout color space.
                // For sRGB scanout formats, decode to linear so the screenshot buffer matches the
                // worker's linear RGBA8 presentation path (and cursor blending semantics).
                if (
                  format === SCANOUT_FORMAT_B8G8R8X8_SRGB ||
                  format === SCANOUT_FORMAT_B8G8R8A8_SRGB ||
                  format === SCANOUT_FORMAT_R8G8B8A8_SRGB ||
                  format === SCANOUT_FORMAT_R8G8B8X8_SRGB
                ) {
                  linearizeSrgbRgba8InPlace(out);
                }
              } else {
                if (!scanoutIsInVram) {
                  if (!guest) {
                    postStub(typeof seq === "number" ? seq : undefined);
                    return true;
                  }

                  // Validate that the descriptor rows are actually backed by guest RAM before using the cached
                  // last-presented scanout buffer. Without this, a corrupt `base_paddr` could cause us to return
                  // stale cached pixels instead of falling back to the stub.
                  if (basePaddr > BigInt(Number.MAX_SAFE_INTEGER)) {
                    postStub(typeof seq === "number" ? seq : undefined);
                    return true;
                  }
                  const basePaddrNum = Number(basePaddr);

                  const ramBytes = guest.byteLength;
                  for (let y = 0; y < height; y += 1) {
                    const rowPaddr = basePaddrNum + y * pitchBytes;
                    if (!Number.isSafeInteger(rowPaddr)) {
                      postStub(typeof seq === "number" ? seq : undefined);
                      return true;
                    }
                    try {
                      if (!guestRangeInBoundsRaw(ramBytes, rowPaddr, srcRowBytes)) {
                        postStub(typeof seq === "number" ? seq : undefined);
                        return true;
                      }
                    } catch {
                      postStub(typeof seq === "number" ? seq : undefined);
                      return true;
                    }
                    const rowOff = guestPaddrToRamOffsetRaw(ramBytes, rowPaddr);
                    if (rowOff === null || rowOff + srcRowBytes > guest.byteLength) {
                      postStub(typeof seq === "number" ? seq : undefined);
                      return true;
                    }
                  }
                }

                // Prefer the cached last-presented scanout buffer when available so the screenshot bytes
                // match what the presenter most recently consumed (important for deterministic hashing).
                //
                // Fall back to re-reading scanout memory if the cache is unavailable or mismatched.
                const cached = wddmScanoutRgba;
                if (
                  cached &&
                  wddmScanoutWidth === width &&
                  wddmScanoutHeight === height &&
                  wddmScanoutFormat === format &&
                  cached.byteLength >= outBytes
                ) {
                  out = cached.subarray(0, outBytes).slice();
                } else {
                  const frame = tryReadScanoutFrame(snap);
                  if (!frame || frame.width !== width || frame.height !== height || frame.pixels.byteLength < outBytes) {
                    postStub(typeof seq === "number" ? seq : undefined);
                    return true;
                  }
                  out = frame.pixels.subarray(0, outBytes).slice();
                }
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

              const rgba8 = toTransferableArrayBuffer(out);
              postToMain(
                {
                  type: "screenshot",
                  requestId: req.requestId,
                  width,
                  height,
                  rgba8,
                  origin: "top-left",
                  ...(typeof seq === "number" ? { frameSeq: seq } : {}),
                },
                [rgba8],
              );
              return true;
            } catch {
              // If scanout was WDDM-owned but we couldn't read/convert the buffer, do not throw.
              postStub(typeof seq === "number" ? seq : undefined);
              return true;
            }
          };

          if (tryPostWddmScanoutScreenshot()) return;

          // Screenshot contract note:
          // The worker-level screenshot API is used for deterministic hashing in tests, so it is
          // defined in terms of the *source framebuffer* bytes (pre-scaling / pre-color-space).
          //
          // For the wgpu WebGL2 presenter presenting a shared framebuffer, we can satisfy that
          // contract more directly (and deterministically) by copying out of the shared buffer,
          // avoiding any ambiguity around "presented output" readback.
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

            const rgba8 = toTransferableArrayBuffer(out);
            postToMain(
              {
                type: "screenshot",
                requestId: req.requestId,
                width,
                height,
                rgba8,
                origin: "top-left",
                ...(typeof frameSeq === "number" ? { frameSeq } : {}),
              },
              [rgba8],
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
              } else if (aerogpuLastOutputSource === "wddm_scanout") {
                let snap: ScanoutStateSnapshot | null = null;
                if (scanoutState) {
                  try {
                    snap = trySnapshotScanoutStateBounded(scanoutState);
                  } catch {
                    snap = null;
                  }
                }

                if (snap?.source === SCANOUT_SOURCE_WDDM && (snap.basePaddrLo | snap.basePaddrHi) !== 0) {
                  const shot = tryReadScanoutRgba8(snap);
                  if (shot) {
                    if (shot.width !== presenterSrcWidth || shot.height !== presenterSrcHeight) {
                      presenterSrcWidth = shot.width;
                      presenterSrcHeight = shot.height;
                      if (presenter.backend === "webgpu") surfaceReconfigures += 1;
                      presenter.resize(shot.width, shot.height, outputDpr);
                      presenterNeedsFullUpload = true;
                    }
                    presenter.present(shot.rgba8, shot.strideBytes);
                    presenterNeedsFullUpload = false;
                    aerogpuLastOutputSource = "wddm_scanout";
                  }
                } else {
                  // If scanoutState is unavailable/unreadable, fall back to the most recent
                  // successful WDDM readback (best-effort).
                  const lastScanout = wddmScanoutRgba;
                  if (lastScanout && wddmScanoutWidth > 0 && wddmScanoutHeight > 0) {
                    if (wddmScanoutWidth !== presenterSrcWidth || wddmScanoutHeight !== presenterSrcHeight) {
                      presenterSrcWidth = wddmScanoutWidth;
                      presenterSrcHeight = wddmScanoutHeight;
                      if (presenter.backend === "webgpu") surfaceReconfigures += 1;
                      presenter.resize(wddmScanoutWidth, wddmScanoutHeight, outputDpr);
                      presenterNeedsFullUpload = true;
                    }
                    presenter.present(lastScanout, wddmScanoutWidth * BYTES_PER_PIXEL_RGBA8);
                    presenterNeedsFullUpload = false;
                    aerogpuLastOutputSource = "wddm_scanout";
                  }
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
                  aerogpuLastOutputSource = frame.outputSource;
                  presenterNeedsFullUpload = false;
                }
              }
            }

            const shot = await presenter.screenshot();
            let pixels = shot.pixels;

            // WebGPU, the wgpu-backed WebGL2 presenter, and the raw WebGL2 presenter all read back
            // the *source texture* only (not the presented/canvas output). Cursor composition must
            // therefore be applied explicitly when requested.
            if (
              includeCursor &&
              (presenter.backend === "webgpu" || presenter.backend === "webgl2_wgpu" || presenter.backend === "webgl2_raw")
            ) {
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
            if (scanoutState) {
              let snap: ScanoutStateSnapshot | null = null;
              try {
                snap = trySnapshotScanoutStateBounded(scanoutState);
              } catch {
                snap = null;
              }
              if (snap?.source === SCANOUT_SOURCE_WDDM && (snap.basePaddrLo | snap.basePaddrHi) !== 0) {
                const shot = tryReadScanoutRgba8(snap);
                if (shot) {
                  const expectedBytes = shot.strideBytes * shot.height;
                  const outView = shot.rgba8.slice(0, expectedBytes);
                  if (includeCursor) {
                    try {
                      compositeCursorOverRgba8(
                        outView,
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
                    } catch {
                      // Ignore; screenshot cursor compositing is best-effort.
                    }
                  }
                  const rgba8 = toTransferableArrayBuffer(outView);
                  postToMain(
                    {
                      type: "screenshot",
                      requestId: req.requestId,
                      width: shot.width,
                      height: shot.height,
                      rgba8,
                      origin: "top-left",
                      ...(typeof seq === "number" ? { frameSeq: seq } : {}),
                    },
                    [rgba8],
                  );
                  return;
                }
              }
            }

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

            const rgba8 = toTransferableArrayBuffer(out);
            postToMain(
              {
                type: "screenshot",
                requestId: req.requestId,
                width: frame.width,
                height: frame.height,
                rgba8,
                origin: "top-left",
                frameSeq: frame.frameSeq,
              },
              [rgba8],
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
        } finally {
          endSnapshotBarrierTask();
        }
      })();
      break;
    }

    case "screenshot_presented": {
      const req = msg as GpuRuntimeScreenshotPresentedRequestMessage;
      void (async () => {
        // `screenshot_presented` is debug-only: it attempts to read back the final pixels that were
        // rendered to the canvas (post-scaling/letterboxing, post-sRGB/alpha policy, etc).
        //
        // This is intentionally separate from `screenshot`, which is defined as a deterministic
        // readback of the source framebuffer bytes for hashing/tests.
        //
        // Best-effort: if the active presenter backend does not implement presented readback yet,
        // we fall back to `presenter.screenshot()` (source bytes).
        beginSnapshotBarrierTask();
        try {
          const postStub = (seq?: number) => {
            const rgba8 = new ArrayBuffer(4);
            new Uint8Array(rgba8).set([0, 0, 0, 255]);
            postToMain(
              {
                type: "screenshot_presented",
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
              await Promise.race([
                recoveryPromise,
                new Promise((resolve) => setTimeout(resolve, 750)),
              ]);
              await maybeSendReady();
            }

            if (snapshotPaused) {
              // Snapshot pause must not touch guest RAM/VRAM. Respond with a stub screenshot (the
              // caller can retry after resume if desired).
              const seqNow = frameState ? lastPresentedSeq : undefined;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }

            const includeCursor = req.includeCursor === true;

          // Similar to the deterministic `screenshot` path, ensure we run a present pass when scanout is
          // WDDM/VBE-owned so the presented output (canvas pixels) reflects the latest scanout bytes
          // before we attempt readback.
          const scanoutSource = (() => {
            const words = scanoutState;
            if (!words) return wddmOwnsScanoutFallback ? SCANOUT_SOURCE_WDDM : null;
            try {
              return Atomics.load(words, ScanoutStateIndex.SOURCE) >>> 0;
            } catch {
              return null;
            }
          })();
          const scanoutWantsTickForScreenshot =
            scanoutSource === SCANOUT_SOURCE_WDDM || scanoutSource === SCANOUT_SOURCE_LEGACY_VBE_LFB;

          if (frameState) {
            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }

            // Best-effort: ensure the worker has presented the latest dirty frame before readback.
            if (!isDeviceLost) {
              const shouldForceTick =
                scanoutWantsTickForScreenshot ||
                (aerogpuLastOutputSource === "framebuffer" && shouldPresentWithSharedState());
              if (shouldForceTick) {
                await handleTick();
              }
            }

            if (!(await waitForNotPresenting(1000))) {
              const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
              postStub(typeof seqNow === "number" ? seqNow : undefined);
              return;
            }
          }

          if (includeCursor) {
            // CursorState polling normally happens on `tick()`. Ensure the presented screenshot uses the
            // latest cursor image/state even if this request races with the frame scheduler.
            if (!presenting) {
              syncHardwareCursorFromState();
            }
          }

          const seq = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;

          if (!runtimeCanvas || !presenter || isDeviceLost) {
            postStub(typeof seq === "number" ? seq : undefined);
            return;
          }

          const prevCursorRenderEnabled = cursorRenderEnabled;
          if (!includeCursor && cursorRenderEnabled) {
            cursorRenderEnabled = false;
            syncCursorToPresenter();
          }

          try {
            // Capture a snapshot of the presenter reference so we don't race with teardown/re-init.
            const p = presenter;
            if (!p) {
              throw new PresenterError("not_initialized", "Presenter not initialized");
            }
            const shot = p.screenshotPresented ? await p.screenshotPresented() : await p.screenshot();

            postToMain(
              {
                type: "screenshot_presented",
                requestId: req.requestId,
                width: shot.width,
                height: shot.height,
                rgba8: shot.pixels,
                origin: "top-left",
                ...(typeof seq === "number" ? { frameSeq: seq } : {}),
              },
              [shot.pixels],
            );
          } finally {
            if (!includeCursor && cursorRenderEnabled !== prevCursorRenderEnabled) {
              cursorRenderEnabled = prevCursorRenderEnabled;
              syncCursorToPresenter();
              getCursorPresenter()?.redraw?.();
            }
          }
          } catch (err) {
            const seqNow = frameState ? lastPresentedSeq : getCurrentFrameInfo()?.frameSeq;
            const deviceLostCode = getDeviceLostCode(err);
            if (deviceLostCode) {
              const startRecovery = deviceLostCode !== "webgl_context_lost";
              handleDeviceLost(
                err instanceof Error ? err.message : String(err),
                { source: "screenshot_presented", code: deviceLostCode, error: err },
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
        } finally {
          endSnapshotBarrierTask();
        }
      })();
      break;
    }

    case "cursor_set_image": {
      const req = msg as GpuRuntimeCursorSetImageMessage;
      if (hwCursorActive) {
        // Hardware cursor state is authoritative once active; ignore legacy messages to
        // avoid flicker when both sources are present.
        break;
      }
      const w = Math.max(0, req.width | 0);
      const h = Math.max(0, req.height | 0);
      if (w === 0 || h === 0) {
        postPresenterError(new PresenterError("invalid_cursor_image", "cursor_set_image width/height must be non-zero"));
        break;
      }

      const requiredBytes = w * h * BYTES_PER_PIXEL_RGBA8;
      if (req.rgba8.byteLength < requiredBytes) {
        postPresenterError(
          new PresenterError(
            "invalid_cursor_image",
            `cursor_set_image rgba8 buffer too small: expected at least ${requiredBytes} bytes, got ${req.rgba8.byteLength}`,
          ),
        );
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
      if (hwCursorActive) {
        break;
      }
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
      cursorPresenterLastImageOwner = null;
      runtimeInit = null;
      runtimeCanvas = null;
      runtimeOptions = null;
      runtimeReadySent = false;
      resetAerogpuContexts();
      aerogpuLastOutputSource = "framebuffer";
      wddmScanoutRgba = null;
      wddmScanoutWidth = 0;
      wddmScanoutHeight = 0;
      wddmScanoutFormat = null;
      wddmScanoutRgbaCapacity = 0;
      wddmScanoutRgbaU32 = null;
      lastScanoutReadbackErrorGeneration = null;
      lastScanoutReadbackErrorReason = null;
      aerogpuWasmD3d9InitPromise = null;
      aerogpuWasmD3d9InitBackend = null;
      aerogpuWasmD3d9Backend = null;
      aerogpuWasmD3d9InternalCanvas = null;
      presenterNeedsFullUpload = true;
      if (aerogpuWasm) {
        try {
          aerogpuWasm.clear_guest_memory();
          aerogpuWasm.clear_vram_memory();
        } catch {
          // Ignore; best-effort cleanup.
        }
        try {
          aerogpuWasm.destroy_gpu();
        } catch {
          // Ignore.
        }
      }
      ctx.close();
      break;
    }
  }
};

void currentConfig;
