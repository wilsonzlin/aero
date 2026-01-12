/// <reference lib="webworker" />

import type { AeroConfig } from "../config/aero_config";
import { perf } from "../perf/perf";
import { installWorkerPerfHandlers } from "../perf/worker";
import { PerfWriter } from "../perf/writer.js";
import { PERF_FRAME_HEADER_ENABLED_INDEX, PERF_FRAME_HEADER_FRAME_ID_INDEX } from "../perf/shared.js";
import { RingBuffer } from "../ipc/ring_buffer";
import { decodeCommand, encodeEvent, type Command, type Event } from "../ipc/protocol";
import { StatusIndex, createSharedMemoryViews, ringRegionsForWorker, setReadyFlag, type WorkerRole } from "../runtime/shared_layout";
import {
  type ConfigAckMessage,
  type ConfigUpdateMessage,
  MessageType,
  type ProtocolMessage,
  type WorkerInitMessage,
} from "../runtime/protocol";
import { fnv1a32Hex } from "../utils/fnv1a";
import { type JitCompileRequest, type JitWorkerResponse, isJitCompileRequest } from "./jit_protocol";

const ctx = self as unknown as DedicatedWorkerGlobalScope;

void installWorkerPerfHandlers();

let role: WorkerRole = "jit";
let status!: Int32Array;
let commandRing!: RingBuffer;
let eventRing: RingBuffer | null = null;

let currentConfig: AeroConfig | null = null;
let currentConfigVersion = 0;

// Empty (but valid) WASM module: just the header.
const WASM_EMPTY_MODULE_BYTES = new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]);

function detectDynamicWasmCompilation(): boolean {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Module !== "function") {
    return false;
  }
  try {
    new WebAssembly.Module(WASM_EMPTY_MODULE_BYTES);
    return true;
  } catch {
    return false;
  }
}

type JitPlatformFeatures = {
  /**
   * Whether dynamic WebAssembly compilation is allowed in this worker context.
   *
   * Primarily gated by CSP (`script-src 'wasm-unsafe-eval'`).
   */
  jit_dynamic_wasm: boolean;
};

let currentPlatformFeatures: JitPlatformFeatures = { jit_dynamic_wasm: detectDynamicWasmCompilation() };
let jitEnabled = false;

const JIT_CACHE_MAX_ENTRIES = 64;
const moduleCache = new Map<string, WebAssembly.Module>();
type InflightCompile = {
  promise: Promise<WebAssembly.Module>;
  startMs: number;
  endMs: number | null;
};

const inflightCompiles = new Map<string, InflightCompile>();

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function maybeUpdatePlatformFeatures(msg: unknown): void {
  if (!isRecord(msg)) return;
  const provided = (msg as { platformFeatures?: unknown }).platformFeatures;
  if (!isRecord(provided)) return;
  if (typeof (provided as { jit_dynamic_wasm?: unknown }).jit_dynamic_wasm !== "boolean") return;

  currentPlatformFeatures.jit_dynamic_wasm = (provided as { jit_dynamic_wasm: boolean }).jit_dynamic_wasm;
}

function recomputeJitEnabled(): void {
  const hasWasm =
    typeof WebAssembly !== "undefined" && typeof WebAssembly.Module === "function" && typeof WebAssembly.compile === "function";
  jitEnabled = hasWasm && currentPlatformFeatures.jit_dynamic_wasm;
}

recomputeJitEnabled();

function isCspBlockedError(err: unknown): boolean {
  if (err instanceof DOMException) {
    if (err.name === "SecurityError") return true;
  }
  const msg = err instanceof Error ? err.message : String(err);
  return /wasm-unsafe-eval|content security policy|csp/i.test(msg);
}

function cacheKeyForWasmBytes(wasmBytes: ArrayBuffer): string {
  const bytes = new Uint8Array(wasmBytes);
  const hash = fnv1a32Hex(bytes);
  return `${hash}:${bytes.byteLength}`;
}

function cacheGet(key: string): WebAssembly.Module | undefined {
  const existing = moduleCache.get(key);
  if (!existing) return undefined;
  // Refresh insertion order for LRU.
  moduleCache.delete(key);
  moduleCache.set(key, existing);
  return existing;
}

function cacheSet(key: string, module: WebAssembly.Module): void {
  // Refresh insertion order for LRU even when the key already exists.
  moduleCache.delete(key);
  moduleCache.set(key, module);
  if (moduleCache.size <= JIT_CACHE_MAX_ENTRIES) return;
  const oldest = moduleCache.keys().next().value as string | undefined;
  if (oldest !== undefined) moduleCache.delete(oldest);
}

async function waitForCommandRingDataNonBlocking(timeoutMs?: number): Promise<void> {
  // JIT worker must remain responsive to structured `postMessage()` compile requests,
  // so avoid blocking `Atomics.wait()` here.
  await commandRing.waitForDataAsync(timeoutMs);
}

let perfWriter: PerfWriter | null = null;
let perfFrameHeader: Int32Array | null = null;

let pendingJitMs = 0;
let pendingJitFlushTimer: number | null = null;
let pendingJitFlushAttempts = 0;

const PENDING_JIT_FLUSH_INTERVAL_MS = 20;
const PENDING_JIT_FLUSH_MAX_ATTEMPTS = 10;

function stopPendingJitFlushTimer(): void {
  if (pendingJitFlushTimer === null) return;
  clearInterval(pendingJitFlushTimer);
  pendingJitFlushTimer = null;
  pendingJitFlushAttempts = 0;
}

function maybeStartPendingJitFlushTimer(): void {
  if (pendingJitFlushTimer !== null) return;
  pendingJitFlushAttempts = 0;
  const timer = setInterval(() => {
    const writer = perfWriter;
    const header = perfFrameHeader;
    if (!writer || !header) {
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      return;
    }

    const enabled = Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
    if (!enabled) {
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      return;
    }

    const frameId = Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
    if (frameId !== 0) {
      const jitMs = pendingJitMs;
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
      if (jitMs > 0) {
        writer.frameSample(frameId, { durations: { jit_ms: jitMs } });
      }
      return;
    }

    pendingJitFlushAttempts += 1;
    if (pendingJitFlushAttempts >= PENDING_JIT_FLUSH_MAX_ATTEMPTS) {
      // Avoid keeping a hot timer alive indefinitely if RAF is throttled / paused.
      pendingJitMs = 0;
      stopPendingJitFlushTimer();
    }
  }, PENDING_JIT_FLUSH_INTERVAL_MS) as unknown as number;
  (timer as unknown as { unref?: () => void }).unref?.();
  pendingJitFlushTimer = timer;
}

function maybeWritePerfSample(jitMs: number): void {
  const writer = perfWriter;
  const header = perfFrameHeader;
  if (!writer || !header) return;
  const enabled = Atomics.load(header, PERF_FRAME_HEADER_ENABLED_INDEX) !== 0;
  if (!enabled) return;
  const frameId = Atomics.load(header, PERF_FRAME_HEADER_FRAME_ID_INDEX) >>> 0;
  if (frameId === 0) {
    // Perf enabled but no published RAF frame ID yet; stash and retry briefly.
    pendingJitMs += jitMs;
    maybeStartPendingJitFlushTimer();
    return;
  }

  const totalMs = pendingJitMs > 0 ? pendingJitMs + jitMs : jitMs;
  pendingJitMs = 0;
  stopPendingJitFlushTimer();
  writer.frameSample(frameId, { durations: { jit_ms: totalMs } });
}

ctx.onmessage = (ev: MessageEvent<unknown>) => {
  if (isJitCompileRequest(ev.data)) {
    void handleCompile(ev.data);
    return;
  }

  const msg = ev.data as Partial<WorkerInitMessage | ConfigUpdateMessage>;
  if (msg?.kind === "config.update") {
    maybeUpdatePlatformFeatures(msg);
    currentConfig = (msg as ConfigUpdateMessage).config;
    currentConfigVersion = (msg as ConfigUpdateMessage).version;
    recomputeJitEnabled();
    ctx.postMessage({ kind: "config.ack", version: currentConfigVersion } satisfies ConfigAckMessage);
    return;
  }

  const init = msg as Partial<WorkerInitMessage>;
  if (init?.kind !== "init") return;

  perf.spanBegin("worker:boot");
  try {
    perf.spanBegin("wasm:init");
    perf.spanEnd("wasm:init");

    perf.spanBegin("worker:init");
    try {
      role = init.role ?? "jit";
      maybeUpdatePlatformFeatures(init);
      recomputeJitEnabled();
      const segments = {
        control: init.controlSab!,
        guestMemory: init.guestMemory!,
        vgaFramebuffer: init.vgaFramebuffer!,
        scanoutState: init.scanoutState,
        scanoutStateOffsetBytes: init.scanoutStateOffsetBytes ?? 0,
        ioIpc: init.ioIpcSab!,
        sharedFramebuffer: init.sharedFramebuffer!,
        sharedFramebufferOffsetBytes: init.sharedFramebufferOffsetBytes ?? 0,
      };
      status = createSharedMemoryViews(segments).status;
      const regions = ringRegionsForWorker(role);
      commandRing = new RingBuffer(segments.control, regions.command.byteOffset);
      eventRing = new RingBuffer(segments.control, regions.event.byteOffset);
      pushEvent({ kind: "log", level: "info", message: "worker ready" });

      if (init.perfChannel) {
        perfWriter = new PerfWriter(init.perfChannel.buffer, {
          workerKind: init.perfChannel.workerKind,
          runStartEpochMs: init.perfChannel.runStartEpochMs,
        });
        perfFrameHeader = new Int32Array(init.perfChannel.frameHeader);
      }

      setReadyFlag(status, role, true);
      ctx.postMessage({ type: MessageType.READY, role } satisfies ProtocolMessage);
      if (perf.traceEnabled) perf.instant("boot:worker:ready", "p", { role });
    } finally {
      perf.spanEnd("worker:init");
    }
  } finally {
    perf.spanEnd("worker:boot");
  }

  void runLoop();
};

async function handleCompile(req: JitCompileRequest): Promise<void> {
  if (typeof WebAssembly === "undefined" || typeof WebAssembly.Module !== "function" || typeof WebAssembly.compile !== "function") {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "unsupported",
      message: "WebAssembly.compile is unavailable in this environment.",
      durationMs: 0,
    });
    return;
  }

  if (!currentPlatformFeatures.jit_dynamic_wasm) {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "csp_blocked",
      message: "Dynamic WebAssembly compilation is blocked by Content Security Policy (missing 'wasm-unsafe-eval').",
      durationMs: 0,
    });
    return;
  }

  if (!jitEnabled) {
    postJitResponse({
      type: "jit:error",
      id: req.id,
      code: "unsupported",
      message: "JIT compilation is disabled in this worker.",
      durationMs: 0,
    });
    return;
  }

  const key = cacheKeyForWasmBytes(req.wasmBytes);
  const cached = cacheGet(key);
  if (cached) {
    if (perf.traceEnabled) perf.instant("jit:cache_hit", "t", { key });
    postJitResponse({ type: "jit:compiled", id: req.id, module: cached, durationMs: 0, cached: true });
    return;
  }

  const inflight = inflightCompiles.get(key);
  if (inflight) {
    if (perf.traceEnabled) perf.instant("jit:inflight_hit", "t", { key });
    try {
      const module = await inflight.promise;
      const durationMs = (inflight.endMs ?? performance.now()) - inflight.startMs;
      cacheSet(key, module);
      postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs, cached: true });
    } catch (err) {
      const durationMs = (inflight.endMs ?? performance.now()) - inflight.startMs;
      const message = err instanceof Error ? err.message : String(err);
      const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
      if (code === "csp_blocked") {
        currentPlatformFeatures.jit_dynamic_wasm = false;
        recomputeJitEnabled();
      }
      postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
    }
    return;
  }

  const startMs = performance.now();
  perf.spanBegin("jit:compile");
  if (perf.traceEnabled) perf.instant("jit:compile:begin", "t", { key });

  const inflightEntry: InflightCompile = {
    startMs,
    endMs: null,
    // Placeholder; assigned below so the promise can reference the entry.
    promise: null as unknown as Promise<WebAssembly.Module>,
  };
  inflightEntry.promise = WebAssembly.compile(req.wasmBytes).then(
    (module) => {
      inflightEntry.endMs = performance.now();
      return module;
    },
    (err) => {
      inflightEntry.endMs = performance.now();
      throw err;
    },
  );
  inflightCompiles.set(key, inflightEntry);

  try {
    const module = await inflightEntry.promise;
    const durationMs = (inflightEntry.endMs ?? performance.now()) - inflightEntry.startMs;
    maybeWritePerfSample(durationMs);
    if (perf.traceEnabled) perf.instant("jit:compile:end", "t", { key, durationMs });

    cacheSet(key, module);
    postJitResponse({ type: "jit:compiled", id: req.id, module, durationMs });
  } catch (err) {
    const durationMs = (inflightEntry.endMs ?? performance.now()) - inflightEntry.startMs;
    maybeWritePerfSample(durationMs);
    const message = err instanceof Error ? err.message : String(err);
    const code = isCspBlockedError(err) ? "csp_blocked" : "compile_failed";
    if (code === "csp_blocked") {
      currentPlatformFeatures.jit_dynamic_wasm = false;
      recomputeJitEnabled();
    }
    postJitResponse({ type: "jit:error", id: req.id, code, message, durationMs });
  } finally {
    inflightCompiles.delete(key);
    perf.spanEnd("jit:compile");
  }
}

function postJitResponse(msg: JitWorkerResponse): void {
  try {
    ctx.postMessage(msg);
  } catch (err) {
    // Most commonly: `DataCloneError` when `WebAssembly.Module` is not structured-cloneable.
    const message = err instanceof Error ? err.message : String(err);
    const fallback: JitWorkerResponse = {
      type: "jit:error",
      id: msg.id,
      code: "unsupported",
      message: `Failed to post JIT response: ${message}`,
      durationMs: "durationMs" in msg ? msg.durationMs : undefined,
    };
    ctx.postMessage(fallback);
  }
}

async function runLoop(): Promise<void> {
  try {
    while (true) {
      while (true) {
        const bytes = commandRing.tryPop();
        if (!bytes) break;
        let cmd: Command;
        try {
          cmd = decodeCommand(bytes);
        } catch {
          continue;
        }
        if (cmd.kind === "shutdown") {
          Atomics.store(status, StatusIndex.StopRequested, 1);
        }
      }

      if (Atomics.load(status, StatusIndex.StopRequested) === 1) break;
      await waitForCommandRingDataNonBlocking();
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    pushEventBlocking({ kind: "panic", message });
    setReadyFlag(status, role, false);
    ctx.postMessage({ type: MessageType.ERROR, role, message } satisfies ProtocolMessage);
    ctx.close();
    return;
  }

  pushEvent({ kind: "log", level: "info", message: "worker shutdown" });
  setReadyFlag(status, role, false);
  ctx.close();
}

void currentConfig;

function pushEvent(evt: Event): void {
  if (!eventRing) return;
  eventRing.tryPush(encodeEvent(evt));
}

function pushEventBlocking(evt: Event, timeoutMs = 1000): void {
  if (!eventRing) return;
  const payload = encodeEvent(evt);
  if (eventRing.tryPush(payload)) return;
  try {
    eventRing.pushBlocking(payload, timeoutMs);
  } catch {
    // ignore
  }
}
